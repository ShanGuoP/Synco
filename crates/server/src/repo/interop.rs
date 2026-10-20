//! 交接任务、来源项目映射与回执；所有状态变更使用条件更新。
use crate::{
    error::{AppError, Result},
    models::interop::Create,
    repo,
    state::Ctx,
};
use rusqlite::OptionalExtension;
use serde_json::Value;
pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS interop_projects(client_id TEXT NOT NULL,source_project_id TEXT NOT NULL,project_id INTEGER NOT NULL,PRIMARY KEY(client_id,source_project_id));
CREATE TABLE IF NOT EXISTS interop_tasks(
 id INTEGER PRIMARY KEY AUTOINCREMENT,client_id TEXT NOT NULL,task_id TEXT NOT NULL,request_json TEXT NOT NULL,
 project_id INTEGER NOT NULL,image_id INTEGER,state TEXT NOT NULL DEFAULT 'importing',opened INTEGER NOT NULL DEFAULT 0,
 acked INTEGER NOT NULL DEFAULT 0,result_json TEXT,last_error TEXT,created_at INTEGER NOT NULL,
 UNIQUE(client_id,task_id));
CREATE INDEX IF NOT EXISTS interop_tasks_image ON interop_tasks(image_id);
"#;
pub fn get(ctx: &Ctx, client: &str, task: &str) -> Result<Value> {
    repo::one(
        ctx,
        "SELECT * FROM interop_tasks WHERE client_id=? AND task_id=?",
        &[repo::s(client), repo::s(task)],
    )?
    .ok_or_else(|| AppError::coded(404, "srv.interop.task_missing"))
}
pub fn by_id(ctx: &Ctx, id: i64) -> Result<Value> {
    repo::one(
        ctx,
        "SELECT * FROM interop_tasks WHERE id=?",
        &[repo::i(id)],
    )?
    .ok_or_else(|| AppError::coded(404, "srv.interop.task_missing"))
}
pub fn register(ctx: &Ctx, spec: &Create) -> Result<(Value, bool)> {
    let raw = serde_json::to_string(spec).map_err(|e| AppError::Fail(e.to_string()))?;
    let mut db = ctx.db();
    let tx = db.transaction()?;
    if let Some(row) = repo::all_on(
        &tx,
        "SELECT * FROM interop_tasks WHERE client_id=? AND task_id=?",
        &[repo::s(&spec.client_id), repo::s(&spec.task_id)],
    )?
    .pop()
    {
        if row["request_json"] != raw {
            return Err(AppError::coded(409, "srv.interop.task_conflict"));
        }
        return Ok((row, false));
    }
    let waiting: i64 = tx.query_row(
        "SELECT count(*) FROM interop_tasks WHERE state IN ('importing','rendering')",
        [],
        |r| r.get(0),
    )?;
    if waiting >= 200 {
        return Err(AppError::coded(503, "srv.interop.task_busy"));
    }
    let pid=if let Some(row)=repo::all_on(&tx,"SELECT p.id FROM interop_projects x JOIN projects p ON p.id=x.project_id WHERE client_id=? AND source_project_id=?",&[repo::s(&spec.client_id),repo::s(&spec.source_project_id)])?.pop(){row["id"].as_i64().unwrap()}else{
        tx.execute("INSERT INTO projects(name) VALUES(?)",[&spec.project_name])?;let pid=tx.last_insert_rowid();
        tx.execute("INSERT OR REPLACE INTO interop_projects(client_id,source_project_id,project_id) VALUES(?,?,?)",rusqlite::params![spec.client_id,spec.source_project_id,pid])?;pid
    };
    tx.execute("INSERT INTO interop_tasks(client_id,task_id,request_json,project_id,created_at) VALUES(?,?,?,?,?)",rusqlite::params![spec.client_id,spec.task_id,raw,pid,crate::util::now_ms() as i64])?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    drop(db);
    Ok((by_id(ctx, id)?, true))
}
pub fn imports(ctx: &Ctx) -> Result<Vec<Value>> {
    repo::all(
        ctx,
        "SELECT * FROM interop_tasks WHERE state='importing' ORDER BY id LIMIT 1",
        &[],
    )
}
pub fn attach(ctx: &Ctx, id: i64, name: &str, path: &str, w: i64, h: i64) -> Result<Option<i64>> {
    let mut db = ctx.db();
    let tx = db.transaction()?;
    let Some(row) = repo::all_on(
        &tx,
        "SELECT project_id FROM interop_tasks WHERE id=? AND state='importing'",
        &[repo::i(id)],
    )?
    .pop() else {
        return Ok(None);
    };
    tx.execute(
        "INSERT INTO images(project_id,name,orig_path,w,h) VALUES(?,?,?,?,?)",
        rusqlite::params![row["project_id"].as_i64().unwrap(), name, path, w, h],
    )?;
    let image = tx.last_insert_rowid();
    tx.execute(
        "UPDATE interop_tasks SET image_id=?,state='editing',last_error=NULL WHERE id=?",
        [image, id],
    )?;
    tx.commit()?;
    Ok(Some(image))
}
pub fn fail(ctx: &Ctx, id: i64, from: &str, to: &str, error: &Value) -> Result<()> {
    repo::run(
        ctx,
        "UPDATE interop_tasks SET state=?,last_error=? WHERE id=? AND state=?",
        &[
            repo::s(to),
            repo::s(&error.to_string()),
            repo::i(id),
            repo::s(from),
        ],
    )?;
    Ok(())
}
pub fn start_render(ctx: &Ctx, id: i64) -> Result<Option<String>> {
    let mut db = ctx.db();
    let tx = db.transaction()?;
    let Some(row) = repo::all_on(
        &tx,
        "SELECT image_id FROM interop_tasks WHERE id=? AND state='editing'",
        &[repo::i(id)],
    )?
    .pop() else {
        return Ok(None);
    };
    let image = row["image_id"].as_i64().unwrap();
    let ops = tx
        .query_row(
            "SELECT ops FROM image_adjust WHERE image_id=?",
            [image],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_else(|| photoedit_core::default_json().to_string());
    tx.execute(
        "UPDATE interop_tasks SET state='rendering',last_error=NULL WHERE id=?",
        [id],
    )?;
    tx.commit()?;
    Ok(Some(ops))
}
pub fn ready(ctx: &Ctx, id: i64, result: &Value) -> Result<bool> {
    Ok(repo::run(ctx,"UPDATE interop_tasks SET state='ready',result_json=?,last_error=NULL WHERE id=? AND state='rendering'",&[repo::s(&result.to_string()),repo::i(id)])?==1)
}
pub fn recover(ctx: &Ctx) -> Result<()> {
    repo::run(
        ctx,
        "UPDATE interop_tasks SET state='editing' WHERE state='rendering'",
        &[],
    )?;
    Ok(())
}
pub fn list(ctx: &Ctx, client: &str, limit: i64) -> Result<Vec<Value>> {
    repo::all(ctx,"SELECT * FROM interop_tasks WHERE client_id=? AND acked=0 AND state IN ('ready','failed','cancelled') ORDER BY id LIMIT ?",&[repo::s(client),repo::i(limit)])
}
pub fn inbox(ctx: &Ctx) -> Result<Vec<Value>> {
    repo::all(
        ctx,
        "SELECT * FROM interop_tasks WHERE state IN ('editing','rendering','ready') ORDER BY id",
        &[],
    )
}
pub fn all_results(ctx: &Ctx) -> Result<Vec<Value>> {
    repo::all(
        ctx,
        "SELECT result_json FROM interop_tasks WHERE result_json IS NOT NULL",
        &[],
    )
}
pub fn opened(ctx: &Ctx, id: i64) -> Result<()> {
    repo::run(
        ctx,
        "UPDATE interop_tasks SET opened=1 WHERE id=?",
        &[repo::i(id)],
    )?;
    Ok(())
}
pub fn ack(ctx: &Ctx, id: i64, result: Option<&str>, hash: Option<&str>) -> Result<()> {
    let mut db = ctx.db();
    let tx = db.transaction()?;
    let row = repo::all_on(
        &tx,
        "SELECT * FROM interop_tasks WHERE id=?",
        &[repo::i(id)],
    )?
    .pop()
    .ok_or_else(|| AppError::coded(404, "srv.interop.task_missing"))?;
    let state = row["state"].as_str().unwrap();
    if ["ready", "delivered"].contains(&state) {
        let saved: Value = serde_json::from_str(row["result_json"].as_str().unwrap_or("{}"))
            .map_err(|e| AppError::Fail(e.to_string()))?;
        if result.is_none()
            || hash.is_none()
            || result != saved["result_id"].as_str()
            || hash != saved["output"]["sha256"].as_str()
        {
            return Err(AppError::coded(409, "srv.interop.result_conflict"));
        }
        tx.execute(
            "UPDATE interop_tasks SET state='delivered',acked=1 WHERE id=?",
            [id],
        )?;
    } else if ["failed", "cancelled"].contains(&state) {
        if result.is_some() || hash.is_some() {
            return Err(AppError::coded(409, "srv.interop.result_conflict"));
        }
        tx.execute("UPDATE interop_tasks SET acked=1 WHERE id=?", [id])?;
    } else {
        return Err(AppError::coded(409, "srv.interop.task_busy"));
    }
    tx.commit()?;
    Ok(())
}
pub fn cancel(ctx: &Ctx, id: i64) -> Result<()> {
    let n=repo::run(ctx,"UPDATE interop_tasks SET state='cancelled',acked=0 WHERE id=? AND state IN ('importing','editing','ready')",&[repo::i(id)])?;
    if n == 0 {
        return Err(AppError::coded(409, "srv.interop.task_busy"));
    }
    Ok(())
}
