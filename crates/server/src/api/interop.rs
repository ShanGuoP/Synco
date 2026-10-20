//! 双向接口的 HTTP 装配；协议错误包含稳定机器码与本地化文案键。
use crate::{
    error::{AppError, Result},
    models::interop::Create,
    service::interop as svc,
    state::Shared,
};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;
use std::collections::HashMap;
type Params = HashMap<String, String>;
fn reply(result: Result<Value>, status: u16) -> Response {
    match result {
        Ok(v) => (StatusCode::from_u16(status).unwrap(), Json(v)).into_response(),
        Err(e) => {
            let (status, body) = svc::error(&e);
            let mut response = (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response();
            if status == 503 {
                response
                    .headers_mut()
                    .insert("retry-after", "2".parse().unwrap());
            }
            response
        }
    }
}
fn parse(raw: &Bytes) -> Result<Value> {
    serde_json::from_slice(raw).map_err(|_| AppError::bad("srv.interop.bad_request"))
}
fn client(params: &Params) -> &str {
    params.get("client_id").map(String::as_str).unwrap_or("")
}
pub async fn info(State(ctx): State<Shared>) -> Response {
    reply(Ok(ctx.interop().info()), 200)
}
pub async fn inbox(State(ctx): State<Shared>) -> Response {
    reply(svc::inbox(&ctx), 200)
}
pub async fn create(State(ctx): State<Shared>, raw: Bytes) -> Response {
    let result = async {
        let spec: Create = serde_json::from_value(parse(&raw)?)
            .map_err(|_| AppError::bad("srv.interop.bad_request"))?;
        svc::create(&ctx, spec).await
    }
    .await;
    let status = if result
        .as_ref()
        .ok()
        .is_some_and(|v| v["state"] != "importing")
    {
        200
    } else {
        202
    };
    reply(result, status)
}
pub async fn get(
    State(ctx): State<Shared>,
    Path(task): Path<String>,
    Query(q): Query<Params>,
) -> Response {
    reply(svc::task(&ctx, client(&q), &task), 200)
}
pub async fn list(State(ctx): State<Shared>, Query(q): Query<Params>) -> Response {
    let result = (|| {
        if q.get("delivery").map(String::as_str) != Some("pending") {
            return Err(AppError::bad("srv.interop.bad_request"));
        }
        let limit = q
            .get("limit")
            .map(|v| v.parse::<i64>())
            .transpose()
            .map_err(|_| AppError::bad("srv.interop.bad_request"))?
            .unwrap_or(50);
        svc::pending(&ctx, client(&q), limit)
    })();
    reply(result, 200)
}
pub async fn ack(State(ctx): State<Shared>, Path(task): Path<String>, raw: Bytes) -> Response {
    let result = (|| {
        let b = parse(&raw)?;
        svc::ack(
            &ctx,
            b["client_id"].as_str().unwrap_or(""),
            &task,
            b["result_id"].as_str(),
            b["sha256"].as_str(),
        )
    })();
    reply(result, 200)
}
pub async fn opened(State(ctx): State<Shared>, Path(task): Path<String>, raw: Bytes) -> Response {
    let result = (|| {
        let b = parse(&raw)?;
        svc::opened(&ctx, b["client_id"].as_str().unwrap_or(""), &task)
    })();
    reply(result, 200)
}
pub async fn cancel(State(ctx): State<Shared>, Path(task): Path<String>, raw: Bytes) -> Response {
    let result = (|| {
        let b = parse(&raw)?;
        svc::cancel(&ctx, b["client_id"].as_str().unwrap_or(""), &task)
    })();
    reply(result, 200)
}
pub async fn complete(State(ctx): State<Shared>, Path(task): Path<String>, raw: Bytes) -> Response {
    let result = async {
        let b = parse(&raw)?;
        svc::complete(
            &ctx,
            b["client_id"].as_str().unwrap_or(""),
            &task,
            b["result_id"].as_i64(),
        )
        .await
    }
    .await;
    reply(result, 202)
}
