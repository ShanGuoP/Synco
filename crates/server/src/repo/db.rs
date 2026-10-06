//! SQLite：建表与迁移。列名、默认值与迁移顺序都得跟着已经发布出去的 `app.db` 走——
//! 库里存着历史行，改一处就会让老库读不出东西。

use rusqlite::Connection;
use std::path::Path;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS projects(
  id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, settings_json TEXT DEFAULT '{}',
  created_at TEXT DEFAULT (datetime('now','localtime')), updated_at TEXT DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS images(
  id INTEGER PRIMARY KEY AUTOINCREMENT, project_id INTEGER, name TEXT,
  orig_path TEXT, mask_path TEXT, w INTEGER, h INTEGER,
  created_at TEXT DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS results(
  id INTEGER PRIMARY KEY AUTOINCREMENT, image_id INTEGER, project_id INTEGER,
  status TEXT DEFAULT 'running', error TEXT, prompt_id TEXT,
  prompt TEXT, steps INTEGER, cfg REAL, seed INTEGER, orig_path TEXT,
  final_path TEXT, crop_path TEXT, maskoverlay_path TEXT,
  created_at TEXT DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS app_settings(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS backends(
  url TEXT PRIMARY KEY, label TEXT,
  added_at TEXT DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS presets(
  id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, project_id INTEGER,
  prompt TEXT DEFAULT '', negative TEXT DEFAULT '', steps INTEGER, cfg REAL,
  loras_json TEXT DEFAULT '[]',
  created_at TEXT DEFAULT (datetime('now','localtime')), updated_at TEXT);
"#;

/// 旧库补列：前五条是历史库里已有的列，后三条是 M3 图像服务化要读的派生档。
/// 逐条按"列在不在"判重，所以老库直接升上来就行，不需要重建。
const MIGRATIONS: [(&str, &str, &str); 8] = [
    ("results", "prompt_id", "ALTER TABLE results ADD COLUMN prompt_id TEXT"),
    ("results", "settings_json", "ALTER TABLE results ADD COLUMN settings_json TEXT"),
    ("results", "rerun_of", "ALTER TABLE results ADD COLUMN rerun_of INTEGER"),
    ("results", "backend", "ALTER TABLE results ADD COLUMN backend TEXT DEFAULT 'comfyui'"),
    ("results", "model", "ALTER TABLE results ADD COLUMN model TEXT"),
    ("images", "thumb_path", "ALTER TABLE images ADD COLUMN thumb_path TEXT"),
    ("images", "proxy_path", "ALTER TABLE images ADD COLUMN proxy_path TEXT"),
    ("results", "thumb_path", "ALTER TABLE results ADD COLUMN thumb_path TEXT"),
];

fn has_column(db: &Connection, table: &str, col: &str) -> rusqlite::Result<bool> {
    let mut st = db.prepare("SELECT 1 FROM pragma_table_info(?) WHERE name=?")?;
    st.exists((table, col))
}

/// 历史脏值修复：早期写侧用 UTC 而列默认是 localtime，被碰过的行会出现
/// updated_at 比 created_at 还早 8 小时，首页按字符串排序就把它压在没碰过的下面。
/// 判据只用"物理上不成立"那一种，跑几次都幂等。
fn heal_stale_timestamps(db: &Connection) -> rusqlite::Result<()> {
    for table in ["projects", "presets"] {
        let sql = format!(
            "UPDATE {table} SET updated_at = datetime(updated_at, 'localtime') \
             WHERE updated_at IS NOT NULL AND updated_at < created_at"
        );
        let n = db.execute(&sql, [])?;
        if n > 0 {
            println!("  已纠正 {table} 里 {n} 行\"更新时间早于创建时间\"的历史值（旧版本按 UTC 写入）");
        }
    }
    Ok(())
}

pub fn open(data_dir: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(data_dir.join("app.db"))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    db.execute_batch(SCHEMA)?;
    for (table, col, ddl) in MIGRATIONS {
        if !has_column(&db, table, col)? {
            db.execute(ddl, [])?;
        }
    }
    heal_stale_timestamps(&db)?;
    Ok(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(db: &Connection) -> Vec<String> {
        let mut st = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    #[test]
    fn 建表与补列跑通() {
        let dir = std::env::temp_dir().join(format!("synco-dbtest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = open(&dir).unwrap();
        let t = tables(&db);
        for want in ["app_settings", "backends", "images", "presets", "projects", "results"] {
            assert!(t.iter().any(|x| x == want), "缺表 {want}，实有 {t:?}");
        }
        assert!(has_column(&db, "results", "backend").unwrap());
        assert!(has_column(&db, "results", "rerun_of").unwrap());
        // 再开一次必须幂等（迁移循环不能重复 ALTER 报错）
        drop(db);
        let db2 = open(&dir).unwrap();
        assert_eq!(tables(&db2).len(), 6);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 历史脏时间被纠正_健康行不动() {
        let dir = std::env::temp_dir().join(format!("synco-dbmig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = open(&dir).unwrap();
        db.execute(
            "INSERT INTO projects(id, name, created_at, updated_at) VALUES (1,'脏','2026-10-01 10:00:00','2026-10-01 02:00:00')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO projects(id, name, created_at, updated_at) VALUES (2,'健康','2026-10-01 10:00:00','2026-10-02 09:00:00')",
            [],
        )
        .unwrap();
        heal_stale_timestamps(&db).unwrap();
        // 期望值用 SQLite 自己算，别把本机时区写死进断言
        let want: String = db
            .query_row("SELECT datetime('2026-10-01 02:00:00','localtime')", [], |r| r.get(0))
            .unwrap();
        let after: String = db
            .query_row("SELECT updated_at FROM projects WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, want, "脏值没被纠正成本机时间");
        let healthy: String = db
            .query_row("SELECT updated_at FROM projects WHERE id=2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(healthy, "2026-10-02 09:00:00", "健康行不该被动");
        std::fs::remove_dir_all(&dir).ok();
    }
}
