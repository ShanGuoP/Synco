//! 画稿与整图生成：载荷准备、尺寸校验及产物落盘。
use crate::error::{AppError, Result};
use crate::models::{entity, entity::Image};
use crate::repo::results as rres;
use crate::service::{cloud, imagesvc, refs};
use crate::state::Shared;
use crate::{img::codec, util};
use serde_json::json;
use super::PendingFiles;
use super::spec::JobSpec;
/// 画布的一次生成：画稿拍到白底当输入图，**不带遮罩**，回来的图直接是成图。
///
/// 这里没有裁切与缝合——画布不存在"蒙版外要保持"这件事。状态机、并发闸、
/// 重启续跑都照用队列这一套，所以只有"准备载荷"和"落盘"两段是画布自己的。
pub(super) async fn run_sketch(ctx: &Shared, row: &entity::ResultRow, img: Image, config: cloud::CloudSettings) -> Result<()> {
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
pub(super) async fn run_full(ctx: &Shared, row: &entity::ResultRow, img: Image, spec: JobSpec, mask: Vec<u8>) -> Result<()> {
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
pub(super) const PNG_SIG: &[u8; 8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

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
    let mut pending = PendingFiles::new(ctx.data.clone());
    imagesvc::write_bytes(&pending.track(&rel), bytes)?;
    let thumb = imagesvc::write_result_thumb(ctx, id, img.project_id, &rel);
    if let Some(t) = &thumb { pending.track(t); }
    if rres::set_done(ctx, id, &rel, thumb.as_deref())? { pending.commit(); }
    Ok(())
}
