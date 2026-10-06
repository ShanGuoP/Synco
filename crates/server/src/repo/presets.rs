//! 预设表：全局预设（project_id 为 NULL）和项目内预设共存，同名判定跨不了作用域。

use crate::error::Result;
use crate::models::dto::PresetFields;
use crate::repo;
use crate::state::Ctx;
use crate::util;
use serde_json::Value;

/// 带 project_id 时"全局 + 这个项目"都要列出来，不带时只看全局
pub fn list(ctx: &Ctx, project_id: Option<i64>) -> Result<Vec<Value>> {
    match project_id {
        Some(pid) => repo::all(
            ctx,
            "SELECT * FROM presets WHERE project_id IS NULL OR project_id=? ORDER BY name",
            &[repo::i(pid)],
        ),
        None => repo::all(ctx, "SELECT * FROM presets WHERE project_id IS NULL ORDER BY name", &[]),
    }
}

pub fn by_id(ctx: &Ctx, id: i64) -> Result<Option<Value>> {
    repo::one(ctx, "SELECT * FROM presets WHERE id=?", &[repo::i(id)])
}

pub fn name_taken(ctx: &Ctx, name: &str, project_id: Option<i64>) -> Result<bool> {
    Ok(repo::one(
        ctx,
        "SELECT id FROM presets WHERE name=? AND (project_id IS ? OR project_id=?)",
        &[repo::s(name), repo::sopt(project_id), repo::i(project_id.unwrap_or(-1))],
    )?
    .is_some())
}

pub fn exists(ctx: &Ctx, id: i64) -> Result<bool> {
    Ok(repo::one(ctx, "SELECT id FROM presets WHERE id=?", &[repo::i(id)])?.is_some())
}

pub fn insert(ctx: &Ctx, name: &str, project_id: Option<i64>, f: &PresetFields) -> Result<i64> {
    let now = util::now_localtime(&ctx.db());
    repo::insert_id(
        ctx,
        "INSERT INTO presets(name,project_id,prompt,negative,steps,cfg,loras_json,updated_at) VALUES(?,?,?,?,?,?,?,?)",
        &[
            repo::s(name),
            repo::sopt(project_id),
            repo::s(&f.prompt),
            repo::s(&f.negative),
            repo::f(f.steps),
            repo::f(f.cfg),
            repo::s(&f.loras_json),
            repo::s(&now),
        ],
    )
}

pub fn update(ctx: &Ctx, id: i64, project_id: Option<i64>, f: &PresetFields) -> Result<()> {
    let now = util::now_localtime(&ctx.db());
    repo::run(
        ctx,
        "UPDATE presets SET name=?, project_id=?, prompt=?, negative=?, steps=?, cfg=?, loras_json=?, updated_at=? WHERE id=?",
        &[
            repo::s(&f.name),
            repo::sopt(project_id),
            repo::s(&f.prompt),
            repo::s(&f.negative),
            repo::f(f.steps),
            repo::f(f.cfg),
            repo::s(&f.loras_json),
            repo::s(&now),
            repo::i(id),
        ],
    )?;
    Ok(())
}

pub fn delete(ctx: &Ctx, id: i64) -> Result<()> {
    repo::run(ctx, "DELETE FROM presets WHERE id=?", &[repo::i(id)])?;
    Ok(())
}
