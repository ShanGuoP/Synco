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
        "SELECT id,status,error,prompt,steps,cfg,seed,settings_json,rerun_of,backend,model,final_path,crop_path,maskoverlay_path,thumb_path,created_at
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
                final_path,crop_path,maskoverlay_path,thumb_path,created_at
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

/// 三件套成套落盘才算 done；断在半路的文件由调用方清掉
pub fn set_files_done(ctx: &Ctx, id: i64, final_rel: &str, crop_rel: &str, overlay_rel: &str) -> Result<()> {
    repo::run(
        ctx,
        "UPDATE results SET status=?, final_path=?, crop_path=?, maskoverlay_path=? WHERE id=?",
        &[repo::s("done"), repo::s(final_rel), repo::s(crop_rel), repo::s(overlay_rel), repo::i(id)],
    )?;
    Ok(())
}

pub fn set_done(ctx: &Ctx, id: i64, final_rel: &str, thumb_rel: Option<&str>) -> Result<()> {
    if thumb_rel.is_some() {
        repo::run(
            ctx,
            "UPDATE results SET status='done', final_path=?, thumb_path=? WHERE id=?",
            &[repo::s(final_rel), repo::si(thumb_rel), repo::i(id)],
        )?;
    } else {
        repo::run(
            ctx,
            "UPDATE results SET status='done', final_path=? WHERE id=?",
            &[repo::s(final_rel), repo::i(id)],
        )?;
    }
    Ok(())
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

/// 结果目录里的三类 PNG：删项目、删单图都要跟着清，否则磁盘只涨不落
pub fn drop_files(ctx: &Ctx, where_sql: &str, arg: repo::SqlValue) -> Result<()> {
    for r in repo::all(
        ctx,
        &format!("SELECT final_path, crop_path, maskoverlay_path, thumb_path FROM results WHERE {where_sql}"),
        &[arg],
    )? {
        for k in ["final_path", "crop_path", "maskoverlay_path", "thumb_path"] {
            if let Some(rel) = r.get(k).and_then(|v| v.as_str()).filter(|x| !x.is_empty()) {
                let _ = std::fs::remove_file(ctx.data.join(rel));
            }
        }
    }
    Ok(())
}

pub fn delete_for_image(ctx: &Ctx, image_id: i64) -> Result<()> {
    repo::run(ctx, "DELETE FROM results WHERE image_id=?", &[repo::i(image_id)])?;
    Ok(())
}

pub fn delete_for_project(ctx: &Ctx, project_id: i64) -> Result<()> {
    repo::run(ctx, "DELETE FROM results WHERE project_id=?", &[repo::i(project_id)])?;
    Ok(())
}

/// 成图缩略图单独回填：320 长边的小档不影响三件套的成套判定
pub fn set_thumb(ctx: &Ctx, id: i64, thumb_rel: &str) -> Result<()> {
    repo::run(ctx, "UPDATE results SET thumb_path=? WHERE id=?", &[repo::s(thumb_rel), repo::i(id)])?;
    Ok(())
}
