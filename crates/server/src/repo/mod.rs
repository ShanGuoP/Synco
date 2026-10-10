//! DAO 层的公共部分：SQL 值的构造小工具 + 动态行读取（与 Node 的 `{...row}` 对齐），
//! 按表分在 projects / images / results / settings / backends 几个子模块里。
//!
//! 下面这几个通用执行器与构造器**只在 crate::repo 内可见**：出了这层就拼不出一条 SQL，
//! 也摸不到连接句柄——分层因此是编译错误，不是 `lint-layers.js` 的一条正则。
//! 外面要新查询，就在对应表的子模块里加一个具名函数。

use crate::error::{AppError, Result};
use crate::state::Ctx;
pub(crate) use rusqlite::types::Value as SqlValue;
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{Map, Value};

pub mod backends;
pub mod db;
pub mod adjust;
pub mod images;
pub mod presets;
pub mod projects;
pub mod results;
pub mod settings;

pub(in crate::repo) fn s(v: &str) -> SqlValue {
    SqlValue::Text(v.to_string())
}
pub(in crate::repo) fn si(v: Option<&str>) -> SqlValue {
    match v {
        Some(x) => SqlValue::Text(x.to_string()),
        None => SqlValue::Null,
    }
}
pub(in crate::repo) fn i(v: i64) -> SqlValue {
    SqlValue::Integer(v)
}
pub(in crate::repo) fn sopt(v: Option<i64>) -> SqlValue {
    match v {
        Some(x) => SqlValue::Integer(x),
        None => SqlValue::Null,
    }
}
pub(in crate::repo) fn f(v: f64) -> SqlValue {
    SqlValue::Real(v)
}

fn refs_of(args: &[SqlValue]) -> Vec<&dyn rusqlite::ToSql> {
    args.iter().map(|v| v as &dyn rusqlite::ToSql).collect()
}

fn row_value(names: &[String], row: &rusqlite::Row) -> Value {
    let mut m = Map::new();
    for (i, name) in names.iter().enumerate() {
        let v = match row.get_ref_unwrap(i) {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(n) => Value::from(n),
            ValueRef::Real(x) => Value::from(x),
            ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
            // 库里的 BLOB 不该出现在 JSON 里：给一句 ASCII 的形状描述，日志与界面都不会把中文当文案
            ValueRef::Blob(b) => Value::String(format!("<blob {} bytes>", b.len())),
        };
        m.insert(name.clone(), v);
    }
    Value::Object(m)
}

pub(in crate::repo) fn all_on(db: &Connection, sql: &str, args: &[SqlValue]) -> Result<Vec<Value>> {
    let mut st = db.prepare(sql).map_err(AppError::Sql)?;
    let names: Vec<String> = (0..st.column_count()).map(|i| st.column_name(i).unwrap_or("?").to_string()).collect();
    let mut rows = st.query(refs_of(args).as_slice()).map_err(AppError::Sql)?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(AppError::Sql)? {
        out.push(row_value(&names, r));
    }
    Ok(out)
}

pub(in crate::repo) fn all(ctx: &Ctx, sql: &str, args: &[SqlValue]) -> Result<Vec<Value>> {
    let db = ctx.db();
    all_on(&db, sql, args)
}

pub(in crate::repo) fn one(ctx: &Ctx, sql: &str, args: &[SqlValue]) -> Result<Option<Value>> {
    let db = ctx.db();
    Ok(all_on(&db, sql, args)?.pop())
}

/// 写操作，返回受影响行数（对应 Node 的 `.run().changes`）
pub(in crate::repo) fn run(ctx: &Ctx, sql: &str, args: &[SqlValue]) -> Result<usize> {
    let db = ctx.db();
    let mut st = db.prepare_cached(sql).map_err(AppError::Sql)?;
    Ok(st.execute(refs_of(args).as_slice()).map_err(AppError::Sql)?)
}

/// INSERT 后取自增主键。execute 与取号必须在**同一次锁作用域**里：
/// 分两次加锁的话，中间只要有别的线程也插一条，`last_insert_rowid()` 就返回别人那行，
/// 结果会挂到错图上——并发建项目/导图/提交时才会撞上，所以平时看不出来。
pub(in crate::repo) fn insert_id(ctx: &Ctx, sql: &str, args: &[SqlValue]) -> Result<i64> {
    let db = ctx.db();
    let mut st = db.prepare_cached(sql).map_err(AppError::Sql)?;
    st.execute(refs_of(args).as_slice()).map_err(AppError::Sql)?;
    Ok(db.last_insert_rowid())
}
