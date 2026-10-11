//! 云端设置与一次请求。
//! M3 起：`edit` 不再收前端裁好的图和蒙版，也不再回传缝合结果——
//! 裁切、调云端、缝合、落盘全在服务端一次做完，进度靠 `results` 行的状态机。

use super::common::{bad, bad_raw, body_of, err, ok};
use crate::error::Result;
use crate::repo::images as rimg;
use crate::service::cloud;
use crate::state::Shared;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde_json::{Map, Value};

pub async fn cloud_get(State(ctx): State<Shared>) -> Response {
    ok(cloud::public_settings(&ctx))
}

pub async fn cloud_set(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    cloud::save(&ctx, &body)?;
    Ok(ok(cloud::public_settings(&ctx)))
}

pub async fn cloud_test(State(ctx): State<Shared>) -> Response {
    ok(cloud::probe(&ctx).await)
}

fn ids_of(body: &Value, key: &str) -> Vec<i64> {
    body.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
        .unwrap_or_default()
}

/// 一次裁切区重绘：等价于"队列里只有一张"
pub async fn cloud_edit(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let iid = body.get("image_id").and_then(|v| v.as_i64()).unwrap_or(-1);
    if rimg::by_id(&ctx, iid)?.is_none() {
        return Ok(err(404, "srv.canvas.noImage"));
    }
    let settings = body.get("settings").cloned().unwrap_or(Value::Object(Map::new()));
    let rerun = body.get("rerun_of").and_then(|v| v.as_i64());
    let ctx2 = ctx.clone();
    let (rows, skipped) = crate::util::blocking(move || crate::service::queue::enqueue(&ctx2, &[iid], &settings, rerun)).await?;
    if rows.is_empty() {
        // 理由可能是队列带上来的一句原文（还没交钥匙），也可能根本没有理由：后者才用钥匙
        match skipped.first().and_then(|v| v.get("reason")).and_then(|v| v.as_str()) {
            Some(r) => return Ok(bad_raw(r)),
            None => return Ok(bad("srv.cloud.cantSubmit")),
        }
    }
    crate::service::queue::pump(&ctx).await;
    Ok(ok(serde_json::json!({ "result_id": rows[0].0, "image_id": rows[0].1, "queued": true })))
}

/// 批量提交：一次建 N 行 queued，调度器按 concurrency 排着跑，关页面继续
pub async fn queue_post(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let ids_in = ids_of(&body, "image_ids");
    if ids_in.is_empty() {
        return Ok(bad("srv.cloud.nothing"));
    }
    let settings = body.get("settings").cloned().unwrap_or(Value::Object(Map::new()));
    let prompt = settings.get("prompt").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if prompt.is_empty() {
        return Ok(bad("srv.cloud.promptEmpty"));
    }
    let rerun = body.get("rerun_of").and_then(|v| v.as_i64());
    let ctx2 = ctx.clone();
    let (rows, skipped) = crate::util::blocking(move || crate::service::queue::enqueue(&ctx2, &ids_in, &settings, rerun)).await?;
    crate::service::queue::pump(&ctx).await;
    // 与 /api/run 同一个形状：每张都要知道"行 id + 图 id"，前端才挂得上轮询
    let results: Vec<Value> = rows.iter().map(|(rid, iid)| serde_json::json!({ "result_id": rid, "image_id": iid })).collect();
    Ok(ok(serde_json::json!({ "results": results, "skipped": skipped, "queued": results.len() })))
}

pub async fn queue_get(State(ctx): State<Shared>) -> Result<Response> {
    Ok(ok(crate::service::queue::snapshot(&ctx).await?))
}
