//! 双向交接用例；持久化声明先于接受响应，输入与返回文件均由 Synco 独立持有。
pub mod discovery;
mod files;
use crate::{
    error::{AppError, Result},
    models::interop::{client_valid, Create},
    repo::interop as db,
    state::{Ctx, Shared},
    util,
};
use serde_json::{json, Value};
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub fn shutdown(ctx: &Ctx) {
    ctx.interop().unpublish();
}

pub fn error(e: &AppError) -> (u16, Value) {
    let (status, key) = match e {
        AppError::Msg { status, code, .. } => (*status, code.to_string()),
        _ => (500, "srv.interop.io_error".into()),
    };
    let code = if key.split('.').nth(1) == Some("interop") {
        key.rsplit('.').next().unwrap_or("processing_error")
    } else {
        "processing_error"
    };
    (
        status,
        json!({"error":{"code":code,"message":e.text(),"message_key":key,"retryable":status>=500},"code":key}),
    )
}
pub fn public(ctx: &Ctx, row: &Value) -> Value {
    let spec: Value =
        serde_json::from_str(row["request_json"].as_str().unwrap_or("{}")).unwrap_or_default();
    let mut out = json!({"task_id":row["task_id"],"client_id":row["client_id"],"source_project_id":spec["source_project_id"],"source_image_id":spec["source_image_id"],"project_name":spec["project_name"],"state":row["state"],"project_id":row["project_id"],"image_id":row["image_id"],"open_in_editor":spec["open_in_editor"],"opened":row["opened"].as_i64()==Some(1)});
    out["image_name"] = json!(
        std::path::Path::new(spec["input"]["path"].as_str().unwrap_or(""))
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    );
    if let Some(result) = row["result_json"]
        .as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
    {
        for (k, v) in result.as_object().into_iter().flatten() {
            if k != "relative_path" {
                out[k] = v.clone();
            }
        }
        if let Some(rel) = result["relative_path"].as_str() {
            out["output"]["path"] = json!(ctx.data.join(rel).display().to_string());
        }
    }
    out["last_error"] = row["last_error"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    out
}
pub fn client(client: &str) -> Result<()> {
    if !client_valid(client) {
        return Err(AppError::bad("srv.interop.bad_request"));
    }
    Ok(())
}
pub async fn create(ctx: &Shared, spec: Create) -> Result<Value> {
    spec.validate()?;
    let c = ctx.clone();
    util::library_work(ctx.clone(), move || {
        let (row, _) = db::register(&c, &spec)?;
        Ok(public(&c, &row))
    })
    .await
}
pub fn task(ctx: &Ctx, client_id: &str, id: &str) -> Result<Value> {
    client(client_id)?;
    Ok(public(ctx, &db::get(ctx, client_id, id)?))
}
pub fn pending(ctx: &Ctx, client_id: &str, limit: i64) -> Result<Value> {
    client(client_id)?;
    if !(1..=200).contains(&limit) {
        return Err(AppError::bad("srv.interop.bad_request"));
    }
    Ok(
        json!({"tasks":db::list(ctx,client_id,limit)?.iter().map(|r|public(ctx,r)).collect::<Vec<_>>()}),
    )
}
pub fn inbox(ctx: &Ctx) -> Result<Value> {
    Ok(json!({"tasks":db::inbox(ctx)?.iter().map(|r|public(ctx,r)).collect::<Vec<_>>()}))
}
pub fn ack(
    ctx: &Ctx,
    client_id: &str,
    id: &str,
    result: Option<&str>,
    hash: Option<&str>,
) -> Result<Value> {
    client(client_id)?;
    let row = db::get(ctx, client_id, id)?;
    db::ack(ctx, row["id"].as_i64().unwrap(), result, hash)?;
    Ok(json!({"ok":true}))
}
pub fn opened(ctx: &Ctx, client_id: &str, id: &str) -> Result<Value> {
    client(client_id)?;
    let row = db::get(ctx, client_id, id)?;
    db::opened(ctx, row["id"].as_i64().unwrap())?;
    Ok(json!({"ok":true}))
}
pub fn cancel(ctx: &Ctx, client_id: &str, id: &str) -> Result<Value> {
    client(client_id)?;
    let row = db::get(ctx, client_id, id)?;
    if row["state"] != "cancelled" {
        db::cancel(ctx, row["id"].as_i64().unwrap())?;
    }
    task(ctx, client_id, id)
}
pub async fn complete(
    ctx: &Shared,
    client_id: &str,
    task_id: &str,
    result_id: Option<i64>,
) -> Result<Value> {
    client(client_id)?;
    let row = db::get(ctx, client_id, task_id)?;
    if let Some(result) = result_id {
        let r = crate::repo::results::by_id(ctx, result)?
            .ok_or_else(|| AppError::bad("srv.interop.bad_request"))?;
        if r.image_id != row["image_id"].as_i64().unwrap_or(0) || r.status != "done" {
            return Err(AppError::bad("srv.interop.bad_request"));
        }
    }
    let id = row["id"].as_i64().unwrap();
    if let Some(ops) = db::start_render(ctx, id)? {
        let (prepared, wait_prepared) = tokio::sync::oneshot::channel();
        let c = ctx.clone();
        let image = row["image_id"].as_i64().unwrap();
        tokio::spawn(async move {
            let _scope = c.work().enter().await;
            let cc = c.clone();
            let result = util::image_work(c.clone(), 0, move || {
                files::export(&cc, id, image, &ops, result_id, prepared)
            })
            .await;
            if let Err(e) = result {
                let _ = db::fail(&c, id, "rendering", "editing", &error(&e).1["error"]);
            }
        });
        // 等待可变依赖被复制；客户端断开也不会丢弃已经交给后台的工作。
        let _ = wait_prepared.await;
    } else if !["rendering", "ready", "delivered"].contains(&row["state"].as_str().unwrap_or("")) {
        return Err(AppError::coded(409, "srv.interop.task_busy"));
    }
    Ok(public(ctx, &db::by_id(ctx, id)?))
}
pub async fn start(ctx: &Shared, port: u16) -> Result<()> {
    let c = ctx.clone();
    util::library_work(ctx.clone(), move || {
        db::recover(&c)?;
        files::recover(&c)?;
        c.interop().publish(&c, port)
    })
    .await?;
    let weak = std::sync::Arc::downgrade(ctx);
    tokio::spawn(async move {
        loop {
            let Some(c) = weak.upgrade() else { break };
            let scope = c.work().enter().await;
            if let Ok(rows) = db::imports(&c) {
                if let Some(row) = rows.into_iter().next() {
                    let id = row["id"].as_i64().unwrap();
                    let cc = c.clone();
                    let result =
                        util::image_work(c.clone(), 0, move || files::import(&cc, &row)).await;
                    if let Err(e) = result {
                        let mut reason = error(&e).1["error"].clone();
                        reason["retryable"] = json!(false);
                        let _ = db::fail(&c, id, "importing", "failed", &reason);
                    }
                }
            }
            drop(scope);
            drop(c);
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    });
    Ok(())
}
