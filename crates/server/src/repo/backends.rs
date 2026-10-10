//! 手工登记的 ComfyUI 地址（`backends` 表）。这张表的 SQL 只写在这里，service 层拿的是函数。

use crate::error::Result;
use crate::repo;
use crate::state::Ctx;
use serde_json::Value;

/// 登记顺序即界面顺序：后加的排最后
pub fn list(ctx: &Ctx) -> Vec<Value> {
    repo::all(ctx, "SELECT url, label, added_at FROM backends ORDER BY added_at", &[]).unwrap_or_default()
}

/// 同一地址重复登记只换名字——冲突按 url 判，不会长出第二条
pub fn upsert(ctx: &Ctx, url: &str, label: Option<&str>) -> Result<()> {
    repo::run(
        ctx,
        "INSERT INTO backends(url,label) VALUES(?,?) ON CONFLICT(url) DO UPDATE SET label=excluded.label",
        &[repo::s(url), repo::si(label)],
    )?;
    Ok(())
}

pub fn remove(ctx: &Ctx, url: &str) -> Result<()> {
    repo::run(ctx, "DELETE FROM backends WHERE url=?", &[repo::s(url)])?;
    Ok(())
}
