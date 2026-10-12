//! 提交时冻结调整依赖；历史重算只使用冻结输入。
use crate::error::{AppError, Result};
use crate::models::entity::Image;
use crate::state::Ctx;
use crate::img::codec;
use crate::repo::adjust as radj;
use photoedit_core::{apply_chain, Chain, EditOps, FaceShape, LutTable};
use serde_json::{json, Value};
use stitch_core::Rgba;
use super::{lut_table, orig_rgba, shape_for};
/// 参数引用的人脸点与 LUT 同样可变，不能在排队结束或无损重算时重新读取。
pub(crate) fn snapshot_inputs(ctx: &Ctx, img: &Image, ops: &EditOps) -> Result<Value> {
    let shape = if ops.warp.auto.is_empty() { None } else { shape_for(ctx, img) };
    let lut = match &ops.lut { Some(l) => lut_table(ctx, &l.name)?, None => None };
    let skin = if ops.beauty.auto_face { Some(radj::landmarks(ctx,img.id)?) } else { None };
    let meshes = if ops.portrait.needs_faces() { Some(crate::service::face::cached_meshes(ctx,img)?) } else { None };
    Ok(json!({ "shape": shape, "lut": lut, "skin": skin, "meshes": meshes }))
}

pub(crate) fn photo_from_snapshot(ctx: &Ctx, img: &Image, ops: &EditOps, inputs: &Value, mask_png: Option<&[u8]>) -> Result<Rgba> {
    let base = orig_rgba(ctx, img)?;
    if ops.is_identity() { return Ok(base); }
    let shape: Option<FaceShape> = serde_json::from_value(inputs["shape"].clone()).map_err(|_| AppError::bad("srv.lossless.noSnap"))?;
    let lut: Option<LutTable> = serde_json::from_value(inputs["lut"].clone()).map_err(|_| AppError::bad("srv.lossless.noSnap"))?;
    let mut mask = if ops.beauty.by_mask {
        match mask_png {
            Some(png) => {
                let m = codec::decode(png)?;
                Some(stitch_core::crop_scale_rgba(&m, 0, 0, m.w, m.h, base.w, base.h).alpha())
            },
            None => None,
        }
    } else { None };
    if ops.beauty.auto_face {
        let rows = inputs["skin"].as_array().ok_or_else(|| AppError::bad("srv.lossless.noSnap"))?;
        let groups: Vec<Vec<[f32;2]>> = rows.iter().filter_map(|r| r["points"].as_str().map(photoedit_core::face::parse_points)).collect();
        let mut skin = photoedit_core::face::mesh_beauty_region(base.w,base.h,&groups);
        if let Some(m) = mask { for (a,b) in skin.v.iter_mut().zip(m.v) { *a=((*a as u16*b as u16)/255) as u8; } }
        mask = Some(skin);
    }
    let meshes: Vec<photoedit_core::portrait_ops::Mesh> = if ops.portrait.needs_faces() {
        serde_json::from_value(inputs["meshes"].clone()).map_err(|_| AppError::bad("srv.lossless.noSnap"))?
    } else { Vec::new() };
    let portrait_mask = if ops.portrait.faces.iter().any(|f| f.beauty.by_mask) {
        match mask_png { Some(png) => { let m=codec::decode(png)?; Some(stitch_core::crop_scale_rgba(&m,0,0,m.w,m.h,base.w,base.h).alpha()) }, None=>None }
    } else { None };
    Ok(apply_chain(&base, ops, &Chain { mask: mask.as_ref(), shape: shape.as_ref(), lut: lut.as_ref(),meshes:&meshes,portrait_mask:portrait_mask.as_ref() }))
}
