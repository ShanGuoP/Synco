//! 云端批量队列：`queued → running → done/error` 全部存在 results 表里。
//! 调度器只在进程内跑一个"有空位就叫醒下一个"的循环，所以关页面照样跑完；
//! 服务重启后 queued 行由启动时的 reclaim 重新叫起来，正在飞的那次请求接不回来（判掉重来）。

use crate::error::Result;
use crate::models::{entity, entity::Image};
use crate::repo::{images as rimg, results as rres};
use crate::service::cloud;
use crate::state::Shared;
use crate::{img::codec, service::imagesvc, util};
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
fn sync_gate(want: usize) {
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
        if !util::file_alive(&ctx.data, &Value::String(img.orig_path.clone())) {
            bad("原图文件已不在磁盘上（data/projects 被清过？）".into());
            continue;
        }
        let mask_ok = match img.mask_path.as_deref() {
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
                let _live = InFlight;
                run_job(&ctx, id).await;
                // 归还坑位后再叫一次：permit 由 Drop 还，在飞计数由 _live 还
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
    let prompt = row.prompt.clone();
    let pre = {
        let ctx = ctx.clone();
        let img = img.clone();
        tokio::task::spawn_blocking(move || prepare(&ctx, &img)).await
    }
    .map_err(|e| format!("裁切线程崩了：{e}"))??;
    let bytes = cloud::edit(ctx, pre.crop.clone(), pre.mask.clone(), &prompt, Some(&pre.size))
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
    size: String,
    base: stitch_core::Rgba,
    alpha: stitch_core::Alpha,
    crop_box: stitch_core::Box2,
    params: StitchParams,
}

/// 读原图 + 蒙版，折出裁切区。全程在原图分辨率上做，浏览器只需要交出 proxy 分辨率的涂抹层。
fn prepare(ctx: &Shared, img: &Image) -> std::result::Result<Prepared, String> {
    let s = cloud::settings(ctx);
    let params = StitchParams {
        expand: s.stitch_expand as f64,
        context: stitch_core::DEFAULT_CONTEXT,
        crop_edge: s.stitch_edge.max(1) as u32,
        feather: s.stitch_feather as f64,
        levels: stitch_core::DEFAULT_LEVELS,
    };
    let photo = codec::decode(&std::fs::read(ctx.data.join(&img.orig_path)).map_err(|e| format!("读原图失败：{e}"))?)?;
    let mask = codec::decode(&std::fs::read(ctx.data.join(img.mask_path.as_deref().unwrap_or(""))).map_err(|e| format!("读遮罩失败：{e}"))?)?;
    match build_crop_payload(&photo, &mask.alpha(), &params) {
        Build::Payload(p) => Ok(Prepared {
            // 云端按 fit 后的档位收图，所以 size 由裁切区自己说了算
            crop: codec::encode_png(&p.image),
            mask: codec::encode_png(&p.mask),
            size: format!("{}x{}", p.image.w, p.image.h),
            base: photo,
            alpha: mask.alpha(),
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
    rres::set_done(ctx, id, &rel, thumb.as_deref()).map_err(|e| e.to_string())
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
}
