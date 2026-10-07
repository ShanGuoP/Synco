//! 生成记录表：本机链路与云端链路共用一张表，靠 backend 列区分。

use crate::error::Result;
use crate::models::entity::ResultRow;
use crate::repo;
use crate::state::Ctx;
use serde_json::Value;

pub fn value_by_id(ctx: &Ctx, id: i64) -> Result<Option<Value>> {
    repo::one(ctx, "SELECT * FROM results WHERE id=?", &[repo::i(id)])
}

pub fn by_id(ctx: &Ctx, id: i64) -> Result<Option<ResultRow>> {
    Ok(value_by_id(ctx, id)?.map(|v| ResultRow::from_value(&v)))
}

/// 单图接口只要这 16 列（Node 的 SELECT 就是这个子集，多给字段就是契约变化；
/// thumb_path 是 M3 加的，历史列靠它，不再回落到 20–33MB 的成图）
pub fn list_for_image(ctx: &Ctx, image_id: i64, limit: i64) -> Result<Vec<Value>> {
    repo::all(
        ctx,
        "SELECT id,status,error,prompt,steps,cfg,seed,settings_json,rerun_of,backend,model,final_path,crop_path,maskoverlay_path,thumb_path,sketch_path,created_at
         FROM results WHERE image_id=? ORDER BY id DESC LIMIT ?",
        &[repo::i(image_id), repo::i(limit)],
    )
}

/// 「派生查看」用的集合查询：一个项目的全部结果行，一次读完，不带任何副作用。
/// `/api/images/{id}` 顶不了它——那条只给 8 条，而且每读一张就判一次云端僵尸、补一次派生档。
pub fn list_for_project(ctx: &Ctx, pid: i64, limit: i64) -> Result<Vec<Value>> {
    repo::all(
        ctx,
        "SELECT id,image_id,project_id,status,error,prompt,steps,cfg,seed,settings_json,rerun_of,backend,model,
                final_path,crop_path,maskoverlay_path,thumb_path,sketch_path,created_at
         FROM results WHERE project_id=? ORDER BY id DESC LIMIT ?",
        &[repo::i(pid), repo::i(limit)],
    )
}

pub fn list_running(ctx: &Ctx) -> Result<Vec<(i64, Option<String>)>> {
    Ok(repo::all(ctx, "SELECT id, prompt_id FROM results WHERE status='running'", &[])?
        .iter()
        .map(|r| {
            (
                r.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                r.get("prompt_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string),
            )
        })
        .collect())
}

/// 云端僵尸行：挂着 running 又没有 prompt_id，本机轮询接不回来
pub fn cloud_zombies(ctx: &Ctx, image_id: i64) -> Result<Vec<i64>> {
    Ok(repo::all(
        ctx,
        "SELECT id FROM results WHERE image_id=? AND status='running' AND (prompt_id IS NULL OR prompt_id='')",
        &[repo::i(image_id)],
    )?
    .iter()
    .filter_map(|r| r.get("id").and_then(|v| v.as_i64()))
    .collect())
}

#[allow(clippy::too_many_arguments)]
pub fn insert_local(
    ctx: &Ctx,
    image_id: i64,
    project_id: i64,
    prompt_id: &str,
    prompt: &str,
    steps: f64,
    cfg: f64,
    seed: i64,
    orig_path: &str,
    settings_json: &str,
    rerun_of: Option<i64>,
) -> Result<i64> {
    repo::insert_id(
        ctx,
        "INSERT INTO results(image_id,project_id,status,prompt_id,prompt,steps,cfg,seed,orig_path,settings_json,rerun_of)
         VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        &[
            repo::i(image_id),
            repo::i(project_id),
            repo::s("running"),
            repo::s(prompt_id),
            repo::s(prompt),
            repo::f(steps),
            repo::f(cfg),
            repo::i(seed),
            repo::s(orig_path),
            repo::s(settings_json),
            repo::sopt(rerun_of),
        ],
    )
}

pub fn insert_cloud(
    ctx: &Ctx,
    image_id: i64,
    project_id: i64,
    prompt: &str,
    settings_json: &str,
    model: &str,
    rerun_of: Option<i64>,
) -> Result<i64> {
    repo::insert_id(
        ctx,
        "INSERT INTO results(image_id,project_id,status,prompt,steps,cfg,settings_json,backend,model,rerun_of)
         VALUES(?,?,?,?,?,?,?,?,?,?)",
        &[
            repo::i(image_id),
            repo::i(project_id),
            repo::s("running"),
            repo::s(prompt),
            repo::f(0.0),
            repo::f(0.0),
            repo::s(settings_json),
            repo::s("cloud"),
            repo::s(model),
            repo::sopt(rerun_of),
        ],
    )
}

/// 成图 + 这一行映射了的辅助输出一起落盘才算 done；断在半路的文件由调用方清掉。
/// 裁切区与遮罩图是诊断用的：用户的工作流里没有那两个节点时它们是 NULL，
/// 界面按没有这两个视图渲染（比为了凑三件套把一张成功成图判死诚实）。
///
/// 状态守卫与 `mark_running` 同一套纪律：只写 `WHERE id=?` 的话，用户点了中断（行已置 error）
/// 之后才回来的那次下载会把行原地改回 done——看着撤掉了，钱照样花了、图还进了历史列。
/// 返回 `false` = 这一行在我们干活期间被中断或删掉了，调用方要把刚落盘的文件清掉。
pub fn set_files_done(ctx: &Ctx, id: i64, final_rel: &str, crop_rel: Option<&str>, overlay_rel: Option<&str>) -> Result<bool> {
    let n = repo::run(
        ctx,
        "UPDATE results SET status=?, final_path=?, crop_path=?, maskoverlay_path=? WHERE id=? AND status='running'",
        &[repo::s("done"), repo::s(final_rel), repo::si(crop_rel), repo::si(overlay_rel), repo::i(id)],
    )?;
    Ok(n == 1)
}

/// 同 [`set_files_done`]：只在行还是 running 时才落定，返回这次是不是我落的定。
pub fn set_done(ctx: &Ctx, id: i64, final_rel: &str, thumb_rel: Option<&str>) -> Result<bool> {
    let n = if thumb_rel.is_some() {
        repo::run(
            ctx,
            "UPDATE results SET status='done', final_path=?, thumb_path=? WHERE id=? AND status='running'",
            &[repo::s(final_rel), repo::si(thumb_rel), repo::i(id)],
        )?
    } else {
        repo::run(
            ctx,
            "UPDATE results SET status='done', final_path=? WHERE id=? AND status='running'",
            &[repo::s(final_rel), repo::i(id)],
        )?
    };
    Ok(n == 1)
}

pub fn set_queued(ctx: &Ctx, id: i64) -> Result<()> {
    repo::run(ctx, "UPDATE results SET status='queued' WHERE id=?", &[repo::i(id)])?;
    Ok(())
}

/// 队列取单：queued → running，同时把 error 清空（重排队时上一轮的报错不该还挂着）。
/// 返回**这一枪是不是我抢到的**：`pump` 可以在入队、每个 worker 收尾、启动三处并发跑，
/// 没有 `AND status='queued'` 的话两个泵都会把同一行判给"自己"，云端就重绘两次、钱花两次。
pub fn mark_running(ctx: &Ctx, id: i64) -> Result<bool> {
    let n = repo::run(ctx, "UPDATE results SET status='running', error=NULL WHERE id=? AND status='queued'", &[repo::i(id)])?;
    Ok(n == 1)
}

/// 队列里的行按提交顺序推进
pub fn list_queued(ctx: &Ctx) -> Result<Vec<i64>> {
    Ok(repo::all(ctx, "SELECT id FROM results WHERE status='queued' ORDER BY id", &[])?
        .iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_i64()))
        .collect())
}

/// 进度面板：还在排的 + 正在跑的，按 id 升序
pub fn list_active(ctx: &Ctx) -> Result<Vec<Value>> {
    repo::all(
        ctx,
        "SELECT id,image_id,project_id,status,error,prompt,backend,model,settings_json,created_at
         FROM results WHERE status IN ('queued','running') ORDER BY id",
        &[],
    )
}

/// 关页面 / 重启之后还在排的行
pub fn count_by_status(ctx: &Ctx) -> Result<(i64, i64)> {
    let q = repo::one(ctx, "SELECT count(*) AS n FROM results WHERE status='queued'", &[])?
        .and_then(|v| v.get("n").and_then(|x| x.as_i64()))
        .unwrap_or(0);
    let r = repo::one(ctx, "SELECT count(*) AS n FROM results WHERE status='running'", &[])?
        .and_then(|v| v.get("n").and_then(|x| x.as_i64()))
        .unwrap_or(0);
    Ok((q, r))
}

pub fn delete(ctx: &Ctx, id: i64) -> Result<()> {
    repo::run(ctx, "DELETE FROM results WHERE id=?", &[repo::i(id)])?;
    Ok(())
}

/// 把这些行记着的五类 PNG 路径取出来。**只查不删**：调用方先删行、再按这份清单删文件。
/// 顺序反过来的话一旦断在中途，库里就挂着指向空气的记录（卡片在、点开是空的）——
/// 而这样最多多留几个没人认领的文件，盘上多一张照片不会骗人。
pub fn list_paths(ctx: &Ctx, where_sql: &str, arg: repo::SqlValue) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for r in repo::all(
        ctx,
        &format!("SELECT final_path, crop_path, maskoverlay_path, thumb_path, sketch_path FROM results WHERE {where_sql}"),
        &[arg],
    )? {
        for k in ["final_path", "crop_path", "maskoverlay_path", "thumb_path", "sketch_path"] {
            if let Some(rel) = r.get(k).and_then(|v| v.as_str()).filter(|x| !x.is_empty()) {
                out.push(rel.to_string());
            }
        }
    }
    Ok(out)
}

pub fn delete_for_image(ctx: &Ctx, image_id: i64) -> Result<()> {
    repo::run(ctx, "DELETE FROM results WHERE image_id=?", &[repo::i(image_id)])?;
    Ok(())
}

pub fn delete_for_project(ctx: &Ctx, project_id: i64) -> Result<()> {
    repo::run(ctx, "DELETE FROM results WHERE project_id=?", &[repo::i(project_id)])?;
    Ok(())
}

/// 成图缩略图单独回填：320 长边的小档不影响三件套的成套判定。
/// 状态守卫放 `running`/`done` 而不是只放 `running`：本机那一路是"先落定再后台补缩略图"，
/// 只认 running 会让历史列永远拿不到小档；被中断（error）的那一行则不该记这张马上要清掉的档。
pub fn set_thumb(ctx: &Ctx, id: i64, thumb_rel: &str) -> Result<()> {
    repo::run(
        ctx,
        "UPDATE results SET thumb_path=? WHERE id=? AND status IN ('running','done')",
        &[repo::s(thumb_rel), repo::i(id)],
    )?;
    Ok(())
}

/// 画布这一版提交时的线稿快照。每一步生成各存一份（不做上限）：
/// 画稿本身是原地覆写的，用户接着画两笔，"出这张图时我画的是什么"就只剩这一份能证明。
pub fn set_sketch(ctx: &Ctx, id: i64, sketch_rel: &str) -> Result<()> {
    repo::run(ctx, "UPDATE results SET sketch_path=? WHERE id=?", &[repo::s(sketch_rel), repo::i(id)])?;
    Ok(())
}

/// 把这一版**实际带走**的参考图记进行上的参数快照。
/// 不开新列：`settings_json` 本来就是"这一枪的参数"，两处真相会开始互相说不清。
/// 读回原块、插一节再写回，所以调用方要在插入之后拿 rid。
pub fn set_refs(ctx: &Ctx, id: i64, rels: &[String]) -> Result<()> {
    let cur = repo::one(ctx, "SELECT settings_json FROM results WHERE id=?", &[repo::i(id)])?
        .and_then(|v| v.get("settings_json").and_then(|x| x.as_str()).map(str::to_string));
    let mut obj = cur
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let arr: Vec<Value> = rels.iter().map(|r| Value::String(r.clone())).collect();
    match obj.as_object_mut() {
        Some(m) => {
            m.insert("refs".into(), Value::Array(arr));
        }
        None => return Err(crate::error::AppError::Fail("这一行的参数快照不是对象，参考图记不进去".into())),
    }
    repo::run(ctx, "UPDATE results SET settings_json=? WHERE id=?", &[repo::s(&obj.to_string()), repo::i(id)])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 落定只认还在跑的那一行：`mark_running` 早就有的守卫，`set_done`/`set_files_done`
    /// 以前没有——点了中断（行置 error）之后才回来的下载会把行原地改回 done。
    #[test]
    fn 中断之后迟到的落定不复活() {
        let dir = std::env::temp_dir().join(format!("synco-res-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
        let pid = crate::repo::projects::create(&ctx, "守卫").unwrap();
        let rel = format!("projects/{pid}/a.png");
        let iid = crate::repo::images::insert(&ctx, pid, "a.png", &rel, 8, 8).unwrap();
        let id = insert_cloud(&ctx, iid, pid, "p", "{}", "m", None).unwrap();
        set_queued(&ctx, id).unwrap();
        assert!(mark_running(&ctx, id).unwrap(), "queued → running 该抢到");
        assert!(!mark_running(&ctx, id).unwrap(), "同一行不能被两个泵各领一次");
        ctx.mark_error(id, "已手动中断");
        assert!(!set_done(&ctx, id, &format!("projects/{pid}/r.png"), None).unwrap(), "迟到的落定不该把中断的行改回 done");
        let row = value_by_id(&ctx, id).unwrap().unwrap();
        assert_eq!(row.get("status").and_then(|v| v.as_str()), Some("error"));
        assert!(row.get("final_path").and_then(|v| v.as_str()).unwrap_or("").is_empty(), "中断的行不该挂着成图路径");
        drop(ctx);   // 连接还开着的时候 Windows 删不掉 app.db
        let _ = std::fs::remove_dir_all(&dir);
    }
}
