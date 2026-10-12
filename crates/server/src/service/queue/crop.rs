//! 裁切载荷、缝合与原子结果登记。
use crate::error::{AppError, Result};
use crate::models::entity::Image;
use crate::repo::results as rres;
use crate::service::imagesvc;
use crate::state::Shared;
use crate::{img::codec, util};
use serde_json::Value;
use stitch_core::{build_crop_payload, stitch_crop, Build, StitchParams};
use super::PendingFiles;
pub(super) struct Prepared {
    pub(super) crop: Vec<u8>,
    pub(super) mask: Vec<u8>,
    /// 反向涂抹且一笔没涂 = 整幅重绘，这时不带 mask 这个 part
    pub(super) no_mask: bool,
    pub(super) size: String,
    pub(super) base: stitch_core::Rgba,
    pub(super) alpha: stitch_core::Alpha,
    pub(super) crop_box: stitch_core::Box2,
    pub(super) params: StitchParams,
    /// 这一枪实际用的遮罩文件字节（原样，供重算时复制落盘）。没有遮罩时是空的
    pub(super) mask_png: Vec<u8>,
    /// 重算无损所需的最小快照：缝合参数 + 几何与调整参数 + 规则版本
    pub(super) snap: Value,
}

/// 由"这一枪要用的那两张图"折出裁切载荷。`prepare`（提交）与 `lossless_bytes`（导出重算）
/// 必须共用这一段——重算要逐位等于当年那一张，就不能另写一份几何。
pub(super) fn build_prepared(photo: stitch_core::Rgba, mask_alpha: stitch_core::Alpha, params: &StitchParams) -> Result<Prepared> {
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

/// 无损重算的全部前提：缝合参数、几何与调整参数、缝合代码的规则版本。
/// 规则版本对不上就不许重算——那段代码改过取值条件之后，"重算"出来的已经不是当年那一张。
pub(super) fn stitch_snapshot(params: &StitchParams, ops: &photoedit_core::EditOps) -> Value {
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
pub(super) fn prepared_from_snapshot(ctx: &Shared, img: &Image, snap: &Value, mask_png: Option<&[u8]>) -> Result<Prepared> {
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
pub(super) fn compose(ctx: &Shared, id: i64, img: &Image, pre: &Prepared, bytes: &[u8]) -> Result<()> {
    if ctx.job_cancelled(id) {
        return Err(AppError::fail("srv.queue.byHand"));
    }
    let model = codec::decode(bytes)?;
    let final_img = stitch_crop(&pre.base, &pre.alpha, pre.crop_box, &model, &pre.params);
    let dir = ctx.data.join("projects").join(img.project_id.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| AppError::fail_detail("srv.common.mkdirFail", e))?;
    let ts = util::now_ms();
    let mut pending = PendingFiles::new(ctx.data.clone());
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

/// 测试辅助同样经过提交规格捕获，再按冻结的输入构造裁切载荷。
#[cfg(test)]
pub(super) fn prepare(ctx: &Shared, img: &Image, settings: &Value) -> Result<Prepared> {
    use super::spec;
    use crate::service::cloud;
    let (spec, mask) = spec::capture(ctx, img, settings, cloud::settings(ctx))?;
    spec::prepare_job(ctx, img, &spec, mask)
}
