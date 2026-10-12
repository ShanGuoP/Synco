//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{body_of, err, ok, path_id};
use crate::error::{AppError, Result};
use crate::models::dto;
use crate::repo::{images as rimg, results as rres};
use crate::state::Shared;
use crate::{service::imagesvc, util};
use axum::body::Bytes;
use axum::extract::{Path as APath, State};
use axum::response::Response;
use serde_json::Value;

pub async fn image_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "srv.common.noImage")) };
    // 纯读：状态推进在服务端那条推进器上（service::reclaim::spawn_advancer），
    // 不再让一次 GET 顺带把云端僵尸判死——跨站页面用一个 <img> 就能驱动写库
    if img.thumb_path.is_none() {
        imagesvc::spawn_derive(&ctx, img.clone());
    }
    let results: Vec<Value> = rres::list_for_image(&ctx, iid, 8)?
        .iter()
        .map(|r| dto::result_json(&ctx, r))
        .collect();
    let mut out = match dto::image_json(&ctx, &img, false).as_object().cloned() {
        Some(m) => m,
        None => return Err(AppError::fail("srv.image.rowBroken")),
    };
    out.insert("results".into(), Value::Array(results.clone()));
    out.insert("last".into(), results.first().cloned().unwrap_or(Value::Null));
    Ok(ok(Value::Object(out)))
}

/// 懒生成的一张 320 缩略图：列表页拿它当占位，不想为一排卡片去 GET 整行详情。
/// 档还没补出来时回 `thumb_url: null` + `pending: true`，前端按既有约定回落 orig_url。
pub async fn thumb_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "srv.common.noImage")) };
    Ok(ok(match img.thumb_path.clone() {
        Some(rel) => serde_json::json!({ "thumb_url": format!("/file/{rel}") }),
        None => {
            imagesvc::spawn_derive(&ctx, img.clone());
            serde_json::json!({ "thumb_url": null, "pending": true })
        }
    }))
}

/// 瓦片清单：首次访问会当场切一套（原图不可变，之后 immutable）
pub async fn tiles_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "srv.common.noImage")) };
    // 限流与"派生档同时在制不超过两份"是同一条纪律，见 imagesvc::tiles_async
    match imagesvc::tiles_async(&ctx, img).await {
        Ok(v) => Ok(ok(v)),
        Err(e) => Err(e),
    }
}

/// 派生查看：这张图「另存为新图」出去的那些子图。
/// 只认库里存的父子关系（`images.derived_from`），不认文件名——改过名的老行由启动时那次回填补上。
pub async fn derived_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let kids = rimg::list_derived(&ctx, iid)?;
    let out: Vec<Value> = kids.iter().map(|i| dto::image_json(&ctx, i, true)).collect();
    Ok(ok(serde_json::json!({ "images": out, "parent": iid })))
}

pub async fn image_delete(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    crate::service::deletion::delete_image(&ctx, iid).await?;
    Ok(ok(serde_json::json!({ "ok": true })))
}

pub async fn mask_post(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let iid = path_id(&id)?;
    let Some(i) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "srv.common.noImage")) };
    let b64 = body.get("b64").and_then(|v| v.as_str()).unwrap_or("");
    let mut mask_rel: Option<String> = None;
    if !b64.is_empty() {
        let stem = util::stem_of(&i.orig_path);
        let pid = i.project_id;
        let bytes = util::decode_b64(b64);
        let data = ctx.data.clone();
        // 覆写前先解码一次：前端兜底路径曾产出过 0×0 画布的 `"data:,"`，那种东西解出来
        // 是三字节垃圾，直接写就会把用户已有的笔迹盖掉且不可恢复。解不出图就拒。
        // 解码与覆写都是同步重活，整段过阻塞池；原地覆写会让并发读者拿到半张 PNG，
        // 所以仍走 write_bytes 那套 .part→rename 纪律
        let rel = util::blocking(move || -> Result<String> {
            if crate::img::codec::decode(&bytes).is_err() {
                return Err(AppError::bad("srv.image.maskBadPng"));
            }
            let rel = util::rel_path(&["projects".into(), pid.to_string(), format!("{stem}_mask.png")]);
            imagesvc::write_bytes(&data.join(&rel), &bytes)?;
            Ok(rel)
        })
        .await?;
        mask_rel = Some(rel);
    } else if let Some(old) = i.mask_path.filter(|s| !s.is_empty()) {
        // 清空遮罩：文件跟着走，否则换掉笔迹后旧 PNG 一直躺在目录里
        if let Some(p) = util::data_file(&ctx.data, &old) {
            let _ = std::fs::remove_file(p);
        }
    }
    rimg::set_mask(&ctx, iid, mask_rel.as_deref())?;
    Ok(ok(serde_json::json!({ "ok": true })))
}
