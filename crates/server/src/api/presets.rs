//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{bad, body_of, err, ok, path_id};
use crate::error::Result;
use crate::models::dto;
use crate::repo::presets as rpre;
use crate::state::Shared;
use axum::body::Bytes;
use axum::extract::{Path as APath, Query, State};
use axum::response::Response;
use serde_json::Value;
use std::collections::HashMap;

pub async fn presets_list(State(ctx): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Result<Response> {
    let rows = rpre::list(&ctx, q.get("project_id").and_then(|s| s.parse::<i64>().ok()))?;
    Ok(ok(Value::Array(rows.iter().map(dto::preset_json).collect())))
}

pub async fn presets_add(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let f = dto::preset_fields(&body);
    if f.name.is_empty() {
        return Ok(bad("给预设起个名字"));
    }
    let pid = body.get("project_id").and_then(|x| x.as_i64());
    if rpre::name_taken(&ctx, &f.name, pid)? {
        return Ok(bad("已经有同名预设了"));
    }
    let id = rpre::insert(&ctx, &f.name, pid, &f)?;
    Ok(ok(dto::preset_json(&rpre::by_id(&ctx, id)?.unwrap_or(Value::Null))))
}

pub async fn preset_delete(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let pid = path_id(&id)?;
    if !rpre::exists(&ctx, pid)? {
        return Ok(err(404, "预设不存在"));
    }
    rpre::delete(&ctx, pid)?;
    Ok(ok(serde_json::json!({ "ok": true })))
}

pub async fn preset_update(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let pid = path_id(&id)?;
    let Some(cur) = rpre::by_id(&ctx, pid)? else { return Ok(err(404, "预设不存在")) };
    let f = dto::preset_fields(&body);
    if f.name.is_empty() {
        return Ok(bad("给预设起个名字"));
    }
    // scope: 'global' 升到全局，数字串则收到某个项目内，没带就维持原作用域
    let project_id = match &body.get("scope").cloned().unwrap_or(Value::Null) {
        Value::String(s) if s == "global" => None,
        Value::Null => cur.get("project_id").and_then(|v| v.as_i64()),
        other => other.as_str().and_then(|s| s.parse::<i64>().ok()).or_else(|| other.as_i64()),
    };
    rpre::update(&ctx, pid, project_id, &f)?;
    Ok(ok(dto::preset_json(&rpre::by_id(&ctx, pid)?.unwrap_or(Value::Null))))
}
