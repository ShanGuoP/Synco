//! 项目表。app_settings 在 `settings.rs`，图片在 `images.rs`。
//! 项目行是整行直展给前端的（Node 侧 `{...row}`），所以这里提供 Value 而不是类型化结构。

use crate::error::Result;
use crate::repo;
use crate::state::Ctx;
use crate::util;
use serde_json::Value;

/// Node 那边是 `p.*` + cnt + cover 的子查询，键集合保持一样；
/// `cover_thumb` 是 M3 加的，只为首页卡片别再解码 30MB 的原图
pub fn list(ctx: &Ctx) -> Result<Vec<Value>> {
    repo::all(
        ctx,
        "SELECT p.*, (SELECT COUNT(*) FROM images i WHERE i.project_id=p.id) AS cnt,
                (SELECT i.orig_path FROM images i WHERE i.project_id=p.id ORDER BY i.id LIMIT 1) AS cover,
                (SELECT i.thumb_path FROM images i WHERE i.project_id=p.id ORDER BY i.id LIMIT 1) AS cover_thumb
         FROM projects p ORDER BY p.updated_at DESC",
        &[],
    )
}

pub fn by_id(ctx: &Ctx, id: i64) -> Result<Option<Value>> {
    repo::one(ctx, "SELECT * FROM projects WHERE id=?", &[repo::i(id)])
}

pub fn create(ctx: &Ctx, name: &str) -> Result<i64> {
    repo::insert_id(ctx, "INSERT INTO projects(name) VALUES(?)", &[repo::s(name)])
}

/// 生成也算"动过这个项目"：不然跑完一批，首页还按导入时间排
pub fn touch(ctx: &Ctx, id: i64) -> Result<()> {
    let now = util::now_localtime(&ctx.db());
    repo::run(ctx, "UPDATE projects SET updated_at=? WHERE id=?", &[repo::s(&now), repo::i(id)])?;
    Ok(())
}

/// 改名。bump `updated_at` 是有意的：首页排序键就是它，"刚改过 = 刚动过"才不反直觉；
/// 不 bump 的话改完名首页位置不动，而排序键的语义就坏了。名字纯显示用，路径全按数字 id 走。
pub fn rename(ctx: &Ctx, id: i64, name: &str) -> Result<usize> {
    let now = util::now_localtime(&ctx.db());
    repo::run(
        ctx,
        "UPDATE projects SET name=?, updated_at=? WHERE id=?",
        &[repo::s(name), repo::s(&now), repo::i(id)],
    )
}

/// 返回受影响行数：0 表示没有这个项目，handler 要据此回 404
pub fn set_settings(ctx: &Ctx, id: i64, settings_json: &str) -> Result<usize> {
    let now = util::now_localtime(&ctx.db());
    repo::run(
        ctx,
        "UPDATE projects SET settings_json=?, updated_at=? WHERE id=?",
        &[repo::s(settings_json), repo::s(&now), repo::i(id)],
    )
}

pub fn delete(ctx: &Ctx, id: i64) -> Result<()> {
    repo::run(ctx, "DELETE FROM projects WHERE id=?", &[repo::i(id)])?;
    Ok(())
}
