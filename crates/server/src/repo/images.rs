//! 图片表：原图、遮罩与 M3 的派生档（thumb/proxy）。

use crate::error::Result;
use crate::models::entity::Image;
use crate::repo;
use crate::state::Ctx;

/// 单图与列表都用这一串列，键集合与 Node 的 SELECT 对齐后再补上 M3 的两个派生档
const COLS: &str =
    "id,project_id,name,orig_path,mask_path,w,h,created_at,thumb_path,proxy_path,kind,derived_from,derived_result";

fn rows_to_images(rows: &[serde_json::Value]) -> Vec<Image> {
    rows.iter().map(Image::from_value).collect()
}

pub fn by_id(ctx: &Ctx, id: i64) -> Result<Option<Image>> {
    Ok(repo::one(ctx, &format!("SELECT {COLS} FROM images WHERE id=?"), &[repo::i(id)])?.map(|v| Image::from_value(&v)))
}

/// 项目详情的列表：除图片列外再带四个结果聚合，供卡片角标与「派生查看」用。
/// 相关子查询走 `idx_results_image`，每张图四次索引查找；比让前端逐张 GET `/api/images/{id}`
/// 少一整轮请求，也不再靠"本次会话跑过几张"猜状态（刷新后 has_result 会全丢）。
pub fn list_for_project(ctx: &Ctx, pid: i64) -> Result<Vec<Image>> {
    Ok(rows_to_images(&repo::all(ctx, &project_list_sql(), &[repo::i(pid)])?))
}

/// 同上，但把聚合列一起交出来（`Image` 是类型化的，接不住这几列）
pub fn list_for_project_rows(ctx: &Ctx, pid: i64) -> Result<Vec<serde_json::Value>> {
    repo::all(ctx, &project_list_sql(), &[repo::i(pid)])
}

fn project_list_sql() -> String {
    format!(
        "SELECT {COLS},
                (SELECT COUNT(*) FROM results r WHERE r.image_id = images.id) AS result_count,
                (SELECT COUNT(*) FROM results r WHERE r.image_id = images.id AND r.status='done') AS result_done,
                (SELECT r.thumb_path FROM results r WHERE r.image_id = images.id AND r.status='done'
                  ORDER BY r.id DESC LIMIT 1) AS result_thumb,
                (SELECT r.final_path FROM results r WHERE r.image_id = images.id AND r.status='done'
                  ORDER BY r.id DESC LIMIT 1) AS result_final,
                /* 卡片角标的第二个数：这张图「另存为新图」出去了几张子图 */
                (SELECT COUNT(*) FROM images d WHERE d.derived_from = images.id) AS derived_count
         FROM images WHERE project_id=? ORDER BY id"
    )
}

/// 派生弹窗的数据源：这张图直接派生出来的子图（不递归，孙图挂在子图名下）
pub fn list_derived(ctx: &Ctx, id: i64) -> Result<Vec<Image>> {
    Ok(rows_to_images(&repo::all(ctx, &format!("SELECT {COLS} FROM images WHERE derived_from=? ORDER BY id"), &[repo::i(id)])?))
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
    insert_kind(ctx, pid, name, orig_path, w, h, "photo")
}

/// 画布用的建行：kind='sketch' 的行走的是另一条出图链路（`api::canvas`）
pub fn insert_kind(ctx: &Ctx, pid: i64, name: &str, orig_path: &str, w: i64, h: i64, kind: &str) -> Result<i64> {
    repo::insert_id(
        ctx,
        "INSERT INTO images(project_id,name,orig_path,w,h,kind) VALUES(?,?,?,?,?,?)",
        &[repo::i(pid), repo::s(name), repo::s(orig_path), repo::i(w), repo::i(h), repo::s(kind)],
    )
}

/// 「另存为新图」造的子图：把父图与来源结果行一起落库。
/// 名字里那个 `派生{结果号}` 只是给人看的，谱系全靠这两列。
#[allow(clippy::too_many_arguments)]
pub fn insert_derived(ctx: &Ctx, pid: i64, name: &str, orig_path: &str, w: i64, h: i64, from_image: i64, from_result: i64) -> Result<i64> {
    repo::insert_id(
        ctx,
        "INSERT INTO images(project_id,name,orig_path,w,h,kind,derived_from,derived_result) VALUES(?,?,?,?,?,?,?,?)",
        &[
            repo::i(pid),
            repo::s(name),
            repo::s(orig_path),
            repo::i(w),
            repo::i(h),
            repo::s("photo"),
            repo::i(from_image),
            repo::i(from_result),
        ],
    )
}

/// 删父图时把子图的来源引用清掉：子图本身要留着（它是独立的一张图，
/// 删父图不该连带删掉用户另存出去的成品），但 `derived_from` 留着就是指向空 id 的悬值
pub fn clear_derived_refs(ctx: &Ctx, id: i64) -> Result<()> {
    repo::run(ctx, "UPDATE images SET derived_from=NULL WHERE derived_from=?", &[repo::i(id)])?;
    Ok(())
}

pub fn set_mask(ctx: &Ctx, id: i64, mask_rel: Option<&str>) -> Result<()> {    repo::run(ctx, "UPDATE images SET mask_path=? WHERE id=?", &[repo::si(mask_rel), repo::i(id)])?;
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
