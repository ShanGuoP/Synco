//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{bad, batch_limit, body_of, err, ok, path_id, save_batch};
use crate::error::Result;
use crate::models::dto;
use crate::repo;
use crate::repo::{images as rimg, projects as rproj, results as rres};
use crate::state::Shared;
use crate::service::refs;
use crate::util;
use axum::body::Bytes;
use axum::extract::{Path as APath, State};
use axum::response::Response;
use serde_json::{Map, Value};

pub async fn projects_list(State(ctx): State<Shared>) -> Result<Response> {
    let out: Vec<Value> = rproj::list(&ctx)?
        .into_iter()
        .map(|mut r| {
            let o = r.as_object_mut().unwrap();
            // cover_url 的语义和 Node 一致（第一张原图），封面用小档是新增的 cover_thumb_url
            for (key, col) in [("cover_url", "cover"), ("cover_thumb_url", "cover_thumb")] {
                let v = o
                    .get(col)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| Value::String(format!("/file/{s}")))
                    .unwrap_or(Value::Null);
                o.insert(key.into(), v);
            }
            r
        })
        .collect();
    Ok(ok(Value::Array(out)))
}

/// 项目名只 trim 不 reject：导入卡允许留空，空名按"未命名项目"落库，
/// 但纯空格不能当名字存进去（早期就这么写过，首页会出现看不见的项目）
fn project_name(body: &Value) -> String {
    let raw = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
    util::clip(if raw.is_empty() { "未命名项目" } else { raw }, 60)
}

pub async fn projects_create(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let name = project_name(&body);
    // 张数在建项目之前先判：反过来先建后判会留下一个空项目挂在库里
    if let Some(files) = body.get("files").and_then(|v| v.as_array()) {
        if let Some(msg) = batch_limit(files) {
            return Ok(bad(msg));
        }
    }
    let pid = rproj::create(&ctx, &name)?;
    let mut ids: Vec<i64> = Vec::new();
    if let Some(files) = body.get("files").and_then(|v| v.as_array()) {
        ids = save_batch(&ctx, pid, files)?;
    }
    rproj::touch(&ctx, pid)?;
    Ok(ok(serde_json::json!({ "id": pid, "name": name, "image_ids": ids })))
}

/// 改名：不动盘上任何一个文件。空名与纯空格拒绝，超长裁到 60
pub async fn project_rename(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let pid = path_id(&id)?;
    let body = body_of(raw).await?;
    let raw_name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
    if raw_name.is_empty() {
        return Ok(bad("项目名不能是空的"));
    }
    let name = util::clip(raw_name, 60);
    if rproj::rename(&ctx, pid, &name)? == 0 {
        return Ok(err(404, "no project"));
    }
    Ok(ok(serde_json::json!({ "ok": true, "name": name })))
}

pub async fn project_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let pid = path_id(&id)?;
    let Some(proj) = rproj::by_id(&ctx, pid)? else { return Ok(err(404, "no project")) };
    let out: Vec<Value> = rimg::list_for_project_rows(&ctx, pid)?
        .iter()
        .map(|row| dto::image_row_json(&ctx, row))
        .collect();
    Ok(ok(serde_json::json!({ "project": proj, "images": out })))
}

pub async fn project_delete(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let pid = path_id(&id)?;
    // 先把这一项目名下的文件名收齐（行一删就查不到盘上有谁了），删完行再动磁盘
    let mut ids: Vec<i64> = Vec::new();
    let mut rels: Vec<String> = Vec::new();
    for i in rimg::list_for_project(&ctx, pid)? {
        ids.push(i.id);
        if !i.orig_path.is_empty() {
            rels.push(i.orig_path.clone());
        }
        if let Some(m) = i.mask_path.clone().filter(|s| !s.is_empty()) {
            rels.push(m);
        }
    }
    rels.extend(rres::list_paths(&ctx, "project_id=?", repo::i(pid))?);
    rimg::delete_for_project(&ctx, pid)?;
    rres::delete_for_project(&ctx, pid)?;
    rproj::delete(&ctx, pid)?;
    // 画布的参考图槽位是存在设置里的：项目没了，那些键与文件也要跟着没，不然只涨不落
    for iid in ids {
        refs::clear(&ctx, pid, iid)?;
    }
    // 中途断掉留下的是没人认领的文件，而不是指向空气的记录——后者会在界面上长成碎图
    for rel in &rels {
        if let Some(p) = util::data_file(&ctx.data, rel) {
            let _ = std::fs::remove_file(p);
        }
    }
    let _ = std::fs::remove_dir_all(ctx.data.join("projects").join(pid.to_string()));
    Ok(ok(serde_json::json!({ "ok": true })))
}

pub async fn images_add(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let pid = path_id(&id)?;
    let body = body_of(raw).await?;
    let mut ids: Vec<Value> = Vec::new();
    if let Some(files) = body.get("files").and_then(|v| v.as_array()) {
        ids = save_batch(&ctx, pid, files)?.into_iter().map(Value::from).collect();
    }
    rproj::touch(&ctx, pid)?;
    Ok(ok(serde_json::json!({ "ids": ids })))
}

pub async fn project_settings_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let Some(row) = rproj::by_id(&ctx, path_id(&id)?)? else { return Ok(err(404, "no project")) };
    let raw = row.get("settings_json").and_then(|v| v.as_str()).unwrap_or("{}");
    let parsed = serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::Object(Map::new()));
    Ok(ok(parsed))
}

pub async fn project_settings_set(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let pid = path_id(&id)?;
    if rproj::set_settings(&ctx, pid, &body.to_string())? == 0 {
        return Ok(err(404, "no project"));
    }
    Ok(ok(serde_json::json!({ "ok": true })))
}
