//! `image_adjust` / `image_face` / `image_landmark` 三张附属表。
//! 它们都只有"跟着某张图"这一层关系，删图时由 `api::images::image_delete` 逐表清——
//! 与 `results` 同一惯例，不靠 REFERENCES（库里其余表也都没有外键）。

use crate::error::Result;
use crate::repo;
use crate::state::Ctx;
use serde_json::Value;

/// 读参数链原文。没存过就是 `None`，由调用方换成全默认——
/// 这里不做 JSON 解析，解析与夹逼是 service 的事。
pub fn ops_of(ctx: &Ctx, id: i64) -> Result<Option<String>> {
    Ok(repo::one(ctx, "SELECT ops FROM image_adjust WHERE image_id=?", &[repo::i(id)])?
        .and_then(|v| v.get("ops").and_then(Value::as_str).map(str::to_string)))
}

/// 原子替换整条参数链（UPSERT）：一次保存写全量，不做字段级合并——
/// 半新半旧的定义在"重开界面"这种最常见动作里会看不出来。
pub fn put(ctx: &Ctx, id: i64, json: &str) -> Result<()> {
    repo::run(
        ctx,
        "INSERT INTO image_adjust(image_id, ops, updated_at) VALUES(?,?,datetime('now','localtime'))
         ON CONFLICT(image_id) DO UPDATE SET ops=excluded.ops, updated_at=excluded.updated_at",
        &[repo::i(id), repo::s(json)],
    )?;
    Ok(())
}

/// 一张图的更新时间，用于判"缓存比图旧"这类比对（现在只有预览的 ETag 用得上）
pub fn updated_at(ctx: &Ctx, id: i64) -> Result<Option<String>> {
    Ok(repo::one(ctx, "SELECT updated_at FROM image_adjust WHERE image_id=?", &[repo::i(id)])?
        .and_then(|v| v.get("updated_at").and_then(Value::as_str).map(str::to_string)))
}

/// 检出的人脸行（YuNet 的框 + 5 点）。按 idx 升序回，前端第 0 张是主脸。
pub fn faces(ctx: &Ctx, id: i64) -> Result<Vec<Value>> {
    repo::all(
        ctx,
        "SELECT idx, box, landmarks, score FROM image_face WHERE image_id=? ORDER BY idx",
        &[repo::i(id)],
    )
}

/// 整批替换某张图的人脸行：重新检出后旧 idx 必须消失，
/// 否则一张图从 3 张脸变成 1 张脸时，前端还会摆出两张不存在的框。
pub fn put_faces(ctx: &Ctx, id: i64, rows: &[Value]) -> Result<()> {
    repo::run(ctx, "DELETE FROM image_face WHERE image_id=?", &[repo::i(id)])?;
    for (i, r) in rows.iter().enumerate() {
        let json_of = |k: &str| r.get(k).map(Value::to_string).unwrap_or_else(|| "[]".into());
        repo::run(
            ctx,
            "INSERT INTO image_face(image_id, idx, box, landmarks, score, updated_at)
             VALUES(?,?,?,?,?,datetime('now','localtime'))",
            &[
                repo::i(id),
                repo::i(i as i64),
                repo::s(&json_of("box")),
                repo::s(&json_of("points5")),
                repo::f(r.get("score").and_then(Value::as_f64).unwrap_or(0.0)),
            ],
        )?;
    }
    Ok(())
}

/// 稠密关键点行（FaceMesh 468 点，按需现算后落表）
pub fn landmarks(ctx: &Ctx, id: i64) -> Result<Vec<Value>> {
    repo::all(ctx, "SELECT idx, points FROM image_landmark WHERE image_id=? ORDER BY idx", &[repo::i(id)])
}

pub fn put_landmarks(ctx: &Ctx, id: i64, rows: &[Value]) -> Result<()> {
    repo::run(ctx, "DELETE FROM image_landmark WHERE image_id=?", &[repo::i(id)])?;
    for (i, r) in rows.iter().enumerate() {
        repo::run(
            ctx,
            "INSERT INTO image_landmark(image_id, idx, points, updated_at) VALUES(?,?,?,datetime('now','localtime'))",
            &[
                repo::i(id),
                repo::i(i as i64),
                repo::s(&r.get("points").map(Value::to_string).unwrap_or_else(|| "[]".into())),
            ],
        )?;
    }
    Ok(())
}

/// 删图时三张表一起清。清完行才动磁盘是既有惯例（行没了没人认领文件，比反过来轻）。
pub fn clear(ctx: &Ctx, id: i64) -> Result<()> {
    for sql in [
        "DELETE FROM image_adjust WHERE image_id=?",
        "DELETE FROM image_face WHERE image_id=?",
        "DELETE FROM image_landmark WHERE image_id=?",
    ] {
        repo::run(ctx, sql, &[repo::i(id)])?;
    }
    Ok(())
}

/// 删项目时连带清三张附属表。**要在 images 行还在的时候调用**——
/// 这三张表只存 image_id，靠子查询找归属，images 先没了就再也对不上是谁的行了
pub fn clear_project(ctx: &Ctx, pid: i64) -> Result<()> {
    for table in ["image_adjust", "image_face", "image_landmark"] {
        repo::run(ctx, &format!("DELETE FROM {table} WHERE image_id IN (SELECT id FROM images WHERE project_id=?)"), &[repo::i(pid)])?;
    }
    Ok(())
}
