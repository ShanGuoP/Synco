//! app_settings 键值表：工作流路径、生效后端、导出目录、云端参数、缝合三参数都存这里。

use crate::error::Result;
use crate::repo;
use crate::state::Ctx;

pub fn get(ctx: &Ctx, key: &str) -> Option<String> {
    get_raw(ctx, key).filter(|v| !v.is_empty())
}

/// 云端那套设置要区分"没存过"（用默认）与"存了空串"（用户明确这个字段别发）
pub fn get_raw(ctx: &Ctx, key: &str) -> Option<String> {
    let db = ctx.db();
    db.query_row("SELECT value FROM app_settings WHERE key=?", [key], |r| r.get::<_, Option<String>>(0))
        .unwrap_or(None)
}

pub fn put(ctx: &Ctx, key: &str, value: &str) -> Result<()> {
    repo::run(
        ctx,
        "INSERT INTO app_settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        &[repo::s(key), repo::s(value)],
    )?;
    Ok(())
}

pub fn del(ctx: &Ctx, key: &str) -> Result<()> {
    repo::run(ctx, "DELETE FROM app_settings WHERE key=?", &[repo::s(key)])?;
    Ok(())
}

/// 只有工作流文件身份与按文件登记的映射需要在资料快照中重定位。
pub fn workflow_locations(ctx: &Ctx) -> Result<Vec<(String, String)>> {
    let rows = repo::all(ctx, "SELECT key,value FROM app_settings WHERE key='workflow_path' OR key LIKE 'workflow_roles:%' OR key LIKE 'workflow_profile:%'", &[])?;
    Ok(rows.into_iter().filter_map(|row| Some((row["key"].as_str()?.to_string(), row["value"].as_str()?.to_string()))).collect())
}
