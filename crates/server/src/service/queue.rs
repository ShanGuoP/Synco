//! 云端批量队列：`queued → running → done/error` 全部存在 results 表里。
//! 调度器只在进程内跑一个"有空位就叫醒下一个"的循环，所以关页面照样跑完；
//! 服务重启后 queued 行由启动时的 reclaim 重新叫起来，正在飞的那次请求接不回来（判掉重来）。

use crate::error::{AppError, Result};
use crate::models::{entity, entity::Image};
use crate::repo::{images as rimg, results as rres};
use crate::service::cloud;
use crate::state::Shared;
use crate::{img::codec, service::imagesvc, service::refs, util};
use serde_json::{json, Map, Value};
use stitch_core::{build_crop_payload, stitch_crop, Build, StitchParams};
use std::sync::atomic::{AtomicUsize, Ordering};
mod spec;
mod files;
pub(crate) use files::PendingFiles;
use spec::{JobKind, JobSpec};

/// 并发上限就是云端设置里那个 concurrency 的刻度，钳在 1–6
pub const CONCURRENCY_MAX: usize = 6;

pub(crate) fn recover_files(ctx: &Shared) -> Result<usize> { files::recover(ctx) }

/// 画稿和参考图已经复制完才发布 queued；配置取提交入口那一刻的值。
pub fn queue_canvas(ctx: &Shared, img: &Image, id: i64, config: cloud::CloudSettings) -> Result<()> {
    let (spec, mask) = spec::capture(ctx, img, &json!({}), config)?;
    let json = serde_json::to_string(&spec).map_err(|e| AppError::fail_detail("srv.queue.specMissing", e))?;
    rres::queue_with_spec(ctx, id, &json, &mask)
}

/// 在飞计数的 RAII 守卫：任务里 panic 走 unwind 时也要把这一格还回去，
/// 普通语句会被 unwind 跳过，所以不能写成 fetch_sub。
/// 计数住在 `Ctx::live` 而不是模块级 static——单测各自造 Ctx 才互不干扰。
struct InFlight(std::sync::Arc<AtomicUsize>);
impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 把「可得许可」对齐到 `want − 在飞数`。
///
/// 不能对总量做增量：`forget_permits` 只削得掉当前**可得**的那部分（tokio 里是
/// `available.saturating_sub(n)`，且实际削掉多少只有返回值知道）。于是 3 张在飞时把并发
/// 从 3 调到 1 一个许可也削不掉，等它们跑完 available 又变回 3——设置写着串行、实际同发 3 张；
/// 之后再调到 6 还会因为重复 add 冲破 CONCURRENCY_MAX。
///
/// 这三步（读在飞 → 读可得 → 补/削）本身没有原子性：入队、worker 收尾、启动恢复三处都会叫
/// `pump`，两个线程同时读到同一个 `available_permits` 就会各补一次，实际并发比设置里的高。
/// 闸门是 tokio 的、`try_acquire` 是原子的，所以只要把这段对齐过程串起来就够了。
fn sync_gate(ctx: &Shared, want: usize) {
    let _serial = ctx.gate_sync.lock().unwrap_or_else(|e| e.into_inner());
    let target = want.saturating_sub(ctx.live.load(Ordering::Acquire));
    let have = ctx.gate.available_permits();
    if target > have {
        ctx.gate.add_permits(target - have);
    } else if have > target {
        ctx.gate.forget_permits(have - target);
    }
}

/// 提交一批云端编辑。能建行的进队列，建不了的逐张给理由——口径和 `/api/run` 一致：
/// 一批里有一张坏了不该把已经排好的撤掉。返回 `(行, 图片)` 的配对，前端好挂轮询。
pub fn enqueue(ctx: &Shared, image_ids: &[i64], settings: &Value, rerun_of: Option<i64>) -> Result<(Vec<(i64, i64)>, Vec<Value>)> {
    let prompt = settings.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
    // 反向涂抹时"一笔没涂"是合法输入（= 整幅重绘），所以不要求遮罩文件存在
    let invert = settings.get("invert").and_then(Value::as_bool).unwrap_or(false);
    let config = cloud::settings(ctx);
    let mut ids = Vec::new();
    let mut skipped = Vec::new();
    for &img_id in image_ids {
        // 理由一律是钥匙：库里那一行坏在哪、为什么没排上，界面按语言自己查
        let mut bad = |e: &AppError| {
            let (code, args) = e.reason();
            let mut o = json!({ "image_id": img_id, "skipped": true, "reason": code });
            if let Some(a) = args { o["reason_args"] = Value::Object(a); }
            skipped.push(o);
        };
        let img = match rimg::by_id(ctx, img_id) {
            Ok(Some(i)) => i,
            Ok(None) => {
                bad(&AppError::bad("srv.submit.imageGone"));
                continue;
            }
            Err(e) => {
                bad(&e);
                continue;
            }
        };
        // 画稿走的是 canvas 那条（整幅生成、不缝合），批量重绘这里要显式挡掉并给可读理由，
        // 而不是让它去报"原图文件已丢失"
        if img.is_sketch() {
            bad(&AppError::bad("srv.submit.isSketch"));
            continue;
        }
        if !util::file_alive(&ctx.data, &Value::String(img.orig_path.clone())) {
            bad(&AppError::bad("srv.submit.origGone"));
            continue;
        }
        // 反向涂抹时"一笔没涂"是合法输入（= 整幅重绘），整图重绘更是**根本不看遮罩**，
        // 所以这两条都不要求遮罩存在或可读；正向局部重绘仍然要先涂
        let full = settings.get("full").and_then(Value::as_bool).unwrap_or(false);
        let mask_ok = invert || full || match img.mask_path.as_deref() {
            Some(m) if util::file_alive(&ctx.data, &Value::String(m.to_string())) => true,
            Some(_) => {
                bad(&AppError::bad("srv.submit.maskGone"));
                false
            }
            None => {
                bad(&AppError::bad("srv.submit.noMask"));
                false
            }
        };
        if !mask_ok {
            continue;
        }
        let (spec, mask) = match spec::capture(ctx, &img, settings, config.clone()) {
            Ok(v) => v,
            Err(e) => { bad(&e); continue; }
        };
        let json = serde_json::to_string(&spec).map_err(|e| AppError::fail_detail("srv.queue.specMissing", e))?;
        let id = rres::insert_cloud_job(ctx, img.id, img.project_id, &prompt, &settings.to_string(), &config.model, rerun_of, &json, &mask)?;
        ids.push((id, img.id));
    }
    Ok((ids, skipped))
}

/// 队列快照：进度面板只看还在排和还在跑的
pub async fn snapshot(ctx: &Shared) -> Result<Value> {
    let rows = rres::list_active(ctx)?;
    let (queued, running) = rres::count_by_status(ctx)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut o = r.as_object().cloned().unwrap_or_else(Map::new);
            o.insert("in_flight".into(), Value::Bool(ctx.job_live(r.get("id").and_then(|v| v.as_i64()).unwrap_or(-1))));
            Value::Object(o)
        })
        .collect();
    Ok(serde_json::json!({
        "items": items, "queued": queued, "running": running,
        "concurrency": cloud::settings(ctx).concurrency,
    }))
}

pub(crate) fn job_grace_ms(ctx: &Shared, id: i64) -> u128 {
    rres::job_timeout(ctx, id).ok().flatten().filter(|t| *t >= cloud::TIMEOUT_MIN)
        .map(|t| t as u128 + cloud::CLOUD_GRACE_MS).unwrap_or_else(|| cloud::grace_ms(ctx))
}

/// 有空位就把 queued 行叫起来。每次入队、每个任务收尾、每次启动都调它。
///
/// 返回擦了类型的 future：worker 收尾要再叫一次 pump，不把这层递归切开，
/// 编译器连 `pump` 自己的 future 类型都定不下来（更谈不上 Send）。
pub fn pump(ctx: &Shared) -> std::pin::Pin<Box<dyn std::future::Future<Output = usize> + Send + '_>> {
    Box::pin(async move {
        let want = cloud::settings(ctx).concurrency.clamp(1, CONCURRENCY_MAX as i64) as usize;
        sync_gate(ctx, want);
        let ids = match rres::list_queued(ctx) {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let mut started = 0usize;
        for id in ids {
            // owned permit：守卫要能跟着 task 一起进 `'static` 的盒子里，借 `&Semaphore` 的那种活不过这层
            let Ok(permit) = ctx.gate.clone().try_acquire_owned() else { break };
            // 登记与改状态都同步做在这一步：任务真正开跑前前端就可能来轮询，
            // 那时它看到的必须已经是"本进程在飞"，否则会被 judge_cloud 判成僵尸
            ctx.job_begin(id);
            match rres::mark_running(ctx, id) {
                // 抢不到 = 另一个泵已经把这一行叫走了，放坑位走人，别去云端跑第二遍
                Ok(false) => { ctx.job_end(id); continue; }
                Err(e) => {
                    eprintln!("  队列起单失败 #{id}：{e}");
                    ctx.job_end(id);
                    continue;
                }
                Ok(true) => {}
            }
            let ctx = ctx.clone();
            ctx.live.fetch_add(1, Ordering::AcqRel);
            let task: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> = Box::pin(async move {
                /* 收尾顺序是要害：`sync_gate` 算的是 `want − 在飞数`，而在飞数由 `_live` 在退出作用域时减。
                   以前它一直挂到整个 task 结束，于是这一次收尾泵看到的是"这一张还在飞"：
                   并发=1 时 target 算成 0，会把刚 `drop(permit)` 还回来的那个坑位又 forget 掉，
                   排在前面的 queued 行就此没人叫——整批停在第二张，直到下次入队或重启才动。 */
                {
                    let _live = InFlight(ctx.live.clone());
                    run_job(&ctx, id).await;
                }
                drop(permit);
                pump(&ctx).await;
            });
            tokio::spawn(task);
            started += 1;
        }
        started
    })
}

async fn run_job(ctx: &Shared, id: i64) {
    let outcome: Result<()> = match rres::by_id(ctx, id) {
        Ok(Some(row)) if row.running() && row.is_cloud() => run_inner(ctx, &row).await,
        // 行已经被删掉或落定了：这一格要还给登记表，否则 ctx.jobs 只涨不落
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    };
    finish(ctx, id, outcome);
}

async fn run_inner(ctx: &Shared, row: &entity::ResultRow) -> Result<()> {
    let mut img = rimg::by_id(ctx, row.image_id)?
        .ok_or_else(|| AppError::fail("srv.queue.imageGone"))?;
    let (spec, mask) = spec::load(ctx, row.id)?;
    img.orig_path = spec.orig_path.clone();
    if matches!(spec.kind, JobKind::Sketch) {
        return run_sketch(ctx, row, img, spec.cloud).await;
    }
    // 整图重绘：把整张原图发过去按提示词生成。它和画布那条同形（不带遮罩、回来不缝合），
    // 差别只在输入是照片、且原图不透明
    if matches!(spec.kind, JobKind::Full) {
        return run_full(ctx, row, img, spec, mask).await;
    }
    let prompt = row.prompt.clone();
    let config = spec.cloud.clone();
    let pre = {
        let ctx = ctx.clone();
        let img = img.clone();
        tokio::task::spawn_blocking(move || spec::prepare_job(&ctx, &img, &spec, mask)).await
    }
    .map_err(|e| AppError::fail_detail("srv.queue.cropPanic", e))??;
    let bytes = cloud::edit_with(
        ctx,
        &config,
        cloud::Edit {
            image: Some(pre.crop.clone()),
            mask: if pre.no_mask { None } else { Some(pre.mask.clone()) },
            // 裁切重绘那一路的"参考"就是这一枪自己的裁切区与遮罩，不再外挂参考图
            refs: Vec::new(),
            prompt: &prompt,
            size: Some(&pre.size),
        },
    )
    .await?;
    let ctx2 = ctx.clone();
    let id = row.id;
    tokio::task::spawn_blocking(move || compose(&ctx2, id, &img, &pre, &bytes))
        .await
        .map_err(|e| AppError::fail_detail("srv.queue.stitchPanic", e))?
}

fn finish(ctx: &Shared, id: i64, r: Result<()>) {
    if let Err(e) = r {
        ctx.mark_error_of(id, &e);
    }
    ctx.job_end(id);
}

struct Prepared {
    crop: Vec<u8>,
    mask: Vec<u8>,
    /// 反向涂抹且一笔没涂 = 整幅重绘，这时不带 mask 这个 part
    no_mask: bool,
    size: String,
    base: stitch_core::Rgba,
    alpha: stitch_core::Alpha,
    crop_box: stitch_core::Box2,
    params: StitchParams,
    /// 这一枪实际用的遮罩文件字节（原样，供重算时复制落盘）。没有遮罩时是空的
    mask_png: Vec<u8>,
    /// 重算无损所需的最小快照：缝合参数 + 几何与调整参数 + 规则版本
    snap: Value,
}

/// 由"这一枪要用的那两张图"折出裁切载荷。`prepare`（提交）与 `lossless_bytes`（导出重算）
/// 必须共用这一段——重算要逐位等于当年那一张，就不能另写一份几何。
fn build_prepared(photo: stitch_core::Rgba, mask_alpha: stitch_core::Alpha, params: &StitchParams) -> Result<Prepared> {
    match build_crop_payload(&photo, &mask_alpha, params) {
        Build::Payload(p) => Ok(Prepared {
            // 云端按 fit 后的档位收图，所以 size 由裁切区自己说了算
            crop: codec::encode_png(&p.image),
            mask: codec::encode_png(&p.mask),
            no_mask: p.no_mask,
            size: format!("{}x{}", p.image.w, p.image.h),
            base: photo,
            alpha: mask_alpha,
            crop_box: p.crop,
            params: *params,
            mask_png: Vec::new(),
            snap: Value::Null,
        }),
        Build::NoInk => Err(AppError::bad("srv.queue.noInk")),
        Build::Unfit => Err(AppError::bad("srv.queue.cropUnfit")),
    }
}

/// 读原图 + 蒙版，折出裁切区。全程在原图分辨率上做，浏览器只需要交出 proxy 分辨率的涂抹层。
///
/// 裁切目标长边取**这一行提交时的快照**（`settings.edge`），不是跑到的那一刻的全局设置：
/// 精修页的尺寸胶囊以前写进 settings 却没人读，等于一个骗人的旋钮。
/// `settings.invert` 同理按行生效（反向涂抹只开云端这条路）。
#[cfg(test)]
fn prepare(ctx: &Shared, img: &Image, settings: &Value) -> Result<Prepared> {
    let (spec, mask) = spec::capture(ctx, img, settings, cloud::settings(ctx))?;
    spec::prepare_job(ctx, img, &spec, mask)
}

/// 无损重算的全部前提：缝合参数、几何与调整参数、缝合代码的规则版本。
/// 规则版本对不上就不许重算——那段代码改过取值条件之后，"重算"出来的已经不是当年那一张。
fn stitch_snapshot(params: &StitchParams, ops: &photoedit_core::EditOps) -> Value {
    serde_json::json!({
        "v": 1,
        "rules": stitch_core::RULES_V,
        "stitch": {
            "expand": params.expand, "context": params.context, "crop_edge": params.crop_edge,
            "feather": params.feather, "levels": params.levels, "invert": params.invert,
        },
        "ops": serde_json::from_str::<Value>(&ops.to_json()).unwrap_or(Value::Null),
    })
}

/// 把这一枪的前提按快照原样摆回来。少一个字段就当不能重算，不猜默认值——
/// 猜出来的那一张看着也正常，其实与用户当年看到的那张不是同一个东西
fn prepared_from_snapshot(ctx: &Shared, img: &Image, snap: &Value, mask_png: Option<&[u8]>) -> Result<Prepared> {
    let sp = snap.get("stitch").and_then(Value::as_object).ok_or_else(|| AppError::bad("srv.lossless.noSnap"))?;
    let num = |k: &str| sp.get(k).and_then(Value::as_f64).ok_or_else(|| AppError::bad("srv.lossless.noSnap"));
    let params = StitchParams {
        expand: num("expand")?,
        context: num("context")?,
        crop_edge: num("crop_edge")? as u32,
        feather: num("feather")?,
        levels: num("levels")? as usize,
        invert: sp.get("invert").and_then(Value::as_bool).unwrap_or(false),
    };
    let ops: photoedit_core::EditOps = serde_json::from_value(snap.get("ops").cloned().unwrap_or(Value::Null))
        .map_err(|_| AppError::bad("srv.lossless.noSnap"))?;
    let photo = if let Some(inputs) = snap.get("adjust_inputs") {
        crate::service::adjust::photo_from_snapshot(ctx, img, &ops, inputs, mask_png)?
    } else {
        // 旧结果的重算格式没有辅助输入快照，保持旧导出行为。
        crate::service::adjust::photo_with(ctx, img, &ops)?
    };
    // 与 prepare 同一套分岔：反向涂抹且一笔没保 = 没有遮罩文件也是合法输入
    let mask_alpha = match mask_png {
        Some(b) => photoedit_core::geometry::apply_alpha(&codec::decode(b)?.alpha(), &ops.geometry),
        None if params.invert => stitch_core::Alpha::new(photo.w, photo.h),
        None => return Err(AppError::bad("srv.lossless.noSnap")),
    };
    build_prepared(photo, mask_alpha, &params)
}

/// 云端回来的图贴回原图并落盘：成图 + 320 缩略 + **重算无损用的那两份**。
///
/// 无损那一张不再整张存盘（24MP 一张 PNG ≈ 21 MB，他库里 37 张就 785 MB）：留下的是模型回来的
/// 那串字节**原样**（窗口大小、未经二次编码，实测均值 1.7 MB）与这一枪实际用的那份遮罩，
/// 再加上参数快照 `pre.snap`。导出、下载、另存都从这三样重算，见 `lossless_bytes`。
/// 少任何一件就给不出无损——那时候界面必须说清给的是哪一档，不许默默降质。
///
/// 只有"裁切—缝合"这条路降档：画布与整图重绘那两条回来的就是全图、也没有可重算的几何，
/// 它们继续存 PNG（本来就是唯一那一份，不是副本）。
fn compose(ctx: &Shared, id: i64, img: &Image, pre: &Prepared, bytes: &[u8]) -> Result<()> {
    if ctx.job_cancelled(id) {
        return Err(AppError::fail("srv.queue.byHand"));
    }
    let model = codec::decode(bytes)?;
    let final_img = stitch_crop(&pre.base, &pre.alpha, pre.crop_box, &model, &pre.params);
    let dir = ctx.data.join("projects").join(img.project_id.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| AppError::fail_detail("srv.common.mkdirFail", e))?;
    let ts = util::now_ms();
    let mut pending = files::PendingFiles::new(ctx.data.clone());
    let rel = |kind: &str, ext: &str| util::rel_path(&["projects".into(), img.project_id.to_string(), format!("r{id}_{ts}_{kind}.{ext}")]);
    let fin = rel("final", "jpg");
    std::fs::write(pending.track(&fin), codec::encode_jpeg(&final_img, imagesvc::FINAL_Q)).map_err(|e| AppError::fail_detail("srv.queue.finalWrite", e))?;
    // 这两份必须是原样字节：重编码一次就把"当年那一张"换成"这一次的又一张"
    let raw = rel("raw", "png");
    std::fs::write(pending.track(&raw), bytes).map_err(|e| AppError::fail_detail("srv.queue.rawWrite", e))?;
    let msnap = if pre.mask_png.is_empty() {
        None
    } else {
        let p = rel("msnap", "png");
        std::fs::write(pending.track(&p), &pre.mask_png).map_err(|e| AppError::fail_detail("srv.queue.msnapWrite", e))?;
        Some(p)
    };
    // 缩略图失败不影响这一张成图：历史列回落到 final_url 就行
    let thumb = imagesvc::write_result_thumb(ctx, id, img.project_id, &fin);
    if let Some(t) = &thumb { pending.track(t); }
    if rres::complete(ctx, id, &fin, thumb.as_deref(), &raw, msnap.as_deref(), &pre.snap.to_string())? {
        pending.commit();
    }
    Ok(())
}

/// 交出去的那一张成图：字节、扩展名，以及**它是不是无损的**。
/// 三条出口，调用方必须让用户知道自己拿到的是哪一条：
/// - 重算（`lossless = true`）：按快照把当年那一次缝合重跑一遍——同样的入参、同一段纯函数，逐位相同；
/// - 原件（`lossless = true`）：改造以前的行本来就存着无损 PNG，直接给那一份；
/// - 次档（`lossless = false`，带 `why`）：重算的前提缺了一件半件（原图被手动删过、窗口原图没留、
///   缝合代码改过规则），只能给成图那一档 q95。默默降质是最坏的结局：用户会以为手里那张就是原件。
pub struct OutImage {
    pub bytes: Vec<u8>,
    pub ext: &'static str,
    pub lossless: bool,
    pub why: Option<&'static str>,
}

/// 取这一行的无损成图。成图那一档现在存的是 q95（24MP 一张 PNG ≈ 21 MB，历史堆不起），
/// 原件由「窗口原图 + 当时那份遮罩 + 参数快照」重算出来——所以前提一旦不在，就必须说清给的是次档。
pub fn lossless_bytes(ctx: &Shared, row: &entity::ResultRow) -> Result<OutImage> {
    /// 次档：把成图文件本身交出去，附上"为什么给不出无损"
    fn degraded(ctx: &Shared, row: &entity::ResultRow, why: &'static str) -> Result<OutImage> {
        let p = row.final_path.as_deref().and_then(|x| util::data_file(&ctx.data, x)).filter(|p| p.is_file());
        match p {
            Some(p) => Ok(OutImage { bytes: std::fs::read(&p).map_err(|e| AppError::fail_detail("srv.lossless.readFail", e))?, ext: "jpg", lossless: false, why: Some(why) }),
            // 连次档都没有：这一行本来就没成图，上层按"没有可导出的成图"处理
            None => Err(AppError::bad("srv.submit.noCopyable")),
        }
    }
    // 老行没有 raw_path：它那份 final 就是无损原件，不用重算也不许重算
    let snap = row.snap();
    if row.raw_path.is_none() || snap.is_null() {
        return match row.final_path.as_deref().and_then(|p| util::data_file(&ctx.data, p)) {
            Some(p) if p.extension().map(|e| e.to_ascii_lowercase()) == Some("png".into()) => {
                Ok(OutImage { bytes: std::fs::read(&p).map_err(|e| AppError::fail_detail("srv.lossless.readFail", e))?, ext: "png", lossless: true, why: None })
            }
            _ => degraded(ctx, row, "srv.lossless.noSnap"),
        };
    }
    if snap.get("rules").and_then(Value::as_u64).unwrap_or(0) as u32 != stitch_core::RULES_V {
        return degraded(ctx, row, "srv.lossless.rulesChanged");
    }
    let raw = match row.raw_path.as_deref().and_then(|p| util::data_file(&ctx.data, p)).filter(|p| p.is_file()) {
        Some(p) => p,
        None => return degraded(ctx, row, "srv.lossless.rawGone"),
    };
    let img = match rimg::by_id(ctx, row.image_id)? {
        Some(i) => i,
        None => return degraded(ctx, row, "srv.lossless.imageGone"),
    };
    if !util::file_alive(&ctx.data, &Value::String(img.orig_path.clone())) {
        return degraded(ctx, row, "srv.lossless.origGone");
    }
    let mask = row.mask_snap_path.as_deref().and_then(|p| util::data_file(&ctx.data, p)).filter(|p| p.is_file());
    let mask_png = match mask.as_ref().map(|p| std::fs::read(p)) {
        Some(Ok(b)) => Some(b),
        Some(_) => return degraded(ctx, row, "srv.lossless.maskGone"),
        None => None,
    };
    let pre = prepared_from_snapshot(ctx, &img, &snap, mask_png.as_deref())
        .map_err(|_| AppError::bad("srv.lossless.noSnap"))?;
    let model = codec::decode(&std::fs::read(&raw).map_err(|e| AppError::fail_detail("srv.lossless.readFail", e))?)?;
    let out = stitch_crop(&pre.base, &pre.alpha, pre.crop_box, &model, &pre.params);
    Ok(OutImage { bytes: codec::encode_png(&out), ext: "png", lossless: true, why: None })
}

/// 画布的一次生成：画稿拍到白底当输入图，**不带遮罩**，回来的图直接是成图。
///
/// 这里没有裁切与缝合——画布不存在"蒙版外要保持"这件事。状态机、并发闸、
/// 重启续跑都照用队列这一套，所以只有"准备载荷"和"落盘"两段是画布自己的。
async fn run_sketch(ctx: &Shared, row: &entity::ResultRow, img: Image, config: cloud::CloudSettings) -> Result<()> {
    let prompt = row.prompt.clone();
    /* 发出去的是**这一行提交那一刻**的那份快照，不是此刻的画稿：排着队的时候接着画两笔，
       画稿会被自动保存覆写，那时图对应 t1 而行上的 sketch_url 还是 t0，
       对比层与「取回这一版画稿」就都拿 t0 说话。没有快照就明确失败，不能回落到后来改过的画稿。 */
    let src = row.sketch_path.clone().ok_or_else(|| AppError::bad("srv.queue.specMissing"))?;
    let (png, size, inked) = {
        let ctx2 = ctx.clone();
        let src2 = src.clone();
        tokio::task::spawn_blocking(move || prepare_sketch(&ctx2, &src2))
            .await
            .map_err(|e| AppError::fail_detail("srv.queue.canvasPanic", e))??
    };
    // 参考图读的是**这一行**的快照（不是此刻的槽位），与画稿快照同一条纪律。
    // 一次最多四张全分辨率 PNG，读盘这段也不该按在 worker 上
    let refs_bytes = {
        let ctx2 = ctx.clone();
        let row2 = row.clone();
        tokio::task::spawn_blocking(move || refs::row_payloads(&ctx2, &row2))
            .await
            .map_err(|e| AppError::fail_detail("srv.queue.refReadPanic", e))??
    };
    // 一笔没涂又挂了参考图：那张全白的纸不必发出去，这一枪就是"按参考图与提示词生成"。
    // 没参考图时维持老行为（发空白画稿），免得动了既有语义。
    let image = if inked || refs_bytes.is_empty() { Some(png) } else { None };
    let bytes = cloud::edit_with(
        ctx,
        &config,
        cloud::Edit { image, mask: None, refs: refs_bytes, prompt: &prompt, size: Some(&size) },
    )
    .await?;
    let id = row.id;
    let ctx2 = ctx.clone();
    tokio::task::spawn_blocking(move || compose_sketch(&ctx2, id, &img, &bytes))
        .await
        .map_err(|e| AppError::fail_detail("srv.queue.diskPanic", e))?
}

/// 返回 `(发出去的 PNG, 尺寸串, 有没有笔迹)`。第三项决定"空白画稿 + 参考图"那一枪要不要带主图。
fn prepare_sketch(ctx: &Shared, rel: &str) -> Result<(Vec<u8>, String, bool)> {
    let raw = std::fs::read(util::data_file(&ctx.data, rel).ok_or_else(|| AppError::bad_args("srv.queue.sketchUnread", json!({ "path": rel.to_string() })))?)
        .map_err(|e| AppError::detail("srv.queue.sketchRead", e))?;
    let rgba = codec::decode(&raw)?;
    sketch_fit(rgba.w, rgba.h)?;
    let inked = rgba.px.iter().skip(3).step_by(4).any(|&a| a != 0);
    let flat = codec::flatten(&rgba, [255, 255, 255]);
    Ok((codec::encode_png(&flat), format!("{}x{}", rgba.w, rgba.h), inked))
}

/// 云端接口对图片尺寸的硬约束（长边 ≤3840、比例 ≤3:1、像素 65.5 万~829 万）。
/// 建画布时按这一条拒过一次，提交时再判一次：库里可能躺着别的入口写进来的行。
pub fn sketch_fit(w: usize, h: usize) -> Result<()> {
    use stitch_core::geom::{EDGE_MAX, PX_MAX, PX_MIN, RATIO_MAX};
    if w == 0 || h == 0 {
        return Err(AppError::bad("srv.queue.canvasZero"));
    }
    let long = w.max(h);
    let ratio = long as f64 / w.min(h) as f64;
    let px = w as u64 * h as u64;
    if long > EDGE_MAX as usize || ratio > RATIO_MAX || !(PX_MIN..=PX_MAX).contains(&px) {
        return Err(AppError::bad_args(
            "srv.queue.canvasOut",
            json!({ "w": w, "h": h, "edge": EDGE_MAX, "ratio": RATIO_MAX as i64, "min": PX_MIN, "max": PX_MAX }),
        ));
    }
    Ok(())
}

/// 整图重绘的一次生成：整张原图折进云端几何框、不带遮罩，回来的图就是成图。
async fn run_full(ctx: &Shared, row: &entity::ResultRow, img: Image, spec: JobSpec, mask: Vec<u8>) -> Result<()> {
    let prompt = row.prompt.clone();
    let config = spec.cloud.clone();
    let (buf, size) = {
        let ctx2 = ctx.clone();
        let img2 = img.clone();
        tokio::task::spawn_blocking(move || prepare_full(&ctx2, &img2, &spec, &mask))
            .await
            .map_err(|e| AppError::fail_detail("srv.queue.wholePanic", e))??
    };
    let bytes = cloud::edit_with(
        ctx,
        &config,
        cloud::Edit { image: Some(buf), mask: None, refs: Vec::new(), prompt: &prompt, size: Some(&size) },
    )
    .await?;
    let id = row.id;
    let ctx2 = ctx.clone();
    tokio::task::spawn_blocking(move || compose_whole(&ctx2, id, &img, &bytes, "f"))
        .await
        .map_err(|e| AppError::fail_detail("srv.queue.diskPanic", e))?
}

/// 整图重绘的目标尺寸判定：`(宽, 高, 是否原样发)`。
/// 纯函数，好让"折不折得进""要不要重编码"这两条判据能被单测钉住（不碰磁盘与像素缓冲）。
pub fn full_target(w: usize, h: usize, edge: u32) -> Result<(u32, u32, bool)> {
    let fit = stitch_core::geom::fit_size(w as u32, h as u32, edge);
    if fit.unfit || fit.w == 0 || fit.h == 0 {
        return Err(AppError::bad_args(
            "srv.queue.wholeFold",
            json!({
                "w": w,
                "h": h,
                "edge": stitch_core::geom::EDGE_MAX,
                "ratio": stitch_core::geom::RATIO_MAX as i64,
                "min": stitch_core::geom::PX_MIN,
                "max": stitch_core::geom::PX_MAX,
            }),
        ));
    }
    Ok((fit.w, fit.h, fit.w as usize == w && fit.h as usize == h))
}

/// PNG 的八字节签名。`cloud::edit` 把 part 硬标成 `image.png` + `image/png`，
/// 而导入落盘的是相机原图（JPEG/WEBP 居多）——声明与实际编码不符会被对面整批拒收，
/// 所以"原样发"必须先确认手里的字节真的是 PNG。
const PNG_SIG: &[u8; 8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// 原样发成立的条件（不含尺寸档位那条，那条由 `full_target` 判）。纯函数，好让单测钉住。
pub fn raw_sendable(raw: &[u8]) -> bool {
    raw.starts_with(PNG_SIG) && raw.len() <= cloud::SEND_MAX_BYTES
}

/// 折整张原图：已经在框里且**本来就是 PNG** 才原样发（JPEG 转一次 PNG 再转回来是平白多一道损失），
/// 其余一律按这一行选的长边重采样到 `full_target` 给的合法尺寸并重编码。
fn prepare_full(ctx: &Shared, img: &Image, spec: &JobSpec, mask: &[u8]) -> Result<(Vec<u8>, String)> {
    let edge = spec.snapshot["stitch"]["crop_edge"].as_u64().ok_or_else(|| AppError::bad("srv.queue.specMissing"))? as u32;
    // 整图重绘同样吃"调整后"的图；调整过就不能再走"原样透传文件字节"那条捷径，
    // 透传的意义是不为一张没动过的图多付一次解码重编码
    let ops: photoedit_core::EditOps = serde_json::from_value(spec.snapshot["ops"].clone()).map_err(|_| AppError::bad("srv.queue.specMissing"))?;
    let dec = crate::service::adjust::photo_from_snapshot(ctx, img, &ops, &spec.snapshot["adjust_inputs"], if mask.is_empty() { None } else { Some(mask) })?;
    let (fw, fh, passthrough) = full_target(dec.w, dec.h, edge)?;
    if passthrough && ops.is_identity() {
        let raw = std::fs::read(util::data_file(&ctx.data, &img.orig_path).ok_or_else(|| AppError::bad("srv.queue.origUnread"))?)
            .map_err(|e| AppError::fail_detail("srv.image.origRead", e))?;
        if raw_sendable(&raw) {
            return Ok((raw, format!("{}x{}", dec.w, dec.h)));
        }
    }
    let scaled = stitch_core::resize_rgba(&dec, fw as usize, fh as usize);
    Ok((codec::encode_png(&scaled), format!("{fw}x{fh}")))
}

fn compose_sketch(ctx: &Shared, id: i64, img: &Image, bytes: &[u8]) -> Result<()> {
    compose_whole(ctx, id, img, bytes, "c")
}

/// 整幅出图的那一路共用的落盘：先解一次确认不是坏图，再写成图 + 补缩略档。
/// 画布（前缀 `c`）与照片整图重绘（前缀 `f`）都走这里——它们都没有缝合这一步。
fn compose_whole(ctx: &Shared, id: i64, img: &Image, bytes: &[u8], prefix: &str) -> Result<()> {
    if ctx.job_cancelled(id) {
        return Err(AppError::fail("srv.queue.byHand"));
    }
    // 先解一次再落盘：坏图进了库就会变成"卡片在、点开是空的"
    let decoded = codec::decode(bytes)?;
    if decoded.w == 0 || decoded.h == 0 {
        return Err(AppError::fail("srv.queue.cloudEmpty"));
    }
    let rel = util::rel_path(&[
        "projects".into(),
        img.project_id.to_string(),
        format!("{prefix}{id}_{}_final.png", util::now_ms()),
    ]);
    let mut pending = files::PendingFiles::new(ctx.data.clone());
    imagesvc::write_bytes(&pending.track(&rel), bytes)?;
    let thumb = imagesvc::write_result_thumb(ctx, id, img.project_id, &rel);
    if let Some(t) = &thumb { pending.track(t); }
    if rres::set_done(ctx, id, &rel, thumb.as_deref())? { pending.commit(); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 导出重算出来的那一张，必须与当年落盘那一张**逐位相同**——不然"无损导出"是句空话：
    /// 用户拿到一张看着一样、像素不一样的图，而文件名后缀还写着 .png。
    /// 顺手钉住另外两条：成图落盘改 q95 后原件靠 raw + 遮罩快照 + 参数快照三样拼回来；
    /// 遮罩在提交之后被人改过笔迹，也不得把重算的那一张带跑。
    #[test]
    fn 重算无损与当时那一张逐位相同() {
        let dir = std::env::temp_dir().join(format!("synco-lossless-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
        let pid = crate::repo::projects::create(&ctx, "无损").unwrap();
        let proj = dir.join("projects").join(pid.to_string());
        std::fs::create_dir_all(&proj).unwrap();
        let mut orig = stitch_core::Rgba::new(400, 300);
        for y in 0..300usize {
            for x in 0..400usize {
                let o = (y * 400 + x) * 4;
                orig.px[o] = x as u8;
                orig.px[o + 1] = y as u8;
                orig.px[o + 2] = 128;
                orig.px[o + 3] = 255;
            }
        }
        std::fs::write(proj.join("a.png"), codec::encode_png(&orig)).unwrap();
        let mut mask = stitch_core::Rgba::new(400, 300);
        for y in 100..180usize {
            for x in 120..260usize {
                mask.px[(y * 400 + x) * 4 + 3] = 255;
            }
        }
        std::fs::write(proj.join("a_mask.png"), codec::encode_png(&mask)).unwrap();
        let iid = crate::repo::images::insert(&ctx, pid, "a.png", &format!("projects/{pid}/a.png"), 400, 300).unwrap();
        crate::repo::images::set_mask(&ctx, iid, Some(&format!("projects/{pid}/a_mask.png"))).unwrap();
        let img = rimg::by_id(&ctx, iid).unwrap().unwrap();
        let settings = serde_json::json!({ "edge": 512, "invert": false });
        let pre = prepare(&ctx, &img, &settings).unwrap();
        // 模型回来的那张：拿发出去的裁切图反一次色，字节确定
        let mut model = codec::decode(&pre.crop).unwrap();
        for p in model.px.chunks_exact_mut(4) {
            let (a, b, c) = (p[0], p[1], p[2]);
            p[0] = 255 - a;
            p[1] = 255 - b;
            p[2] = 255 - c;
        }
        let model_png = codec::encode_png(&model);
        let want = codec::encode_png(&stitch_crop(&pre.base, &pre.alpha, pre.crop_box, &model, &pre.params));
        let rid = rres::insert_cloud(&ctx, iid, pid, "p", &settings.to_string(), "m", None).unwrap();
        compose(&ctx, rid, &img, &pre, &model_png).unwrap();
        let row = rres::by_id(&ctx, rid).unwrap().unwrap();
        assert!(row.raw_path.as_deref().unwrap_or("").ends_with("_raw.png"), "窗口原图没落盘");
        assert!(row.mask_snap_path.is_some(), "遮罩快照没落盘");
        assert!(row.final_path.as_deref().unwrap_or("").ends_with(".jpg"), "成图那一份该是 q95");
        let got = lossless_bytes(&ctx, &row).unwrap();
        assert!(got.lossless, "重算这一条该报无损：{}", got.why.unwrap_or(""));
        assert_eq!(got.ext, "png");
        assert_eq!(got.bytes, want, "重算的那一张与当时落盘的逐位不同");

        // 提交之后又涂了一笔：重算必须还按当时那份快照走
        let mut later = mask.clone();
        for y in 0..40usize {
            for x in 0..40usize {
                later.px[(y * 400 + x) * 4 + 3] = 255;
            }
        }
        std::fs::write(proj.join("a_mask.png"), codec::encode_png(&later)).unwrap();
        let again = lossless_bytes(&ctx, &row).unwrap();
        assert_eq!(again.bytes, want, "遮罩漂移把重算的那一张带跑了");

        // 原图被手动删过：给不出无损，必须明说为什么，并把 q95 那一份照交
        std::fs::remove_file(proj.join("a.png")).unwrap();
        let deg = lossless_bytes(&ctx, &row).unwrap();
        assert!(!deg.lossless);
        assert_eq!(deg.ext, "jpg");
        assert_eq!(deg.why, Some("srv.lossless.origGone"));
        drop(ctx);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 并发从 3 下调到 1 时若还有 3 张在飞：增量式调整一个许可也削不掉，
    /// 等它们跑完 available 会回到 3（设置写串行、实际同发 3 张），再往上调还能冲破上限。
    /// 闸门与在飞计数都挂在各自新建的 `Ctx` 上——它们曾是模块级 static，
    /// 于是这条测试与任何碰闸门的测试都有顺序依赖，跑的顺序一变结论就变。
    #[test]
    fn 在飞时下调并发不会漏发也不冲破上限() {
        let dir = std::env::temp_dir().join(format!("synco-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
        sync_gate(&ctx, 3);
        assert_eq!(ctx.gate.available_permits(), 3);
        let held: Vec<_> = (0..3).map(|_| ctx.gate.clone().try_acquire_owned().unwrap()).collect();
        ctx.live.fetch_add(3, Ordering::AcqRel);
        sync_gate(&ctx, 1);
        assert_eq!(ctx.gate.available_permits(), 0, "三张在飞时不该还有空位");
        drop(held);
        ctx.live.fetch_sub(3, Ordering::AcqRel);
        sync_gate(&ctx, 1);
        assert_eq!(ctx.gate.available_permits(), 1, "释放之后只留一个坑");
        sync_gate(&ctx, 6);
        assert_eq!(ctx.gate.available_permits(), 6, "上调要能一路开到上限");
        sync_gate(&ctx, 1);
        assert_eq!(ctx.gate.available_permits(), 1);
        drop(ctx);   // 连接还开着的时候 Windows 删不掉 app.db
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 建画布时按同一口径拒过，提交时再判一次：库里可能躺着别的入口写进来的行
    #[test]
    fn 画布尺寸按云端硬约束判() {        assert!(sketch_fit(1024, 1024).is_ok());
        assert!(sketch_fit(2048, 1536).is_ok(), "3:2 的 3MP 该过");
        assert!(sketch_fit(4000, 6000).is_err(), "长边与像素都顶破了");
        assert!(sketch_fit(800, 200).is_err(), "4:1 超比例");
        assert!(sketch_fit(700, 700).is_err(), "49 万像素低于下限");
        assert!(sketch_fit(0, 0).is_err());
        assert!(sketch_fit(1, 4000).is_err());
    }

    /// 原样发的两条判据：签名不对（相机直出的 JPEG/WEBP）或体积顶穿中转上限，都必须回落重编码
    #[test]
    fn 原样发只认真正的小png() {
        let png = |n: usize| [&PNG_SIG[..], &vec![7u8; n][..]].concat();
        assert!(raw_sendable(&png(1024)));
        assert!(!raw_sendable(&[0xff, 0xd8, 0xff, 0xe0, 0, 0][..]), "JPEG 不能被声明成 PNG 发出去");
        assert!(!raw_sendable(&[]), "空字节不是 PNG");
        assert!(!raw_sendable(&png(cloud::SEND_MAX_BYTES + 1)), "超上限就不该原样发");
        assert!(raw_sendable(&png(cloud::SEND_MAX_BYTES - PNG_SIG.len())), "正好卡在上限内的该放行");
    }

    /// 整图重绘的目标尺寸：判据是纯函数，所以能一条不打折地钉住
    #[test]
    fn 整图重绘的目标尺寸永远落在云端框里() {
        use stitch_core::geom::{EDGE_MAX, PX_MAX, PX_MIN, RATIO_MAX};
        let cases = [
            (1200usize, 800usize, 2048u32),
            (1200, 800, 1024),
            (4000, 1400, 4096),
            (3368, 6000, 1024),
            (3840, 3840, 3840),
            (900, 900, 512),
        ];
        for (w, h, edge) in cases {
            let (fw, fh, same) = super::full_target(w, h, edge).unwrap_or_else(|e| panic!("{w}×{h}@{edge}：{}", e.text()));
            let long = fw.max(fh) as usize;
            let ratio = long as f64 / fw.min(fh) as f64;
            let px = fw as u64 * fh as u64;
            assert!(long <= EDGE_MAX as usize, "{w}×{h}@{edge} → {fw}×{fh} 长边顶破");
            assert!(ratio <= RATIO_MAX + 0.02, "{w}×{h}@{edge} → {fw}×{fh} 比例 {ratio}");
            assert!((PX_MIN..=PX_MAX).contains(&px), "{w}×{h}@{edge} → {fw}×{fh} 像素 {px} 出界");
            assert_eq!(same, (fw as usize, fh as usize) == (w, h), "{w}×{h}@{edge} 的\"原样发\"判据要一致");
        }
    }

    #[test]
    fn 已经在框里的原图不重编码() {
        // 96 万像素、比例 1.5：本来就在框内，edge 给到 2048 时应当一个像素都不动地原样发
        let (w, h, passthrough) = super::full_target(1200, 800, 2048).unwrap();
        assert_eq!((w, h), (1200, 800));
        assert!(passthrough, "已经在框里就该原样发");
        // 同一张图把目标长边压到 1024：要折，且不再算"原样"
        let (w2, h2, same2) = super::full_target(1200, 800, 1024).unwrap();
        assert_eq!(w2, 1024, "长边该按这一行选的档位折");
        assert!(h2 > 0 && !same2);
    }

    /// 比例不可能合法时给可读理由，而不是硬发一张会被对面拒的图
    #[test]
    fn 折不进框的整图给可读理由() {
        let e = super::full_target(4000, 400, 2048).unwrap_err().text();
        assert!(e.contains("4000×400"), "{e}");
        assert!(e.contains("折不进"), "{e}");
    }
}
