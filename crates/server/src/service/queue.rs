//! 云端批量队列：`queued → running → done/error` 全部存在 results 表里。
//! 调度器只在进程内跑一个"有空位就叫醒下一个"的循环，所以关页面照样跑完；
//! 服务重启后 queued 行由启动时的 reclaim 重新叫起来，正在飞的那次请求接不回来（判掉重来）。

use crate::error::Result;
use crate::models::{entity, entity::Image};
use crate::repo::{images as rimg, results as rres};
use crate::service::cloud;
use crate::state::Shared;
use crate::{img::codec, service::imagesvc, service::refs, util};
use serde_json::{Map, Value};
use stitch_core::{build_crop_payload, stitch_crop, Build, StitchParams};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use tokio::sync::Semaphore;

/// 并发上限就是云端设置里那个 concurrency 的刻度，钳在 1–6
pub const CONCURRENCY_MAX: usize = 6;

static GATE: OnceLock<Semaphore> = OnceLock::new();
/// 已经发出去的坑位数，也就是本进程在飞的云端任务数
static LIVE: AtomicUsize = AtomicUsize::new(0);

fn gate() -> &'static Semaphore {
    GATE.get_or_init(|| Semaphore::new(0))
}

/// 在飞计数的 RAII 守卫：任务里 panic 走 unwind 时也要把这一格还回去，
/// 普通语句会被 unwind 跳过，所以不能写成 fetch_sub。
struct InFlight;
impl Drop for InFlight {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::AcqRel);
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
static GATE_SYNC: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn sync_gate(want: usize) {
    let _serial = GATE_SYNC.lock().unwrap_or_else(|e| e.into_inner());
    let target = want.saturating_sub(LIVE.load(Ordering::Acquire));
    let have = gate().available_permits();
    if target > have {
        gate().add_permits(target - have);
    } else if have > target {
        gate().forget_permits(have - target);
    }
}

/// 提交一批云端编辑。能建行的进队列，建不了的逐张给理由——口径和 `/api/run` 一致：
/// 一批里有一张坏了不该把已经排好的撤掉。返回 `(行, 图片)` 的配对，前端好挂轮询。
pub fn enqueue(ctx: &Shared, image_ids: &[i64], settings: &Value, rerun_of: Option<i64>) -> Result<(Vec<(i64, i64)>, Vec<Value>)> {
    let prompt = settings.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
    // 反向涂抹时"一笔没涂"是合法输入（= 整幅重绘），所以不要求遮罩文件存在
    let invert = settings.get("invert").and_then(Value::as_bool).unwrap_or(false);
    let model = cloud::settings(ctx).model;
    let mut ids = Vec::new();
    let mut skipped = Vec::new();
    for &img_id in image_ids {
        let mut bad = |reason: String| skipped.push(serde_json::json!({ "image_id": img_id, "skipped": true, "reason": reason }));
        let img = match rimg::by_id(ctx, img_id) {
            Ok(Some(i)) => i,
            Ok(None) => {
                bad("图片已不存在".into());
                continue;
            }
            Err(e) => {
                bad(e.to_string());
                continue;
            }
        };
        // 画稿走的是 canvas 那条（整幅生成、不缝合），批量重绘这里要显式挡掉并给可读理由，
        // 而不是让它去报"原图文件已丢失"
        if img.is_sketch() {
            bad("这是画稿，请在画布里点「生成」".into());
            continue;
        }
        if !util::file_alive(&ctx.data, &Value::String(img.orig_path.clone())) {
            bad("原图文件已不在磁盘上（data/projects 被清过？）".into());
            continue;
        }
        // 反向涂抹时"一笔没涂"是合法输入（= 整幅重绘），整图重绘更是**根本不看遮罩**，
        // 所以这两条都不要求遮罩存在或可读；正向局部重绘仍然要先涂
        let full = settings.get("full").and_then(Value::as_bool).unwrap_or(false);
        let mask_ok = invert || full || match img.mask_path.as_deref() {
            Some(m) if util::file_alive(&ctx.data, &Value::String(m.to_string())) => true,
            Some(_) => {
                bad("遮罩文件已不在磁盘上，重涂一次再提交".into());
                false
            }
            None => {
                bad("未涂遮罩".into());
                false
            }
        };
        if !mask_ok {
            continue;
        }
        let id = rres::insert_cloud(ctx, img.id, img.project_id, &prompt, &settings.to_string(), &model, rerun_of)?;
        rres::set_queued(ctx, id)?;
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

/// 有空位就把 queued 行叫起来。每次入队、每个任务收尾、每次启动都调它。
///
/// 返回擦了类型的 future：worker 收尾要再叫一次 pump，不把这层递归切开，
/// 编译器连 `pump` 自己的 future 类型都定不下来（更谈不上 Send）。
pub fn pump(ctx: &Shared) -> std::pin::Pin<Box<dyn std::future::Future<Output = usize> + Send + '_>> {
    Box::pin(async move {
        let want = cloud::settings(ctx).concurrency.clamp(1, CONCURRENCY_MAX as i64) as usize;
        sync_gate(want);
        let ids = match rres::list_queued(ctx) {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let mut started = 0usize;
        for id in ids {
            let Ok(permit) = gate().try_acquire() else { break };
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
            LIVE.fetch_add(1, Ordering::AcqRel);
            let task: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> = Box::pin(async move {
                /* 收尾顺序是要害：`sync_gate` 算的是 `want − LIVE`，而 LIVE 由 `_live` 在退出作用域时减。
                   以前它一直挂到整个 task 结束，于是这一次收尾泵看到的是"这一张还在飞"：
                   并发=1 时 target 算成 0，会把刚 `drop(permit)` 还回来的那个坑位又 forget 掉，
                   排在前面的 queued 行就此没人叫——整批停在第二张，直到下次入队或重启才动。 */
                {
                    let _live = InFlight;
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
    let outcome = match rres::by_id(ctx, id) {
        Ok(Some(row)) if row.running() && row.is_cloud() => run_inner(ctx, &row).await,
        // 行已经被删掉或落定了：这一格要还给登记表，否则 ctx.jobs 只涨不落
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    };
    finish(ctx, id, outcome);
}

async fn run_inner(ctx: &Shared, row: &entity::ResultRow) -> std::result::Result<(), String> {
    let img = rimg::by_id(ctx, row.image_id).map_err(|e| e.to_string())?
        .ok_or_else(|| "图片已不存在，这一张没法跑".to_string())?;
    if img.is_sketch() {
        return run_sketch(ctx, row, img).await;
    }
    // 整图重绘：把整张原图发过去按提示词生成。它和画布那条同形（不带遮罩、回来不缝合），
    // 差别只在输入是照片、且原图不透明
    if row.settings().get("full").and_then(Value::as_bool).unwrap_or(false) {
        return run_full(ctx, row, img).await;
    }
    let prompt = row.prompt.clone();
    let settings = row.settings();
    let pre = {
        let ctx = ctx.clone();
        let img = img.clone();
        let settings = settings.clone();
        tokio::task::spawn_blocking(move || prepare(&ctx, &img, &settings)).await
    }
    .map_err(|e| format!("裁切线程崩了：{e}"))??;
    let bytes = cloud::edit(
        ctx,
        cloud::Edit {
            image: Some(pre.crop.clone()),
            mask: if pre.no_mask { None } else { Some(pre.mask.clone()) },
            // 裁切重绘那一路的"参考"就是这一枪自己的裁切区与遮罩，不再外挂参考图
            refs: Vec::new(),
            prompt: &prompt,
            size: Some(&pre.size),
        },
    )
    .await
    .map_err(|e| e.chars().take(300).collect::<String>())?;
    let ctx2 = ctx.clone();
    let id = row.id;
    tokio::task::spawn_blocking(move || compose(&ctx2, id, &img, pre, &bytes))
        .await
        .map_err(|e| format!("缝合线程崩了：{e}"))?
}

fn finish(ctx: &Shared, id: i64, r: std::result::Result<(), String>) {
    if let Err(e) = r {
        ctx.mark_error(id, &e);
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
}

/// 读原图 + 蒙版，折出裁切区。全程在原图分辨率上做，浏览器只需要交出 proxy 分辨率的涂抹层。
///
/// 裁切目标长边取**这一行提交时的快照**（`settings.edge`），不是跑到的那一刻的全局设置：
/// 精修页的尺寸胶囊以前写进 settings 却没人读，等于一个骗人的旋钮。
/// `settings.invert` 同理按行生效（反向涂抹只开云端这条路）。
fn prepare(ctx: &Shared, img: &Image, settings: &Value) -> std::result::Result<Prepared, String> {
    let s = cloud::settings(ctx);
    let edge = util::number_of(settings.get("edge")).filter(|n| *n >= 1.0).map(|n| n.clamp(512.0, 3840.0) as i64);
    let params = StitchParams {
        expand: s.stitch_expand as f64,
        context: stitch_core::DEFAULT_CONTEXT,
        crop_edge: edge.unwrap_or(s.stitch_edge.max(1)) as u32,
        feather: s.stitch_feather as f64,
        levels: stitch_core::DEFAULT_LEVELS,
        invert: settings.get("invert").and_then(Value::as_bool).unwrap_or(false),
    };
    // 云端吃的也是"调整后"的那一张（拍板 4）：用户涂的蒙版画在调整后预览上，坐标天然同域
    let ops = crate::service::adjust::load_ops(ctx, img.id);
    let photo = crate::service::adjust::photo_with(ctx, img, &ops)?;
    // 反向涂抹且没存过遮罩 = 一笔没保 = 整幅重绘，这时没有遮罩文件是合法输入；正向仍然要拦
    let mask_alpha = match img.mask_path.as_deref().and_then(|m| util::data_file(&ctx.data, m)) {
        Some(p) => codec::decode(&std::fs::read(&p).map_err(|e| format!("读遮罩失败：{e}"))?)?.alpha(),
        // 按**已解码的真实尺寸**开这张空笔迹缓冲：库里那对 w/h 是导入时客户端自报的（只夹到 3 万），
        // 跟着它开就是 30000×30000 = 900MB，dilate 再 clone 一份，ink_bbox 单线程扫 9 亿个点
        None if params.invert => stitch_core::Alpha::new(photo.w, photo.h),
        None => return Err("遮罩上没有可用笔迹，涂一处再提交".into()),
    };
    match build_crop_payload(&photo, &mask_alpha, &params) {
        Build::Payload(p) => Ok(Prepared {
            // 云端按 fit 后的档位收图，所以 size 由裁切区自己说了算
            crop: codec::encode_png(&p.image),
            mask: codec::encode_png(&p.mask),
            no_mask: p.no_mask,
            size: format!("{}x{}", p.image.w, p.image.h),
            base: photo,
            alpha: mask_alpha,
            crop_box: p.crop,
            params,
        }),
        Build::NoInk => Err("遮罩上没有可用笔迹，涂一处再提交".into()),
        Build::Unfit => Err("裁切区的尺寸/比例超出云端允许范围（长边≤3840、比例≤3:1、像素 65.5 万~829 万）".into()),
    }
}

/// 云端回来的图贴回原图并落盘：一次写完成图 + 320 缩略图，前端不用再回传任何东西
fn compose(ctx: &Shared, id: i64, img: &Image, pre: Prepared, bytes: &[u8]) -> std::result::Result<(), String> {
    if ctx.job_cancelled(id) {
        return Err("已手动中断，这一张的结果不再采用".into());
    }
    let model = codec::decode(bytes)?;
    let final_img = stitch_crop(&pre.base, &pre.alpha, pre.crop_box, &model, &pre.params);
    let dir = ctx.data.join("projects").join(img.project_id.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败：{e}"))?;
    let rel = util::rel_path(&["projects".into(), img.project_id.to_string(), format!("r{id}_{}_final.png", util::now_ms())]);
    std::fs::write(ctx.data.join(&rel), codec::encode_png(&final_img)).map_err(|e| format!("写成图失败：{e}"))?;
    // 缩略图失败不影响这一张成图：历史列回落到 final_url 就行
    let thumb = imagesvc::result_thumb(ctx, id, img.project_id, &rel);
    if !rres::set_done(ctx, id, &rel, thumb.as_deref()).map_err(|e| e.to_string())? {
        // 这几秒里那一行被中断或删掉了：成图与缩略档都不该留在盘上指着空气
        let _ = std::fs::remove_file(ctx.data.join(&rel));
        if let Some(t) = thumb.as_deref() {
            let _ = std::fs::remove_file(ctx.data.join(t));
        }
    }
    Ok(())
}

/// 画布的一次生成：画稿拍到白底当输入图，**不带遮罩**，回来的图直接是成图。
///
/// 这里没有裁切与缝合——画布不存在"蒙版外要保持"这件事。状态机、并发闸、
/// 重启续跑都照用队列这一套，所以只有"准备载荷"和"落盘"两段是画布自己的。
async fn run_sketch(ctx: &Shared, row: &entity::ResultRow, img: Image) -> std::result::Result<(), String> {
    let prompt = row.prompt.clone();
    /* 发出去的是**这一行提交那一刻**的那份快照，不是此刻的画稿：排着队的时候接着画两笔，
       画稿会被自动保存覆写，那时图对应 t1 而行上的 sketch_url 还是 t0，
       对比层与「取回这一版画稿」就都拿 t0 说话。快照没留下时才回落到此刻的画稿。 */
    let src = row.sketch_path.clone().unwrap_or_else(|| img.orig_path.clone());
    let (png, size, inked) = {
        let ctx2 = ctx.clone();
        let src2 = src.clone();
        tokio::task::spawn_blocking(move || prepare_sketch(&ctx2, &src2))
            .await
            .map_err(|e| format!("画布线程崩了：{e}"))??
    };
    // 参考图读的是**这一行**的快照（不是此刻的槽位），与画稿快照同一条纪律。
    // 一次最多四张全分辨率 PNG，读盘这段也不该按在 worker 上
    let refs_bytes = {
        let ctx2 = ctx.clone();
        let row2 = row.clone();
        tokio::task::spawn_blocking(move || refs::row_payloads(&ctx2, &row2))
            .await
            .map_err(|e| format!("读参考图的那次没跑完：{e}"))??
    };
    // 一笔没涂又挂了参考图：那张全白的纸不必发出去，这一枪就是"按参考图与提示词生成"。
    // 没参考图时维持老行为（发空白画稿），免得动了既有语义。
    let image = if inked || refs_bytes.is_empty() { Some(png) } else { None };
    let bytes = cloud::edit(
        ctx,
        cloud::Edit { image, mask: None, refs: refs_bytes, prompt: &prompt, size: Some(&size) },
    )
    .await
    .map_err(|e| e.chars().take(300).collect::<String>())?;
    let id = row.id;
    let ctx2 = ctx.clone();
    tokio::task::spawn_blocking(move || compose_sketch(&ctx2, id, &img, &bytes))
        .await
        .map_err(|e| format!("落盘线程崩了：{e}"))?
}

/// 返回 `(发出去的 PNG, 尺寸串, 有没有笔迹)`。第三项决定"空白画稿 + 参考图"那一枪要不要带主图。
fn prepare_sketch(ctx: &Shared, rel: &str) -> std::result::Result<(Vec<u8>, String, bool), String> {
    let raw = std::fs::read(util::data_file(&ctx.data, rel).ok_or_else(|| format!("读不到画稿：{rel}"))?)
        .map_err(|e| format!("读不到画稿：{e}"))?;
    let rgba = codec::decode(&raw)?;
    sketch_fit(rgba.w, rgba.h)?;
    let inked = rgba.px.iter().skip(3).step_by(4).any(|&a| a != 0);
    let flat = codec::flatten(&rgba, [255, 255, 255]);
    Ok((codec::encode_png(&flat), format!("{}x{}", rgba.w, rgba.h), inked))
}

/// 云端接口对图片尺寸的硬约束（长边 ≤3840、比例 ≤3:1、像素 65.5 万~829 万）。
/// 建画布时按这一条拒过一次，提交时再判一次：库里可能躺着别的入口写进来的行。
pub fn sketch_fit(w: usize, h: usize) -> std::result::Result<(), String> {
    use stitch_core::geom::{EDGE_MAX, PX_MAX, PX_MIN, RATIO_MAX};
    if w == 0 || h == 0 {
        return Err("画布尺寸是 0".into());
    }
    let long = w.max(h);
    let ratio = long as f64 / w.min(h) as f64;
    let px = w as u64 * h as u64;
    if long > EDGE_MAX as usize || ratio > RATIO_MAX || !(PX_MIN..=PX_MAX).contains(&px) {
        return Err(format!(
            "画布 {w}×{h} 超出云端允许范围（长边≤{}、比例≤{RATIO_MAX}:1、像素 {}~{}）",
            EDGE_MAX,
            PX_MIN,
            PX_MAX
        ));
    }
    Ok(())
}

/// 整图重绘的一次生成：整张原图折进云端几何框、不带遮罩，回来的图就是成图。
async fn run_full(ctx: &Shared, row: &entity::ResultRow, img: Image) -> std::result::Result<(), String> {
    let prompt = row.prompt.clone();
    let settings = row.settings();
    let (buf, size) = {
        let ctx2 = ctx.clone();
        let img2 = img.clone();
        let settings = settings.clone();
        tokio::task::spawn_blocking(move || prepare_full(&ctx2, &img2, &settings))
            .await
            .map_err(|e| format!("整图线程崩了：{e}"))??
    };
    let bytes = cloud::edit(
        ctx,
        cloud::Edit { image: Some(buf), mask: None, refs: Vec::new(), prompt: &prompt, size: Some(&size) },
    )
    .await
    .map_err(|e| e.chars().take(300).collect::<String>())?;
    let id = row.id;
    let ctx2 = ctx.clone();
    tokio::task::spawn_blocking(move || compose_whole(&ctx2, id, &img, &bytes, "f"))
        .await
        .map_err(|e| format!("落盘线程崩了：{e}"))?
}

/// 整图重绘的目标尺寸判定：`(宽, 高, 是否原样发)`。
/// 纯函数，好让"折不折得进""要不要重编码"这两条判据能被单测钉住（不碰磁盘与像素缓冲）。
pub fn full_target(w: usize, h: usize, edge: u32) -> std::result::Result<(u32, u32, bool), String> {
    let fit = stitch_core::geom::fit_size(w as u32, h as u32, edge);
    if fit.unfit || fit.w == 0 || fit.h == 0 {
        return Err(format!(
            "整张图 {w}×{h} 折不进云端允许的框（长边≤{}、比例≤{}:1、像素 {}~{}）",
            stitch_core::geom::EDGE_MAX,
            stitch_core::geom::RATIO_MAX,
            stitch_core::geom::PX_MIN,
            stitch_core::geom::PX_MAX
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
fn prepare_full(ctx: &Shared, img: &Image, settings: &Value) -> std::result::Result<(Vec<u8>, String), String> {
    let s = cloud::settings(ctx);
    let edge = util::number_of(settings.get("edge"))
        .filter(|n| *n >= 1.0)
        .map(|n| n.clamp(512.0, 3840.0) as u32)
        .unwrap_or_else(|| s.stitch_edge.max(1) as u32);
    // 整图重绘同样吃"调整后"的图；调整过就不能再走"原样透传文件字节"那条捷径，
    // 透传的意义是不为一张没动过的图多付一次解码重编码
    let ops = crate::service::adjust::load_ops(ctx, img.id);
    let dec = crate::service::adjust::photo_with(ctx, img, &ops)?;
    let (fw, fh, passthrough) = full_target(dec.w, dec.h, edge)?;
    if passthrough && ops.is_identity() {
        let raw = std::fs::read(util::data_file(&ctx.data, &img.orig_path).ok_or_else(|| "读不到原图".to_string())?)
            .map_err(|e| format!("读原图失败：{e}"))?;
        if raw_sendable(&raw) {
            return Ok((raw, format!("{}x{}", dec.w, dec.h)));
        }
    }
    let scaled = stitch_core::resize_rgba(&dec, fw as usize, fh as usize);
    Ok((codec::encode_png(&scaled), format!("{fw}x{fh}")))
}

fn compose_sketch(ctx: &Shared, id: i64, img: &Image, bytes: &[u8]) -> std::result::Result<(), String> {
    compose_whole(ctx, id, img, bytes, "c")
}

/// 整幅出图的那一路共用的落盘：先解一次确认不是坏图，再写成图 + 补缩略档。
/// 画布（前缀 `c`）与照片整图重绘（前缀 `f`）都走这里——它们都没有缝合这一步。
fn compose_whole(ctx: &Shared, id: i64, img: &Image, bytes: &[u8], prefix: &str) -> std::result::Result<(), String> {
    if ctx.job_cancelled(id) {
        return Err("已手动中断，这一张的结果不再采用".into());
    }
    // 先解一次再落盘：坏图进了库就会变成"卡片在、点开是空的"
    let decoded = codec::decode(bytes)?;
    if decoded.w == 0 || decoded.h == 0 {
        return Err("云端回来的图是空的".into());
    }
    let rel = util::rel_path(&[
        "projects".into(),
        img.project_id.to_string(),
        format!("{prefix}{id}_{}_final.png", util::now_ms()),
    ]);
    imagesvc::write_bytes(&ctx.data.join(&rel), bytes).map_err(|e| format!("写成图失败：{e}"))?;
    let thumb = imagesvc::result_thumb(ctx, id, img.project_id, &rel);
    if !rres::set_done(ctx, id, &rel, thumb.as_deref()).map_err(|e| e.to_string())? {
        let _ = std::fs::remove_file(ctx.data.join(&rel));
        if let Some(t) = thumb.as_deref() {
            let _ = std::fs::remove_file(ctx.data.join(t));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 并发从 3 下调到 1 时若还有 3 张在飞：增量式调整一个许可也削不掉，
    /// 等它们跑完 available 会回到 3（设置写串行、实际同发 3 张），再往上调还能冲破上限。
    #[test]
    fn 在飞时下调并发不会漏发也不冲破上限() {
        sync_gate(3);
        assert_eq!(gate().available_permits(), 3);
        let held: Vec<_> = (0..3).map(|_| gate().try_acquire().unwrap()).collect();
        LIVE.fetch_add(3, Ordering::AcqRel);
        sync_gate(1);
        assert_eq!(gate().available_permits(), 0, "三张在飞时不该还有空位");
        drop(held);
        LIVE.fetch_sub(3, Ordering::AcqRel);
        sync_gate(1);
        assert_eq!(gate().available_permits(), 1, "释放之后只留一个坑");
        sync_gate(6);
        assert_eq!(gate().available_permits(), 6, "上调要能一路开到上限");
        sync_gate(1);
        assert_eq!(gate().available_permits(), 1);
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
            let (fw, fh, same) = super::full_target(w, h, edge).unwrap_or_else(|e| panic!("{w}×{h}@{edge}：{e}"));
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
        let e = super::full_target(4000, 400, 2048).unwrap_err();
        assert!(e.contains("4000×400"), "{e}");
        assert!(e.contains("折不进"), "{e}");
    }
}
