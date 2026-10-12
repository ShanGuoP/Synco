//! 云端批量队列：`queued → running → done/error` 全部存在 results 表里。
//! 调度器只在进程内跑一个"有空位就叫醒下一个"的循环，所以关页面照样跑完；
//! 服务重启后 queued 行由启动时的 reclaim 重新叫起来，正在飞的那次请求接不回来（判掉重来）。

use crate::error::{AppError, Result};
use crate::models::{entity, entity::Image};
use crate::repo::{images as rimg, results as rres};
use crate::service::cloud;
use crate::state::Shared;
use crate::util;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
mod spec;
mod files;
pub(crate) use files::PendingFiles;
use spec::JobKind;

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


mod crop;
mod whole;
mod lossless;
use crop::compose;
use whole::{run_full, run_sketch};
pub use whole::{full_target, raw_sendable, sketch_fit};
pub use lossless::{lossless_bytes, OutImage};
#[cfg(test)]
use crop::prepare;
#[cfg(test)]
use whole::PNG_SIG;
#[cfg(test)]
use crate::img::codec;
#[cfg(test)]
use stitch_core::stitch_crop;
#[cfg(test)]
mod tests;
