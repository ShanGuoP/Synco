//! SQLite：建表与迁移。列名、默认值与迁移顺序都得跟着已经发布出去的 `app.db` 走——
//! 库里存着历史行，改一处就会让老库读不出东西。

use rusqlite::Connection;
use std::path::Path;

/// 建表语句一次写完。0.3 本地精修那两张附属表（image_adjust / image_face）都是"跟着图走"的，
/// 删图时由 api::images::image_delete 逐表清（与 results 同一惯例，不靠 REFERENCES）。
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
  status TEXT DEFAULT 'running', error TEXT, error_args TEXT, prompt_id TEXT,
  prompt TEXT, steps INTEGER, cfg REAL, seed INTEGER, orig_path TEXT,
  final_path TEXT, crop_path TEXT, maskoverlay_path TEXT,
  created_at TEXT DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS app_settings(key TEXT PRIMARY KEY, value TEXT);
/* 内部执行规格单独存放，SELECT results 与接口 DTO 不会带出凭据或输入字节。 */
CREATE TABLE IF NOT EXISTS job_specs(
  result_id INTEGER PRIMARY KEY, spec_json TEXT NOT NULL, mask_png BLOB);
CREATE TRIGGER IF NOT EXISTS delete_job_spec AFTER DELETE ON results
BEGIN DELETE FROM job_specs WHERE result_id=OLD.id; END;
CREATE TRIGGER IF NOT EXISTS release_job_spec AFTER UPDATE OF status ON results
WHEN NEW.status IN ('done','error')
BEGIN DELETE FROM job_specs WHERE result_id=NEW.id; END;
CREATE TABLE IF NOT EXISTS backends(
  url TEXT PRIMARY KEY, label TEXT,
  added_at TEXT DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS presets(
  id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, project_id INTEGER,
  kind TEXT DEFAULT 'preset',
  prompt TEXT DEFAULT '', negative TEXT DEFAULT '', steps INTEGER, cfg REAL,
  loras_json TEXT DEFAULT '[]',
  created_at TEXT DEFAULT (datetime('now','localtime')), updated_at TEXT);
CREATE TABLE IF NOT EXISTS image_adjust(
  image_id INTEGER PRIMARY KEY, ops TEXT NOT NULL,
  updated_at TEXT DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS image_face(
  image_id INTEGER, idx INTEGER, box TEXT, landmarks TEXT, score REAL,
  updated_at TEXT DEFAULT (datetime('now','localtime')),
  PRIMARY KEY(image_id, idx));
CREATE TABLE IF NOT EXISTS image_landmark(
  image_id INTEGER, idx INTEGER, points TEXT,
  updated_at TEXT DEFAULT (datetime('now','localtime')),
  PRIMARY KEY(image_id, idx));

/* 按项目/按图聚合结果行用的（项目页角标、派生弹窗）。表一直没有任何索引，
   这两条新查询是每个项目一次全表扫，加了才够快 */
CREATE INDEX IF NOT EXISTS idx_results_image ON results(image_id);
CREATE INDEX IF NOT EXISTS idx_results_project ON results(project_id);
"#;

/// 旧库补列：前五条是历史库里已有的列，中间三条是 M3 图像服务化要读的派生档，
/// 再后面分别是提示词短语分桶、画稿/照片分桶，和这一批的**派生谱系 + 画稿快照**。
/// 逐条按"列在不在"判重，所以老库直接升上来就行，不需要重建。
const MIGRATIONS: [(&str, &str, &str); 17] = [
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
    // 「另存为新图」造的子图：父图与来源结果行。以前只把 `派生{rid}` 写进**文件名**，
    // 于是"这张图派生出了哪几张"只能靠改名字猜，改名就断（项目页那个派生入口要靠它）
    ("images", "derived_from", "ALTER TABLE images ADD COLUMN derived_from INTEGER"),
    ("images", "derived_result", "ALTER TABLE images ADD COLUMN derived_result INTEGER"),
    // 画布每一版生成时的线稿快照：不然用户接着画两笔，就再也回不到"出这张图时我画的是什么"
    ("results", "sketch_path", "ALTER TABLE results ADD COLUMN sketch_path TEXT"),
    // 报错不再把某一语言的句子焊进库：error 存钥匙、这一列存参数，界面按当前语言查字典。
    // 老行的 error 本身就是一句话，查不到钥匙就原样显示，所以不需要回填。
    ("results", "error_args", "ALTER TABLE results ADD COLUMN error_args TEXT"),
    // 无损成图不再整张存盘（24MP 一张 PNG ≈ 21 MB）。这三样是"能把那一张重算出来"的最小集：
    // 模型回来的窗口原图（未经任何二次编码）、这一行提交那一刻的遮罩、当时那套缝合与调整参数
    // （里面带着 `stitch_core::RULES_V`）。缺任一件就重算不出——那一行就保住它现有的那一份。
    ("results", "raw_path", "ALTER TABLE results ADD COLUMN raw_path TEXT"),
    ("results", "mask_snap_path", "ALTER TABLE results ADD COLUMN mask_snap_path TEXT"),
    ("results", "snap_json", "ALTER TABLE results ADD COLUMN snap_json TEXT"),
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
    // i18n-keep 这个名字写进库里的 name 列：它是用户数据，换语言不该改写已经存着的项目名
    const BLANK_NAME: &str = "未命名项目"; // i18n-keep
    let n = db.execute(
        "UPDATE projects SET name=? WHERE name IS NULL OR trim(name, ' ' || char(9) || char(10) || char(13))=''",
        [BLANK_NAME],
    )?;
    if n > 0 {
        println!("  已给 projects 里 {n} 行空名字补上兜底显示名（早期版本没 trim 就落库）");
    }
    Ok(())
}

/// 从文件名尾部取出来源结果号：`a 派生123.png` → 123。认不出就 None，不猜。
/// 「派生2版.png」这种手工改过的名字必须返回 None——把 2 当成结果号就会挂到一张无关的图上。
fn derived_rid(name: &str) -> Option<i64> {
    let tail = name.rsplit_once("派生")?.1; // i18n-keep 认的是老库文件名里的来源标记，不是界面文案
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 18 {
        return None;
    }
    let rest = &tail[digits.len()..];
    if rest.is_empty() || rest.starts_with('.') {
        digits.parse::<i64>().ok()
    } else {
        None
    }
}

/// 历史脏值修复：早期「另存为新图」只把来源写进**文件名**（`… 派生{结果号}.png`），
/// 库里没有父子关系，于是"这张图派生出了哪几张"只能靠改名字猜、改个名就断。
/// 这一条把名字里那个结果号反查回 `results.image_id` 落成两列真值。判据幂等：
/// 只动 `derived_from IS NULL`、名字解析得出、且那条结果行确实存在、父图不是自己的行。
fn heal_derived_from_names(db: &Connection) -> rusqlite::Result<()> {
    let mut rows: Vec<(i64, String)> = Vec::new();
    {
        let mut st = db.prepare("SELECT id, name FROM images WHERE derived_from IS NULL AND name LIKE '%派生%'")?; // i18n-keep 同上
        let mut it = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        while let Some(v) = it.next() {
            rows.push(v?);
        }
    }
    let mut n = 0;
    for (id, name) in rows {
        let Some(rid) = derived_rid(&name) else { continue };
        let parent = db
            .query_row("SELECT image_id FROM results WHERE id=?", [rid], |r| r.get::<_, Option<i64>>(0))
            .unwrap_or(None);
        let Some(pid) = parent.filter(|p| *p != id) else { continue };
        if db.execute("UPDATE images SET derived_from=?, derived_result=? WHERE id=?", (pid, rid, id))? > 0 {
            n += 1;
        }
    }
    if n > 0 {
        println!("  已按文件名回填 {n} 张派生图的父子关系（早期版本只把来源写进名字里）");
    }
    Ok(())
}

/// 修图界面那一排提示词短语的出厂 10 条。以前它们是写死在前端 JS 里的字面量，
/// 用户想加一条只能改代码；现在落到库里（kind='phrase'），前端从接口读。
///
/// 句子按三条规矩写：一条只讲一个目标状态（胶囊是并进同一句话的，堆三四条以后模型抓不住重点）；
/// 正向里只描述"要成为什么样"，排除式说法交给负面词框——两个模型的官方写法指南都把这一条分开；
/// 皮肤、妆容、表情这几类必须自带"轻微/自然"的限定，缺了限定模型会把它做成另一个人。
/// i18n-keep 这些是写进库里的用户数据（短语名 + 发给模型的提示词），不是界面文案；
/// 换语言去改种子，等于把已经躺在库里那些行的出处也改掉。
const DEFAULT_PHRASES: &[(&str, &str)] = &[
    ("皮肤质感", "皮肤保留真实毛孔与绒毛质感，光泽柔和，油光与暗沉自然减轻"), // i18n-keep
    ("妆容清淡", "妆面轻薄自然，唇色与腮红向皮肤柔和过渡，眼妆层次清晰，与人物原有气质一致"), // i18n-keep
    ("去碎发", "碎发收进主发束，发际线与鬓角整齐利落，露出的头皮与周围发色自然渐变"), // i18n-keep
    ("换发型", "发际线与鬓角过渡自然，发量与头型比例协调，高光沿发束走向连续，与脸型相称"), // i18n-keep
    ("服装平整", "衣料褶皱走向与身体姿态相符，面料纹理与印花清晰对位，缝线与版型保持原设计"), // i18n-keep
    ("补空位", "被移除处由相邻地面与背景连续补齐，透视、纹理、色温与颗粒感和周围一致"), // i18n-keep
    ("光影统一", "光源方向、阴影长度与色温延续画面其余部分，受光面过渡连续，明暗反差保持原片水平"), // i18n-keep
    ("手部修正", "五指结构完整，指节弯曲符合抓握受力，指甲与手掌肤色一致，手与人物比例相称"), // i18n-keep
    ("只改遮罩区", "只在遮罩覆盖处生成，遮罩外的五官比例、肤色、发型、表情、服装与背景原样保留"), // i18n-keep
    ("只改遮罩区（英文）", "Change only the masked area. Keep the person's identity, facial proportions, skin tone, hairstyle, expression, outfit, pose, lighting and background exactly the same. Photorealistic, natural skin texture. No added text, watermarks or extra objects."), // i18n-keep
];

/// 上一版那 7 条的 (旧名, 旧句) → 本版 (新名, 新句)。
/// 只改**字节完全等于旧出厂句**的行：用户润色过的一个字都不动——那是他自己写的话，
/// 覆盖它就不是升级默认值，是覆盖用户数据。
const PHRASE_UPGRADES: &[(&str, &str, &str, &str)] = &[
    ("皮肤精修", "皮肤质感细腻通透，保留毛孔与绒毛细节", "皮肤质感", "皮肤保留真实毛孔与绒毛质感，光泽柔和，油光与暗沉自然减轻"), // i18n-keep
    ("去碎发", "去除杂乱碎发，发际线与鬓角干净", "去碎发", "碎发收进主发束，发际线与鬓角整齐利落，露出的头皮与周围发色自然渐变"), // i18n-keep
    ("服装平整", "服装褶皱自然平整，材质纹理清晰", "服装平整", "衣料褶皱走向与身体姿态相符，面料纹理与印花清晰对位，缝线与版型保持原设计"), // i18n-keep
    ("背景干净", "背景杂物与高光溢出消除，画面干净", "补空位", "被移除处由相邻地面与背景连续补齐，透视、纹理、色温与颗粒感和周围一致"), // i18n-keep
    ("光影统一", "光线柔和统一，与周围环境色温一致", "光影统一", "光源方向、阴影长度与色温延续画面其余部分，受光面过渡连续，明暗反差保持原片水平"), // i18n-keep
    ("手部修正", "手指结构与数量正确，关节自然", "手部修正", "五指结构完整，指节弯曲符合抓握受力，指甲与手掌肤色一致，手与人物比例相称"), // i18n-keep
    ("只改遮罩区", "只编辑遮罩区域，其余保持原样", "只改遮罩区", "只在遮罩覆盖处生成，遮罩外的五官比例、肤色、发型、表情、服装与背景原样保留"), // i18n-keep
];

/// 播种一次就够：`phrases_seeded` 记在 app_settings 里，
/// 用户在管理面板把整排全删了，下次启动不该又长回来。
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

/// 把出厂句子的改版落到老库上。只跑一次（`phrases_v2`），且只动没被用户改过的那些行；
/// 新增的几条在整排被删空的库里**不补**——删空是明确意愿，不该借升级塞回去。
fn upgrade_phrases(db: &Connection) -> rusqlite::Result<()> {
    let done = db
        .query_row("SELECT 1 FROM app_settings WHERE key='phrases_v2'", [], |_| Ok(true))
        .unwrap_or(false);
    if done {
        return Ok(());
    }
    let mut n = 0;
    for (old_name, old_text, new_name, new_text) in PHRASE_UPGRADES {
        // 表上没有 name 唯一约束，硬改会把两个同名胶囊摆进同一排；目标名被人占着就留着旧行让他自己处置
        let taken: i64 = db.query_row(
            "SELECT COUNT(*) FROM presets WHERE kind='phrase' AND name=? AND name<>?",
            [*new_name, *old_name],
            |r| r.get(0),
        )?;
        if taken == 0 {
            n += db.execute(
                "UPDATE presets SET name=?, prompt=? WHERE kind='phrase' AND name=? AND prompt=?",
                [*new_name, *new_text, *old_name, *old_text],
            )? as i64;
        }
    }
    let alive: i64 = db.query_row("SELECT COUNT(*) FROM presets WHERE kind='phrase'", [], |r| r.get(0))?;
    if alive > 0 {
        for (label, text) in DEFAULT_PHRASES {
            let hit = db
                .query_row("SELECT 1 FROM presets WHERE name=? AND kind='phrase'", [*label], |_| Ok(true))
                .unwrap_or(false);
            if !hit {
                db.execute("INSERT INTO presets(name, kind, prompt) VALUES(?, 'phrase', ?)", (*label, *text))?;
                n += 1;
            }
        }
    }
    db.execute("INSERT OR REPLACE INTO app_settings(key, value) VALUES('phrases_v2', '1')", [])?;
    if n > 0 {
        println!("  已按新写法更新 {n} 条出厂提示词短语（你自己改过的没动）");
    }
    Ok(())
}

pub fn open(data_dir: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(data_dir.join("app.db"))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    // 跨进程等锁：本进程内靠那把 Mutex，但同一个库被第二个进程打开时（`synco-tools` 的体检与补齐
    // 就在外面跑），没有这一句就是一撞 WAL 写锁立刻 SQLITE_BUSY——那句报错只有开发者读得懂
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch(SCHEMA)?;
    for (table, col, ddl) in MIGRATIONS {
        if !has_column(&db, table, col)? {
            db.execute(ddl, [])?;
        }
    }
    // 派生谱系的索引建在补列**之后**：新库的 CREATE TABLE 里没有 derived_from，
    // 放在 SCHEMA 里会在 open() 第一步就 "no such column" 把整个库卡住
    db.execute_batch("CREATE INDEX IF NOT EXISTS idx_images_derived ON images(derived_from);")?;
    heal_stale_timestamps(&db)?;
    heal_blank_names(&db)?;
    heal_derived_from_names(&db)?;
    seed_phrases(&db)?;
    upgrade_phrases(&db)?;
    Ok(db)
}

/// 资料页要的两条计数。**取一次就撒手**：连接全局只有一个，握着它去遍历整棵 `data/`
/// （几十 GB 瓦片走一遍）期间，所有走库的 HTTP handler——含 2.5s 一次的队列轮询——都在排队。
pub fn library_counts(ctx: &crate::state::Ctx) -> (i64, i64) {
    let db = ctx.db();
    let count = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    (count("SELECT count(*) FROM projects"), count("SELECT count(*) FROM images"))
}

/// 把 WAL 合回主文件。复制资料目录前必须走一次，否则拷过去的是半个库 + 一份没人认领的 `-wal`。
pub fn checkpoint_truncate(ctx: &crate::state::Ctx) {
    let _ = ctx.db().execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
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
        for want in ["app_settings", "backends", "images", "presets", "projects", "results", "job_specs", "image_adjust", "image_face", "image_landmark"] {
            assert!(t.iter().any(|x| x == want), "缺表 {want}，实有 {t:?}");
        }
        assert!(has_column(&db, "results", "backend").unwrap());
        assert!(has_column(&db, "results", "rerun_of").unwrap());
        // 再开一次必须幂等（迁移循环不能重复 ALTER 报错）
        drop(db);
        let db2 = open(&dir).unwrap();
        assert_eq!(tables(&db2).len(), 10);
        // Windows 上句柄还开着就删不掉：连接必须先撒手，否则这句静默失败，每次跑测试留一个目录
        drop(db2);
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
        drop(db);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 派生谱系：早期只把来源写进文件名，回填要能挂上父子，且**认不出的一律不猜**
    #[test]
    fn 派生名字回填父子_认不出的不动() {
        let dir = std::env::temp_dir().join(format!("synco-dbderive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = open(&dir).unwrap();
        db.execute("INSERT INTO projects(id,name) VALUES (1,'p')", []).unwrap();
        db.execute(
            "INSERT INTO images(id,project_id,name,orig_path,w,h) VALUES
              (3,1,'PLQ.jpg','a',1,1), (4,1,'PLQ 派生7.png','b',1,1), (5,1,'手改 派生2版.png','c',1,1),
              (6,1,'没 派生 数字.png','d',1,1), (7,1,'孤儿 派生88.png','e',1,1), (8,1,'自派生 派生9.png','f',1,1)",
            [],
        )
        .unwrap();
        // 7 号结果行属于图 3；9 号结果行属于图 8（自己派生自己，不该挂）
        db.execute("INSERT INTO results(id,image_id,project_id,status) VALUES (7,3,1,'done'), (9,8,1,'done')", [])
            .unwrap();
        heal_derived_from_names(&db).unwrap();
        let got = |id: i64| -> (Option<i64>, Option<i64>) {
            db.query_row("SELECT derived_from, derived_result FROM images WHERE id=?", [id], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
        };
        assert_eq!(got(4), (Some(3), Some(7)), "名字里那个结果号该挂到结果行的图上");
        assert_eq!(got(5), (None, None), "「派生2版」不是结果号，挂上就错了");
        assert_eq!(got(6), (None, None), "没有数字的不该动");
        assert_eq!(got(7), (None, None), "结果行不存在就不猜来源");
        assert_eq!(got(8), (None, None), "父图算成自己的不算派生");
        // 幂等：第二次一条都不该再改
        heal_derived_from_names(&db).unwrap();
        assert_eq!(got(4), (Some(3), Some(7)));
        drop(db);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 派生号解析只认尾部纯数字() {
        assert_eq!(derived_rid("a 派生7.png"), Some(7));
        assert_eq!(derived_rid("a 派生7"), Some(7));
        assert_eq!(derived_rid("a 派生12 派生34.png"), Some(34), "名字里出现两次时取最后一段");
        assert_eq!(derived_rid("a 派生.png"), None);
        assert_eq!(derived_rid("a 派生2版.png"), None);
        assert_eq!(derived_rid("a 派生-3.png"), None);
        assert_eq!(derived_rid("普通照片.png"), None);
    }

    fn phrase_count(db: &Connection) -> i64 {        db.query_row("SELECT COUNT(*) FROM presets WHERE kind='phrase'", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn 短语播种一次_删光不再长回来() {
        let dir = std::env::temp_dir().join(format!("synco-dbphrase-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = open(&dir).unwrap();
        assert_eq!(phrase_count(&db), 10, "出厂 10 条短语没播进去");
        assert!(has_column(&db, "presets", "kind").unwrap());
        drop(db);
        // 第二次开库不能重复插（每次启动都加一遍会越堆越多）
        let db2 = open(&dir).unwrap();
        assert_eq!(phrase_count(&db2), 10, "重开一次就重复播种了");
        // 用户在管理面板里删光，属于明确意愿，不该下次启动又复活
        db2.execute("DELETE FROM presets WHERE kind='phrase'", []).unwrap();
        seed_phrases(&db2).unwrap();
        assert_eq!(phrase_count(&db2), 0, "删光的短语被重新播出来了");
        drop(db2);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 出厂句子改版怎么落到已经存在的库上：没动过的跟着改，动过的一个字不碰，
    /// 整排被删空的也不借升级塞回去。
    #[test]
    fn 出厂短语升级_只改没动过的那几条() {
        let dir = std::env::temp_dir().join(format!("synco-dbphup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 先建库，再把它摆成"上一版已经播过种"的样子
        let db = open(&dir).unwrap();
        db.execute("DELETE FROM presets WHERE kind='phrase'", []).unwrap();
        db.execute("DELETE FROM app_settings WHERE key='phrases_v2'", []).unwrap();
        for (old_name, old_text, _, _) in PHRASE_UPGRADES {
            db.execute("INSERT INTO presets(name, kind, prompt) VALUES(?, 'phrase', ?)", [*old_name, *old_text]).unwrap();
        }
        // 其中「光影统一」被用户改成自己的话，「妆容清淡」是他自己建的同名条
        db.execute("UPDATE presets SET prompt='光线再暖一点' WHERE name='光影统一' AND kind='phrase'", []).unwrap();
        db.execute("INSERT INTO presets(name, kind, prompt) VALUES('妆容清淡','phrase','用户自己写的那一句')", []).unwrap();
        drop(db);

        let db2 = open(&dir).unwrap();
        let got = |n: &str| -> Option<String> {
            db2.query_row("SELECT prompt FROM presets WHERE name=? AND kind='phrase'", [n], |r| r.get(0)).ok()
        };
        assert_eq!(phrase_count(&db2), 10, "升级后该有 10 条");
        assert_eq!(got("妆容清淡").as_deref(), Some("用户自己写的那一句"), "他自建的同名条被动了或被插成两条");
        for (old_name, _, new_name, new_text) in PHRASE_UPGRADES {
            if *old_name == "光影统一" {
                continue;   // 这一条用户动过，走下面的单独断言
            }
            assert_eq!(got(new_name).as_deref(), Some(*new_text), "「{new_name}」没跟着升级");
            // 只有真改了名的才该消失，改名那两条以外旧名就是新名
            if old_name != new_name {
                assert_eq!(got(old_name), None, "旧名「{old_name}」该被换掉");
            }
        }
        assert_eq!(got("光影统一").as_deref(), Some("光线再暖一点"), "用户改过的句子被动了");
        for (label, _) in DEFAULT_PHRASES {
            assert!(got(label).is_some(), "升级后少了「{label}」");
        }
        drop(db2);

        // 幂等：再开一次不该又插一遍，也不该把用户那句话盖回去
        let db3 = open(&dir).unwrap();
        assert_eq!(phrase_count(&db3), 10, "重开一次就重复补种了");
        assert_eq!(
            db3.query_row::<String, _, _>("SELECT prompt FROM presets WHERE name='光影统一' AND kind='phrase'", [], |r| r.get(0)).unwrap(),
            "光线再暖一点"
        );
        db3.execute("DELETE FROM presets WHERE kind='phrase'", []).unwrap();
        db3.execute("DELETE FROM app_settings WHERE key='phrases_v2'", []).unwrap();
        drop(db3);

        // 删空 = 明确意愿，升级不补
        let db4 = open(&dir).unwrap();
        assert_eq!(phrase_count(&db4), 0, "删空的短语被升级补回来了");
        drop(db4);
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
        drop(db);
        std::fs::remove_dir_all(&dir).ok();
    }
}
