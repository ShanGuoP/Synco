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


/// `?kind=preset`（默认）列参数预设，`?kind=phrase` 列提示词短语
pub async fn presets_list(State(ctx): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Result<Response> {
    let kind = rpre::norm_kind(q.get("kind").map(|s| s.as_str()));
    let rows = rpre::list(&ctx, q.get("project_id").and_then(|s| s.parse::<i64>().ok()), &kind)?;
    Ok(ok(Value::Array(rows.iter().map(dto::preset_json).collect())))
}

pub async fn presets_add(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let kind = rpre::norm_kind(body.get("kind").and_then(|v| v.as_str()));
    let f = dto::preset_fields(&body);
    if f.name.is_empty() {
        return Ok(bad(if kind == rpre::KIND_PHRASE { "srv.preset.needPhrase" } else { "srv.preset.needName" }));
    }
    // 短语只用到 prompt 那一格；空的"一句话"没地方生效，收进来只会变成一条打不响的胶囊
    if kind == rpre::KIND_PHRASE && f.prompt.trim().is_empty() {
        return Ok(bad("srv.preset.phraseEmpty"));
    }
    let pid = body.get("project_id").and_then(|x| x.as_i64());
    if rpre::name_taken(&ctx, &f.name, &kind, None)? {
        return Ok(bad(if kind == rpre::KIND_PHRASE { "srv.preset.dupPhrase" } else { "srv.preset.dupName" }));
    }
    let id = rpre::insert(&ctx, &f.name, pid, &kind, &f)?;
    Ok(ok(dto::preset_json(&rpre::by_id(&ctx, id)?.unwrap_or(Value::Null))))
}

pub async fn preset_delete(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let pid = path_id(&id)?;
    if !rpre::exists(&ctx, pid)? {
        return Ok(err(404, "srv.preset.missing"));
    }
    rpre::delete(&ctx, pid)?;
    Ok(ok(serde_json::json!({ "ok": true })))
}

pub async fn preset_update(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let pid = path_id(&id)?;
    let Some(cur) = rpre::by_id(&ctx, pid)? else { return Ok(err(404, "srv.preset.missing")) };
    // 桶跟着库里这一行走：改名/改内容不会把一条短语悄悄变成一份预设
    let kind = rpre::norm_kind(cur.get("kind").and_then(|v| v.as_str()));
    let f = dto::preset_fields(&body);
    if f.name.is_empty() {
        return Ok(bad(if kind == rpre::KIND_PHRASE { "srv.preset.needPhrase" } else { "srv.preset.needName" }));
    }
    if kind == rpre::KIND_PHRASE && f.prompt.trim().is_empty() {
        return Ok(bad("srv.preset.phraseEmpty"));
    }
    // 改名与换作用域以前完全不查名，于是一条全局 "X" 和项目里的 "X" 能同时存在，列表显示成两条
    if rpre::name_taken(&ctx, &f.name, &kind, Some(pid))? {
        return Ok(bad(if kind == rpre::KIND_PHRASE { "srv.preset.dupPhrase" } else { "srv.preset.dupName" }));
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
