//! 图片表：原图、遮罩与 M3 的派生档（thumb/proxy）。

use crate::error::Result;
use crate::models::entity::Image;
use crate::repo;
use crate::state::Ctx;

/// 单图与列表都用这一串列，键集合与 Node 的 SELECT 对齐后再补上 M3 的两个派生档
const COLS: &str = "id,project_id,name,orig_path,mask_path,w,h,created_at,thumb_path,proxy_path";

fn rows_to_images(rows: &[serde_json::Value]) -> Vec<Image> {
    rows.iter().map(Image::from_value).collect()
}

pub fn by_id(ctx: &Ctx, id: i64) -> Result<Option<Image>> {
    Ok(repo::one(ctx, &format!("SELECT {COLS} FROM images WHERE id=?"), &[repo::i(id)])?.map(|v| Image::from_value(&v)))
}

pub fn list_for_project(ctx: &Ctx, pid: i64) -> Result<Vec<Image>> {
    Ok(rows_to_images(&repo::all(ctx, &format!("SELECT {COLS} FROM images WHERE project_id=? ORDER BY id"), &[repo::i(pid)])?))
}

/// 派生档补空当用的扫描：只挑还没生成过缩略图的行，跑一次就少一批
pub fn list_missing_thumb(ctx: &Ctx, limit: i64) -> Result<Vec<Image>> {
    Ok(rows_to_images(
        &repo::all(
            ctx,
            &format!("SELECT {COLS} FROM images WHERE thumb_path IS NULL ORDER BY id LIMIT ?"),
            &[repo::i(limit)],
        )?,
    ))
}

pub fn insert(ctx: &Ctx, pid: i64, name: &str, orig_path: &str, w: i64, h: i64) -> Result<i64> {
    repo::insert_id(
        ctx,
        "INSERT INTO images(project_id,name,orig_path,w,h) VALUES(?,?,?,?,?)",
        &[repo::i(pid), repo::s(name), repo::s(orig_path), repo::i(w), repo::i(h)],
    )
}

pub fn set_mask(ctx: &Ctx, id: i64, mask_rel: Option<&str>) -> Result<()> {
    repo::run(ctx, "UPDATE images SET mask_path=? WHERE id=?", &[repo::si(mask_rel), repo::i(id)])?;
    Ok(())
}

/// thumb/proxy 一次写两列：缺失时传 None，前端一律回退 orig_url
pub fn set_derived(ctx: &Ctx, id: i64, thumb: Option<&str>, proxy: Option<&str>) -> Result<()> {
    repo::run(
        ctx,
        "UPDATE images SET thumb_path=?, proxy_path=? WHERE id=?",
        &[repo::si(thumb), repo::si(proxy), repo::i(id)],
    )?;
    Ok(())
}

/// 校正回真实像素尺寸：库里那对 w/h 是导入时客户端报的，报错了显示与裁切会一直歪着
pub fn set_dims(ctx: &Ctx, id: i64, w: i64, h: i64) -> Result<()> {
    repo::run(ctx, "UPDATE images SET w=?, h=? WHERE id=?", &[repo::i(w), repo::i(h), repo::i(id)])?;
    Ok(())
}

pub fn delete(ctx: &Ctx, id: i64) -> Result<()> {
    repo::run(ctx, "DELETE FROM images WHERE id=?", &[repo::i(id)])?;
    Ok(())
}

pub fn delete_for_project(ctx: &Ctx, pid: i64) -> Result<()> {
    repo::run(ctx, "DELETE FROM images WHERE project_id=?", &[repo::i(pid)])?;
    Ok(())
}

/// 名称/尺寸，派生新图时拿来起名（fork 用得上，不需要整行）
pub fn name_and_size(ctx: &Ctx, id: i64) -> Result<Option<(String, i64, i64)>> {
    Ok(repo::one(ctx, "SELECT name, w, h FROM images WHERE id=?", &[repo::i(id)])?.map(|r| {
        (
            r.get("name").and_then(|v| v.as_str()).unwrap_or("photo").to_string(),
            r.get("w").and_then(|v| v.as_i64()).unwrap_or(0),
            r.get("h").and_then(|v| v.as_i64()).unwrap_or(0),
        )
    }))
}
