//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{body_of, ok};
use crate::error::{AppError, Result};
use crate::service::backend;
use serde_json::Value;
use crate::state::Shared;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;


pub async fn backends_get(State(ctx): State<Shared>) -> Result<Response> {
    Ok(ok(serde_json::json!({ "active": backend::active_url(&ctx), "custom": backend::list_custom(&ctx) })))
}

pub async fn backends_scan(State(ctx): State<Shared>) -> Response {
    ok(serde_json::json!({ "active": backend::active_url(&ctx), "found": backend::scan(&ctx).await }))
}

pub async fn backends_add(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let url = backend::add_custom(
        &ctx,
        body.get("url").and_then(|v| v.as_str()).unwrap_or(""),
        body.get("label").and_then(|v| v.as_str()),
    )
    ?;
    let p = backend::probe(&ctx, &url).await;
    Ok(ok(serde_json::json!({ "url": url, "probe": p })))
}

pub async fn backends_remove(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let active = backend::remove_custom(&ctx, body.get("url").and_then(|v| v.as_str()).unwrap_or(""))
        ?;
    Ok(ok(serde_json::json!({ "ok": true, "active": active })))
}

pub async fn backends_select(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let url = backend::norm_url(body.get("url").and_then(|v| v.as_str()).unwrap_or(""))?;
    let p = backend::probe(&ctx, &url).await;
    if !p.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        // 探活那条结论自己就带钥匙，原样升上去，不再包一句"探活失败"
        return match p.get("error").and_then(|v| v.as_str()) {
            Some(code) => Err(AppError::verdict(400, code.to_string(), p.get("error_args").cloned().unwrap_or(Value::Null))),
            None => Err(AppError::bad("srv.backend.noAnswer")),
        };
    }
    let active = backend::set_active(&ctx, &url)?;
    Ok(ok(serde_json::json!({ "active": active, "probe": p })))
}

// ---------------------------------------------------------------- 云端
