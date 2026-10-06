//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{body_of, err, ok, path_id};
use crate::error::{AppError, Result};
use crate::models::dto;
use crate::repo::{images as rimg, results as rres};
use crate::service::reclaim;
use crate::state::Shared;
use crate::{service::imagesvc, util};
use axum::body::Bytes;
use axum::extract::{Path as APath, State};
use axum::response::Response;
use serde_json::Value;

pub async fn image_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "no image")) };
    // 有人读到这张图 = 顺手把它的云端僵尸判掉，再把它欠的派生档补上（存量库第一次进编辑器也靠这条）
    for z in rres::cloud_zombies(&ctx, iid)? {
        reclaim::judge_cloud(&ctx, z);
    }
    if img.thumb_path.is_none() {
        imagesvc::spawn_derive(&ctx, img.clone());
    }
    let results: Vec<Value> = rres::list_for_image(&ctx, iid, 8)?
        .iter()
        .map(|r| dto::result_json(&ctx, r))
        .collect();
    let mut out = match dto::image_json(&ctx, &img, false).as_object().cloned() {
        Some(m) => m,
        None => return Err(AppError::Fail("这张图的响应构不出来（库里的行坏了）".into())),
    };
    out.insert("results".into(), Value::Array(results.clone()));
    out.insert("last".into(), results.first().cloned().unwrap_or(Value::Null));
    Ok(ok(Value::Object(out)))
}

/// 懒生成的一张 320 缩略图：列表页拿它当占位，不想为一排卡片去 GET 整行详情。
/// 档还没补出来时回 `thumb_url: null` + `pending: true`，前端按既有约定回落 orig_url。
pub async fn thumb_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "no image")) };
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
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "no image")) };
    let ctx2 = ctx.clone();
    let out = tokio::task::spawn_blocking(move || imagesvc::tiles(&ctx2, &img))
        .await
        .unwrap_or_else(|e| Err(format!("瓦片线程崩了：{e}")));
    match out {
        Ok(v) => Ok(ok(v)),
        Err(e) => Ok(err(500, e.as_str())),
    }
}

pub async fn image_delete(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    if let Some(i) = rimg::by_id(&ctx, iid)? {
        for rel in [Some(i.orig_path.clone()).filter(|s| !s.is_empty()), i.mask_path.clone()] {
            if let Some(rel) = rel {
                let _ = std::fs::remove_file(ctx.data.join(rel));
            }
        }
        imagesvc::purge(&ctx, &i);
        rres::drop_files(&ctx, "image_id=?", crate::repo::i(iid))?;
        rres::delete_for_image(&ctx, iid)?;
        rimg::delete(&ctx, iid)?;
    }
    Ok(ok(serde_json::json!({ "ok": true })))
}

pub async fn mask_post(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let iid = path_id(&id)?;
    let Some(i) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "no image")) };
    let b64 = body.get("b64").and_then(|v| v.as_str()).unwrap_or("");
    let mut mask_rel: Option<String> = None;
    if !b64.is_empty() {
        let stem = util::stem_of(&i.orig_path);
        let bytes = util::decode_b64(b64);
        // 覆写前先解码一次：前端兜底路径曾产出过 0×0 画布的 `"data:,"`，那种东西解出来
        // 是三字节垃圾，直接写就会把用户已有的笔迹盖掉且不可恢复。解不出图就拒。
        if crate::img::codec::decode(&bytes).is_err() {
            return Err(AppError::bad("遮罩不是能解码的 PNG，这次没有覆盖已有遮罩"));
        }
        let rel = util::rel_path(&["projects".into(), i.project_id.to_string(), format!("{stem}_mask.png")]);
        // 原地覆写会让并发读者拿到半张 PNG，与派生档同一套 .part→rename 纪律
        imagesvc::write_bytes(&ctx.data.join(&rel), &bytes).map_err(AppError::Fail)?;
        mask_rel = Some(rel);
    } else if let Some(old) = i.mask_path.filter(|s| !s.is_empty()) {
        // 清空遮罩：文件跟着走，否则换掉笔迹后旧 PNG 一直躺在目录里
        let _ = std::fs::remove_file(ctx.data.join(old));
    }
    rimg::set_mask(&ctx, iid, mask_rel.as_deref())?;
    Ok(ok(serde_json::json!({ "ok": true })))
}
