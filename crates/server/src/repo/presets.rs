//! 预设与提示词短语共用一张表，靠 `kind` 分桶：
//! `preset` = 整份参数快照（提示词+负面+步数+CFG+LoRA 链），`phrase` = 可单独并入/移出的一句话。
//! 同名的查重跨作用域判——以前按"一个桶"查，全局一条 X 和项目 5 一条 X 能同时存在，界面上显示成两条。

use crate::error::Result;
use crate::models::dto::PresetFields;
use crate::repo;
use crate::state::Ctx;
use crate::util;
use serde_json::Value;

pub const KIND_PRESET: &str = "preset";
pub const KIND_PHRASE: &str = "phrase";

/// 请求里的 kind 只认这两种，其它一律按预设处理（不给造新桶的机会）
pub fn norm_kind(v: Option<&str>) -> String {
    if v == Some(KIND_PHRASE) { KIND_PHRASE.to_string() } else { KIND_PRESET.to_string() }
}

/// 带 project_id 时"全局 + 这个项目"都要列出来，不带时只看全局。
/// 短语按 id 排（胶囊那一排的顺序是有意义的，出厂 7 条的摆位不该被字面排序打乱），预设按名字排。
pub fn list(ctx: &Ctx, project_id: Option<i64>, kind: &str) -> Result<Vec<Value>> {
    let order = if kind == KIND_PHRASE { "id" } else { "name" };
    let sql = match project_id {
        Some(_) => format!("SELECT * FROM presets WHERE kind=? AND (project_id IS NULL OR project_id=?) ORDER BY {order}"),
        None => format!("SELECT * FROM presets WHERE kind=? AND project_id IS NULL ORDER BY {order}"),
    };
    match project_id {
        Some(pid) => repo::all(ctx, &sql, &[repo::s(kind), repo::i(pid)]),
        None => repo::all(ctx, &sql, &[repo::s(kind)]),
    }
}

pub fn by_id(ctx: &Ctx, id: i64) -> Result<Option<Value>> {
    repo::one(ctx, "SELECT * FROM presets WHERE id=?", &[repo::i(id)])
}

/// 同一个桶里名字唯一，不看作用域；except_id 给改名用（自己不算撞）
pub fn name_taken(ctx: &Ctx, name: &str, kind: &str, except_id: Option<i64>) -> Result<bool> {
    Ok(repo::one(
        ctx,
        "SELECT id FROM presets WHERE name=? AND kind=? AND (? IS NULL OR id<>?)",
        &[repo::s(name), repo::s(kind), repo::sopt(except_id), repo::i(except_id.unwrap_or(-1))],
    )?
    .is_some())
}

pub fn exists(ctx: &Ctx, id: i64) -> Result<bool> {
    Ok(repo::one(ctx, "SELECT id FROM presets WHERE id=?", &[repo::i(id)])?.is_some())
}

pub fn insert(ctx: &Ctx, name: &str, project_id: Option<i64>, kind: &str, f: &PresetFields) -> Result<i64> {
    let now = util::now_localtime(&ctx.db());
    repo::insert_id(
        ctx,
        "INSERT INTO presets(name,project_id,kind,prompt,negative,steps,cfg,loras_json,updated_at) VALUES(?,?,?,?,?,?,?,?,?)",
        &[
            repo::s(name),
            repo::sopt(project_id),
            repo::s(kind),
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
