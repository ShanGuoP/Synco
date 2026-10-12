//! 历史快照的无损重算与显式降级。
use crate::error::{AppError, Result};
use crate::models::entity;
use crate::repo::images as rimg;
use crate::state::Shared;
use crate::{img::codec, util};
use serde_json::Value;
use stitch_core::stitch_crop;
use super::crop::prepared_from_snapshot;
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
