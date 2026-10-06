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
  kind TEXT DEFAULT 'preset',
  prompt TEXT DEFAULT '', negative TEXT DEFAULT '', steps INTEGER, cfg REAL,
  loras_json TEXT DEFAULT '[]',
  created_at TEXT DEFAULT (datetime('now','localtime')), updated_at TEXT);

/* 按项目/按图聚合结果行用的（项目页角标、派生弹窗）。表一直没有任何索引，
   这两条新查询是每个项目一次全表扫，加了才够快 */
CREATE INDEX IF NOT EXISTS idx_results_image ON results(image_id);
CREATE INDEX IF NOT EXISTS idx_results_project ON results(project_id);
"#;

/// 旧库补列：前五条是历史库里已有的列，中间三条是 M3 图像服务化要读的派生档，
/// 后两条分别是提示词短语分桶（presets.kind）与画稿/照片分桶（images.kind）。
/// 逐条按"列在不在"判重，所以老库直接升上来就行，不需要重建。
const MIGRATIONS: [(&str, &str, &str); 10] = [
    ("results", "prompt_id", "ALTER TABLE results ADD COLUMN prompt_id TEXT"),
    ("results", "settings_json", "ALTER TABLE results ADD COLUMN settings_json TEXT"),
    ("results", "rerun_of", "ALTER TABLE results ADD COLUMN rerun_of INTEGER"),
    ("results", "backend", "ALTER TABLE results ADD COLUMN backend TEXT DEFAULT 'comfyui'"),
    ("results", "model", "ALTER TABLE results ADD COLUMN model TEXT"),
    ("images", "thumb_path", "ALTER TABLE images ADD COLUMN thumb_path TEXT"),
    ("images", "proxy_path", "ALTER TABLE images ADD COLUMN proxy_path TEXT"),
    ("results", "thumb_path", "ALTER TABLE results ADD COLUMN thumb_path TEXT"),
    ("presets", "kind", "ALTER TABLE presets ADD COLUMN kind TEXT DEFAULT 'preset'"),
    ("images", "kind", "ALTER TABLE images ADD COLUMN kind TEXT DEFAULT 'photo'"),
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

/// 历史脏值修复：早期建项目只裁长度不 trim，纯空格能当名字存进去，
/// 首页就出现看不见的项目、搜索也搜不到。改名入口上线时一并清一次，判据幂等。
fn heal_blank_names(db: &Connection) -> rusqlite::Result<()> {
    let n = db.execute(
        "UPDATE projects SET name='未命名项目' \
         WHERE name IS NULL OR trim(name, ' ' || char(9) || char(10) || char(13))=''",
        [],
    )?;
    if n > 0 {
        println!("  已给 projects 里 {n} 行空名字补上兜底显示名（早期版本没 trim 就落库）");
    }
    Ok(())
}

/// 修图界面那一排提示词短语的出厂 7 条。以前它们是写死在前端 JS 里的字面量，
/// 用户想加一条只能改代码；现在落到库里（kind='phrase'），前端从接口读。
const DEFAULT_PHRASES: &[(&str, &str)] = &[
    ("皮肤精修", "皮肤质感细腻通透，保留毛孔与绒毛细节"),
    ("去碎发", "去除杂乱碎发，发际线与鬓角干净"),
    ("服装平整", "服装褶皱自然平整，材质纹理清晰"),
    ("背景干净", "背景杂物与高光溢出消除，画面干净"),
    ("光影统一", "光线柔和统一，与周围环境色温一致"),
    ("手部修正", "手指结构与数量正确，关节自然"),
    ("只改遮罩区", "只编辑遮罩区域，其余保持原样"),
];

/// 播种一次就够：`phrases_seeded` 记在 app_settings 里，
/// 用户在管理面板把 7 条全删了，下次启动不该又长回来。
fn seed_phrases(db: &Connection) -> rusqlite::Result<()> {
    let seeded = db
        .query_row("SELECT 1 FROM app_settings WHERE key='phrases_seeded'", [], |_| Ok(true))
        .unwrap_or(false);
    if seeded {
        return Ok(());
    }
    let mut n = 0;
    for (label, text) in DEFAULT_PHRASES {
        let hit = db
            .query_row("SELECT 1 FROM presets WHERE name=? AND kind='phrase'", [*label], |_| Ok(true))
            .unwrap_or(false);
        if !hit {
            db.execute("INSERT INTO presets(name, kind, prompt) VALUES(?, 'phrase', ?)", (*label, *text))?;
            n += 1;
        }
    }
    db.execute("INSERT OR REPLACE INTO app_settings(key, value) VALUES('phrases_seeded', '1')", [])?;
    if n > 0 {
        println!("  已把 {n} 条默认提示词短语写进库，之后它们在「设置 → 提示词短语」里可增删改");
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
    heal_blank_names(&db)?;
    seed_phrases(&db)?;
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
    fn 空项目名补兜底_正常名不动() {
        let dir = std::env::temp_dir().join(format!("synco-dbname-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = open(&dir).unwrap();
        db.execute(
            "INSERT INTO projects(id, name) VALUES (1,'   '), (2,'0925 漫展'), (3,NULL)",
            [],
        )
        .unwrap();
        heal_blank_names(&db).unwrap();
        let got = |id: i64| -> String {
            db.query_row("SELECT name FROM projects WHERE id=?", [id], |r| r.get(0)).unwrap()
        };
        assert_eq!(got(1), "未命名项目", "纯空格名没被兜住");
        assert_eq!(got(3), "未命名项目", "NULL 名没被兜住");
        assert_eq!(got(2), "0925 漫展", "正常名不该被动");
        // 幂等：第二次不该再有改动
        assert_eq!(heal_blank_names(&db).unwrap(), ());
        assert_eq!(got(2), "0925 漫展");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn phrase_count(db: &Connection) -> i64 {
        db.query_row("SELECT COUNT(*) FROM presets WHERE kind='phrase'", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn 短语播种一次_删光不再长回来() {
        let dir = std::env::temp_dir().join(format!("synco-dbphrase-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = open(&dir).unwrap();
        assert_eq!(phrase_count(&db), 7, "出厂 7 条短语没播进去");
        assert!(has_column(&db, "presets", "kind").unwrap());
        drop(db);
        // 第二次开库不能重复插（每次启动都加一遍会越堆越多）
        let db2 = open(&dir).unwrap();
        assert_eq!(phrase_count(&db2), 7, "重开一次就重复播种了");
        // 用户在管理面板里删光，属于明确意愿，不该下次启动又复活
        db2.execute("DELETE FROM presets WHERE kind='phrase'", []).unwrap();
        seed_phrases(&db2).unwrap();
        assert_eq!(phrase_count(&db2), 0, "删光的短语被重新播出来了");
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
