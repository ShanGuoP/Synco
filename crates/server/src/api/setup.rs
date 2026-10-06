//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{body_of, ok};
use crate::error::{AppError, Result};
use crate::service::{backend, setup};
use crate::state::Shared;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde_json::{Map, Value};
use std::collections::HashSet;


pub async fn setup_get(State(ctx): State<Shared>) -> Result<Response> {
    let root = setup::root_setting(&ctx);
    let d = setup::detect(&ctx, &root);
    let mut classes: Vec<String> = Vec::new();
    for pack in &setup::PACKS {
        for node in pack.3 {
            classes.push((*node).to_string());
        }
    }
    for extra in ["InpaintCropImproved", "QwenImage21Cache", "TextEncodeQwenImage21"] {
        classes.push(extra.to_string());
    }
    let mut seen = HashSet::new();
    classes.retain(|c| seen.insert(c.clone()));
    let nodes = if root.is_empty() { Value::Object(Map::new()) } else { setup::probe_nodes(&ctx, &backend::active_url(&ctx), &classes).await };
    Ok(ok(serde_json::json!({
        "root": root, "backend": backend::active_url(&ctx), "detect": d, "nodes": nodes, "progress": setup::progress(&ctx)
    })))
}

pub async fn setup_progress(State(ctx): State<Shared>) -> Response {
    ok(serde_json::json!({ "progress": setup::progress(&ctx) }))
}

pub async fn setup_root(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let root = setup::set_root(&ctx, body.get("path").and_then(|v| v.as_str()).unwrap_or(""));
    Ok(ok(serde_json::json!({ "root": root, "detect": setup::detect(&ctx, &root) })))
}

pub async fn setup_script(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let root = match body.get("path").and_then(|v| v.as_str()) {
        Some(p) => setup::neat_root(p),
        None => setup::root_setting(&ctx),
    };
    let proxy = body.get("proxy").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let r = setup::write_script(&ctx, &root, &proxy).map_err(|e| AppError::bad(e))?;
    let manifest = r.get("manifest").cloned().unwrap_or(Value::Null);
    Ok(ok(serde_json::json!({
        "dir": r["dir"], "bat": r["bat"], "ps1": r["ps1"],
        "packs": manifest.get("packs").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
        "models": manifest.get("models").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)
    })))
}

pub async fn setup_verify(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let root = match body.get("path").and_then(|v| v.as_str()) {
        Some(p) => setup::neat_root(p),
        None => setup::root_setting(&ctx),
    };
    // 逐个算 22GB 的指纹要十几秒，放去阻塞线程池，别把 tokio 的工作线程占死
    let ctx2 = ctx.clone();
    let rows = tokio::task::spawn_blocking(move || setup::verify(&ctx2, &root)).await.unwrap_or_default();
    Ok(ok(serde_json::json!({ "rows": rows })))
}

// ---------------------------------------------------------------- 后端
