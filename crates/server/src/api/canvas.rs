//! 画布（图生图）：空白画面上画几笔，整张交给云端按提示词生成。
//!
//! 画稿是 `images` 表里 `kind='sketch'` 的一行，`orig_path` 存的就是那张 PNG。
//! 它与照片的唯一区别是**没有"蒙版外要保持"这回事**：不带遮罩、不裁切、不缝合，
//! 所以出图走 `service::queue` 里的画布分支，而照片那两条入口（`/api/run`、`/api/cloud/queue`）
//! 会按 `kind` 显式拒绝画稿。

use super::common::{bad, body_of, err, ok, path_id};
use crate::error::{AppError, Result};
use crate::models::dto;
use crate::repo::{images as rimg, projects as rproj, results as rres};
use crate::service::{cloud, imagesvc, queue};
use crate::state::Shared;
use crate::util;
use axum::body::Bytes;
use axum::extract::{Path as APath, State};
use axum::response::Response;
use serde_json::{Map, Value};
use stitch_core::Rgba;

/// 画布默认档：1024×1024 正好落在云端硬约束中间，和本机那路的 1024 手感一致
const DEFAULT_EDGE: i64 = 1024;

fn new_name(body: &Value) -> String {
    let raw = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
    util::clip(if raw.is_empty() { "画布" } else { raw }, 60)
}

/// 建一张空白画布。可以给现有项目加，也可以顺手开一个只装画布的新项目。
pub async fn canvas_create(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let w = util::clamp_round(body.get("w"), 1, 8192, DEFAULT_EDGE) as usize;
    let h = util::clamp_round(body.get("h"), 1, 8192, DEFAULT_EDGE) as usize;
    if let Err(e) = queue::sketch_fit(w, h) {
        return Ok(bad(e));
    }
    let name = new_name(&body);
    let pid = match body.get("project_id").and_then(|v| v.as_i64()) {
        Some(id) => {
            if rproj::by_id(&ctx, id)?.is_none() {
                return Ok(err(404, "项目不存在"));
            }
            id
        }
        None => rproj::create(&ctx, &format!("画布 · {name}"))?,
    };
    let dir = ctx.data.join("projects").join(pid.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败：{e}"))?;
    let safe = util::safe_name(&name);
    let rel = util::rel_path(&[
        "projects".into(),
        pid.to_string(),
        format!("{}_{r4}_{safe}_sketch.png", util::now_ms(), r4 = util::r4()),
    ]);
    // 空白画布 = 全透明的 RGBA；发出去时才拍到白底上（prepare_sketch 那一步在队列里）
    let blank = Rgba::new(w, h);
    imagesvc::write_bytes(&ctx.data.join(&rel), &crate::img::codec::encode_png(&blank)).map_err(AppError::Fail)?;
    let id = rimg::insert_kind(&ctx, pid, &safe, &rel, w as i64, h as i64, "sketch")?;
    rproj::touch(&ctx, pid)?;
    Ok(ok(serde_json::json!({ "project_id": pid, "image_id": id, "name": safe, "w": w, "h": h })))
}

/// 画布详情：画稿地址 + 历史成图。不走 `/api/images/{id}`，那条会替照片判云端僵尸、补派生档。
pub async fn canvas_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "没有这张画稿")) };
    if !img.is_sketch() {
        return Ok(err(404, "这不是画稿"));
    }
    let rows: Vec<Value> = rres::list_for_image(&ctx, iid, 60)?
        .iter()
        .map(|r| dto::result_json(&ctx, r))
        .collect();
    let mut o = Map::new();
    o.insert("image".into(), dto::image_json(&ctx, &img, true));
    o.insert("sketch_url".into(), Value::String(format!("/file/{}", img.orig_path)));
    o.insert("results".into(), Value::Array(rows));
    Ok(ok(Value::Object(o)))
}

/// 存画稿：原地覆写（.part→rename），先解一次确认是能解码的图。
pub async fn sketch_post(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "没有这张画稿")) };
    if !img.is_sketch() {
        return Ok(bad("这不是画稿"));
    }
    let body = body_of(raw).await?;
    let b64 = body.get("b64").and_then(|v| v.as_str()).unwrap_or("");
    let bytes = util::decode_b64(b64);
    // 解不出来就不覆盖：画稿是用户唯一的手感来源，写坏一次等于抹掉他的草稿
    let decoded = crate::img::codec::decode(&bytes)
        .map_err(|_| AppError::bad("画稿不是能解码的 PNG，这次没有覆盖已有画稿"))?;
    if decoded.w == 0 || decoded.h == 0 {
        return Ok(bad("画稿是空的，这次没有覆盖"));
    }
    imagesvc::write_bytes(&ctx.data.join(&img.orig_path), &bytes).map_err(AppError::Fail)?;
    if decoded.w as i64 != img.w || decoded.h as i64 != img.h {
        rimg::set_dims(&ctx, iid, decoded.w as i64, decoded.h as i64)?;
    }
    Ok(ok(serde_json::json!({ "ok": true, "w": decoded.w, "h": decoded.h })))
}

/// 提交一次生成：进同一条云端队列，进度就是这条 results 行。
pub async fn canvas_generate(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "没有这张画稿")) };
    if !img.is_sketch() {
        return Ok(bad("这不是画稿"));
    }
    let body = body_of(raw).await?;
    let settings = body.get("settings").cloned().unwrap_or(Value::Object(Map::new()));
    let prompt = settings.get("prompt").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if prompt.is_empty() {
        return Ok(bad("画布生成只看提示词，先写要生成什么"));
    }
    if !util::file_alive(&ctx.data, &Value::String(img.orig_path.clone())) {
        return Ok(bad("画稿文件不在磁盘上，重画一版再提交"));
    }
    if let Err(e) = queue::sketch_fit(img.w.max(1) as usize, img.h.max(1) as usize) {
        return Ok(bad(e));
    }
    let s = cloud::settings(&ctx);
    if s.base.is_empty() || s.model.is_empty() || s.key.is_empty() {
        return Ok(bad("画布生成要走云端：先到设置 → 云端生成填 base_url、模型名与 API key"));
    }
    let rerun_of = body.get("rerun_of").and_then(|v| v.as_i64());
    let payload = serde_json::json!({ "prompt": prompt, "negative": "", "steps": 0, "cfg": 0, "loras": [], "canvas": true });
    let rid = rres::insert_cloud(&ctx, img.id, img.project_id, &prompt, &payload.to_string(), &s.model, rerun_of)?;
    rres::set_queued(&ctx, rid)?;
    rproj::touch(&ctx, img.project_id)?;
    queue::pump(&ctx).await;
    Ok(ok(serde_json::json!({ "result_id": rid, "image_id": img.id, "queued": true })))
}
