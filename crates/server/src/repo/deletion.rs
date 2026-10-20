//! 删除用例的跨表事务与持久化文件清理清单；事务失败不转移文件归属。
use crate::{error::Result, repo, state::Ctx};
use serde_json::{json, Value};

pub fn delete_project(ctx: &Ctx, id: i64) -> Result<Vec<i64>> { remove(ctx, id, true) }
pub fn delete_image(ctx: &Ctx, id: i64) -> Result<Vec<i64>> { remove(ctx, id, false) }

fn remove(ctx: &Ctx, id: i64, project: bool) -> Result<Vec<i64>> {
    let mut db = ctx.db();
    let tx = db.transaction()?;
    let protected:i64=tx.query_row(if project {"SELECT count(*) FROM interop_tasks WHERE project_id=? AND state IN ('importing','editing','rendering','ready')"} else {"SELECT count(*) FROM interop_tasks WHERE image_id=? AND state IN ('importing','editing','rendering','ready')"},[id],|r|r.get(0))?;
    if protected>0 {return Err(crate::error::AppError::coded(409,"srv.interop.pending_delivery"));}
    let images = repo::all_on(&tx, if project { "SELECT * FROM images WHERE project_id=?" } else { "SELECT * FROM images WHERE id=?" }, &[repo::i(id)])?;
    let jobs: Vec<i64> = repo::all_on(&tx, if project { "SELECT id FROM results WHERE project_id=?" } else { "SELECT id FROM results WHERE image_id=?" }, &[repo::i(id)])?.into_iter().filter_map(|r| r["id"].as_i64()).collect();
    let mut files = repo::results::rows_paths_on(&tx, if project { "project_id=?" } else { "image_id=?" }, &[repo::i(id)])?;
    for row in repo::all_on(&tx, if project { "SELECT orig_path FROM results WHERE project_id=?" } else { "SELECT orig_path FROM results WHERE image_id=?" }, &[repo::i(id)])? {
        if let Some(p) = row["orig_path"].as_str().filter(|p| p.contains("_adjinput")) { files.push(p.to_string()); }
    }
    let mut dirs = Vec::new();
    for image in &images {
        let iid = image["id"].as_i64().unwrap();
        let pid = image["project_id"].as_i64().unwrap();
        for k in ["orig_path", "mask_path", "thumb_path", "proxy_path"] {
            if let Some(p) = image[k].as_str().filter(|p| !p.is_empty()) { files.push(p.to_string()); }
        }
        let key = format!("canvas_refs:{iid}");
        for row in repo::all_on(&tx, "SELECT value FROM app_settings WHERE key=?", &[repo::s(&key)])? {
            if let Some(refs) = row["value"].as_str().and_then(|s| serde_json::from_str::<Vec<String>>(s).ok()) {
                files.extend(refs.into_iter().filter(|s| s.starts_with(&format!("projects/{pid}/"))));
            }
        }
        tx.execute("DELETE FROM app_settings WHERE key=?", [&key])?;
        for table in ["image_adjust", "image_face", "image_landmark"] {
            tx.execute(&format!("DELETE FROM {table} WHERE image_id=?"), [iid])?;
        }
        // 子图是独立成品，仅撤来源引用。
        tx.execute("UPDATE images SET derived_from=NULL,derived_result=NULL WHERE derived_from=?", [iid])?;
        dirs.extend([format!("runtime/cache/t/{iid}"), format!("runtime/cache/s/{iid}")]);
    }
    tx.execute(if project { "UPDATE images SET derived_result=NULL WHERE derived_result IN (SELECT id FROM results WHERE project_id=?)" } else { "UPDATE images SET derived_result=NULL WHERE derived_result IN (SELECT id FROM results WHERE image_id=?)" }, [id])?;
    tx.execute(if project { "DELETE FROM results WHERE project_id=?" } else { "DELETE FROM results WHERE image_id=?" }, [id])?;
    tx.execute(if project { "DELETE FROM images WHERE project_id=?" } else { "DELETE FROM images WHERE id=?" }, [id])?;
    if project {
        tx.execute("DELETE FROM projects WHERE id=?", [id])?;
        tx.execute("DELETE FROM presets WHERE project_id=?", [id])?;
        dirs.push(format!("projects/{id}"));
    }
    let key = format!("delete_cleanup:{}-{}", crate::util::now_ms(), crate::util::r4());
    tx.execute("INSERT INTO app_settings(key,value) VALUES(?,?)", (&key, json!({"files":files,"dirs":dirs,"images":images}).to_string()))?;
    tx.commit()?;
    Ok(jobs)
}

pub fn pending(ctx: &Ctx) -> Result<Vec<Value>> {
    repo::all(ctx, "SELECT key,value FROM app_settings WHERE key LIKE 'delete_cleanup:%'", &[])
}
