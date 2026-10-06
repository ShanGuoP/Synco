//! 工坊运维工具（M5）：把原来三个双击脚本要干的事收进一个 exe。
//!
//! `synco-tools.exe` 不带参数就是菜单：每轮先打一遍实时状态，再让你选做哪件。
//! 原来的 `启动修图Demo.bat` / `停止端口服务.bat` 与权重锁定脚本都归到这里，
//! .bat 只留"双击进菜单"一层壳——cmd 用 OEM 代码页解析，中文一律不进 bat。

use synco_server::error::{AppError, Result};
use synco_server::service::setup;
use synco_server::util;
use serde_json::Value;
use std::io::{IsTerminal, Read};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fs};


/// 用哪个端口：SYNCO_PORT 优先，没设就是 7861
fn want() -> u16 { synco_server::want_port() }

/// 数据目录：与 `data_dir()` 同一套判据，但这里允许只读打开
fn data_dir() -> PathBuf {
    if let Some(p) = env::var_os("SYNCO_DATA") {
        return PathBuf::from(p);
    }
    synco_server::data_dir()
}

fn exe_dir() -> PathBuf {
    env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| PathBuf::from("."))
}

/// 同目录的服务端 exe；打包态就是 synco-desktop.exe 自己
fn server_exe() -> PathBuf {
    let d = exe_dir();
    for name in ["synco.exe", "Synco.exe", "synco-desktop.exe"] {
        let p = d.join(name);
        if p.is_file() {
            return p;
        }
    }
    d.join("synco.exe")
}

/// workspace 根：exe 在 target/<profile>/ 下时往上三层就是仓库根
fn workspace_root() -> PathBuf {
    if let Some(p) = env::var_os("SYNCO_WORKSPACE") {
        return PathBuf::from(p);
    }
    let d = exe_dir();
    for up in 1..=4 {
        let cand = match (0..up).try_fold(d.as_path(), |acc, _| acc.parent()) {
            Some(p) => p,
            None => break,
        };
        if cand.join("crates").join("server").join("defaults.json").is_file() {
            return cand.to_path_buf();
        }
    }
    d
}

// ---------------------------------------------------------------- 端口与进程
/// 从菜单里反复 spawn 控制台程序（netstat/tasklist/taskkill）会一个黑框一闪一闪。
/// CREATE_NO_WINDOW 给它一个不显示的控制台：拿得到输出，又不开新窗口。
fn quiet(mut cmd: Command) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn port_taken(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpListener::bind(addr).is_err()
}

fn free_port() -> u16 {
    TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(0)
}

/// netstat 里找"在这个端口上 LISTENING 的 PID"
fn pids_on(port: u16) -> Vec<u32> {
    let out = quiet(Command::new("netstat")).args(["-ano", "-p", "TCP"]).output().map(|o| o.stdout).unwrap_or_default();
    let text = String::from_utf8_lossy(&out);
    let needle = format!(":{port}");
    let mut v = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 || !cols[0].eq_ignore_ascii_case("TCP") || !cols[3].eq_ignore_ascii_case("LISTENING") {
            continue;
        }
        if cols[1].ends_with(&needle) {
            if let Ok(pid) = cols[4].parse::<u32>() {
                if !v.contains(&pid) {
                    v.push(pid);
                }
            }
        }
    }
    v
}

fn who(pid: u32) -> String {
    let out = quiet(Command::new("tasklist"))
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    // "synco.exe","1234","Console","1","160,000 K"
    out.split("\",\"").next().map(|s| s.trim_matches('"').to_string()).unwrap_or_else(|| "?".into())
}

// ---------------------------------------------------------------- 状态
fn status_line(port: u16) {
    let data = data_dir();
    let taken = port_taken(port);
    let mut db_line = String::from("库还没建（第一次启动服务时会自动建）");
    let mut pending = String::new();
    if let Ok(conn) = rusqlite::Connection::open_with_flags(
        data.join("app.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(-1);
        let (p, i, running, nothumb) = (
            count("SELECT count(*) FROM projects"),
            count("SELECT count(*) FROM images"),
            count("SELECT count(*) FROM results WHERE status IN ('running','queued')"),
            count("SELECT count(*) FROM images WHERE thumb_path IS NULL"),
        );
        db_line = format!("{p} 个项目 · {i} 张图 · 未落定的生成 {running} 条");
        if nothumb > 0 {
           pending = format!("  还没切出缩略档的图：{nothumb} 张（跑一次「补齐派生档」）");
        }
    }
    println!("  数据目录  {}", util::neat_path(&data));
    println!("  库        {db_line}");
    if taken {
        let pids = pids_on(port);
        let who = pids.iter().map(|p| format!("{p} {}", who(*p))).collect::<Vec<_>>().join("、");
        println!("  端口 {port}  已有人听着：{who}");
    } else {
        println!("  端口 {port}  空着，可以直接启动");
    }
    if !pending.is_empty() {
        println!("{pending}");
    }
    let root = setup_root_hint();
    println!("  ComfyUI   {}（根目录 {root}）", if root.is_empty() { "没设根目录".to_string() } else { String::new() });
}

fn setup_root_hint() -> String {
    let data = data_dir();
    rusqlite::Connection::open_with_flags(data.join("app.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .ok()
        .and_then(|c| c.query_row("SELECT value FROM app_settings WHERE key='comfy_root'", [], |r| r.get::<_, String>(0)).ok())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- 动作
fn start(open: bool) -> Result<()> {
    let exe = server_exe();
    if !exe.is_file() {
        return Err(AppError::Fail(format!("找不到服务端 exe：{}", exe.display())));
    }
    let port = if port_taken(want()) { free_port() } else { want() };
    let data = data_dir();
    fs::create_dir_all(&data).map_err(AppError::Io)?;
    let log = fs::File::create(data.join("server.log")).map_err(AppError::Io)?;
    let mut cmd = Command::new(&exe);
    cmd.stdout(Stdio::from(log.try_clone().map_err(AppError::Io)?)).stderr(Stdio::from(log)).env("SYNCO_DATA", &data).env("SYNCO_PORT", port.to_string());
    // 给服务一个"不显示的控制台"：菜单那个窗口关了不顺手把它带走——"关窗任务继续跑"靠这个
    let child = quiet(cmd).spawn().map_err(|e| AppError::Fail(format!("起进程失败：{e}")))?;
    println!("  已交给 PID {} 在后台跑，日志：{}", child.id(), data.join("server.log").display());
    let mut url_port = port;
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if let Ok(s) = fs::read_to_string(data.join("port.txt")) {
            if let Ok(p) = s.trim().parse::<u16>() {
                url_port = p;
                break;
            }
        }
    }
    let url = format!("http://127.0.0.1:{url_port}/");
    println!("  SYNCO_URL={url}");
    if open {
        let _ = quiet(Command::new("cmd")).args(["/C", "start", "", &url]).stdout(Stdio::null()).spawn();
    }
    Ok(())
}

/// 端口别名：与老脚本一致的三个名字，或直接给端口号
fn resolve_port(s: &str) -> Option<u16> {
    match s.to_ascii_lowercase().as_str() {
        "app" | "synco" | "工坊" | "pixfish" => Some(7861),
        "comfy" | "comfyui" => Some(8188),
        "comfy2" | "minimax" => Some(18188),
        other => other.parse::<u16>().ok().filter(|p| *p > 0),
    }
}

fn stop(target: &str, assume_yes: bool) -> Result<()> {
    let Some(port) = resolve_port(target) else {
        println!("  认不出目标 {target}：可用 app(7861) / comfy(8188) / comfy2(18188)，或直接给端口号");
        return Ok(());
    };
    if port.to_string() != target {
        println!("  目标：{target} → 端口 {port}");
    }
    stop_port(port, assume_yes)
}

/// 停端口上的服务：报清楚是谁在听，确认后才动手
fn stop_port(port: u16, assume_yes: bool) -> Result<()> {
    let pids = pids_on(port);
    if pids.is_empty() {
        println!("  端口 {port} 上没人听着，已经是空的");
        return Ok(());
    }
    println!("  端口 {port} 被这些进程占着：");
    for p in &pids {
        println!("    PID {p:<8} {}", who(*p));
    }
    // 系统端口无条件拒绝：--yes 是"别问我"，不是"连 SMB/RDP 也可以停"。
    // 旁路掉这一条，`stop 445` 就会 taskkill /F 掉系统的 SMB 宿主进程。
    if [135, 137, 138, 139, 445, 3389, 53].contains(&port) {
        return Err(AppError::Fail(format!("端口 {port} 是 Windows 系统服务用的（RPC/SMB/RDP/DNS），拒绝动手")));
    }
    if !assume_yes {
        print!("  确定停掉上面这些进程？[y/N] ");
        flush();
        if !yes(&input()) {
            println!("  算了，什么都没动");
            return Ok(());
        }
    }
    for p in &pids {
        let ok = quiet(Command::new("taskkill")).args(["/PID", &p.to_string(), "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        println!("  {} PID {p}", if ok { "已停" } else { "停不掉" });
    }
    Ok(())
}

/// 切换生产：**先停旧版、再备份、再起新版**。
/// 顺序不能反：旧进程还在写库的时候复制 app.db(+wal)，拷到的是撕裂的库，
/// 而这份备份恰好是唯一的回滚依据——把坏库装回去等于没有回滚。
/// 只动 app.db 这一族文件——照片与成图从来不在 Node/Rust 之间搬，两边读的是同一批路径。
fn switch_prod(open: bool) -> Result<()> {
    let data = data_dir();
    let db = data.join("app.db");
    if !db.is_file() {
        return Err(AppError::Fail(format!("{} 不存在：这不是放着资料的那个数据目录", db.display())));
    }
    println!("  这一步会：停掉 {} 上的旧版 → 备份库 → 用 Rust 版顶同一个端口。", want());
    print!("\n  确认？[y/N] ");
    flush();
    if !yes(&input()) {
        println!("  那先不动，什么都没改");
        return Ok(());
    }
    stop_port(want(), true)?;
    std::thread::sleep(std::time::Duration::from_millis(600));
    let bak = data.join(format!("backup-{}", chrono_free_stamp()));
    // create_dir 而不是 create_dir_all：撞名就说明这一秒已经备份过一次，
    // 静默覆盖会把上一份（可能正是唯一可用的一份）抹掉
    fs::create_dir(&bak).map_err(|_| AppError::Fail(format!("{} 已经在了，不肯覆盖上一份备份", bak.display())))?;
    let mut moved = Vec::new();
    for name in ["app.db", "app.db-wal", "app.db-shm"] {
        let p = data.join(name);
        if p.is_file() {
            fs::copy(&p, bak.join(name)).map_err(AppError::Io)?;
            moved.push(name);
        }
    }
    println!("  已备份 {} → {}（{}）", util::neat_path(&db), util::neat_path(&bak), moved.join("、"));
    if let Err(e) = check_backup(&bak.join("app.db")) {
        return Err(AppError::Fail(format!("备份读不出来，先不起新版：{e}（{} 留着，什么都没动）", util::neat_path(&bak))));
    }
    println!("  回滚就一句：把 {} 里的文件放回原位，再用旧版启动。", util::neat_path(&bak));
    start(open)
}

/// 备份要能当回滚依据，就得先证明它读得开：integrity_check 走一遍只读连接
fn check_backup(p: &Path) -> std::result::Result<(), String> {
    let db = rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打不开：{e}"))?;
    let top: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0)).map_err(|e| format!("验不了：{e}"))?;
    if top != "ok" {
        return Err(format!("integrity_check 说：{}", top.lines().take(3).collect::<Vec<_>>().join(" / ")));
    }
    Ok(())
}

/// 时间戳不用引 chrono：目录名只要单调可排
fn chrono_free_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (d, rem) = (secs / 86400, secs % 86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // 从 1970-01-01 推日号，够人名看得懂就行。带秒是必须的：备份目录同名就等于覆盖上一份
    let days = d as i64;
    let (y, mo, dd) = civil_from_days(days);
    format!("{y:04}{mo:02}{dd:02}-{h:02}{m:02}{s:02}")
}

/// Howard Hinnant 的 civil_from_days：不引日历库也要能打出可读目录名
fn civil_from_days(z0: i64) -> (i64, u32, u32) {
    let z = z0 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 补齐派生档：把 320/3072 两档给存量图切出来
fn backfill() -> Result<()> {
    let data = data_dir();
    fs::create_dir_all(&data).map_err(AppError::Io)?;
    let conn = synco_server::repo::db::open(&data).map_err(AppError::Sql)?;
    let ctx = synco_server::state::Ctx::new(data.clone(), synco_server::public_dir(), conn);
    let n = synco_server::repo::images::list_missing_thumb(&ctx, 100_000)?.len();
    if n == 0 {
        println!("  没有欠档的图，不用补");
        return Ok(());
    }
    println!("  {n} 张图还没切出派生档，开始补（每张一次全分辨率解码，会占满几个核）…");
    let mut done = 0usize;
    let mut bad = 0usize;
    for img in synco_server::repo::images::list_missing_thumb(&ctx, 100_000)? {
        match synco_server::service::imagesvc::derive(&ctx, &img) {
            Ok(true) => done += 1,
            Ok(false) => {}
            Err(e) => {
                bad += 1;
                println!("    跳过 {}：{e}", img.orig_path);
            }
        }
        if (done + bad) % 20 == 0 {
            println!("    已补 {done} / {n}");
        }
    }
    println!("  补完：{done} 张，{bad} 张补不出（原图不在盘上或格式不认识）");
    Ok(())
}

/// 流式算权重的体积与 sha256，写回 crates/server/defaults.json
fn lock_weights(root: Option<String>) -> Result<()> {
    let fp = workspace_root().join("crates").join("server").join("defaults.json");
    if !fp.is_file() {
        return Err(AppError::Fail(format!("找不到 {}", fp.display())));
    }
    let text = fs::read_to_string(&fp).map_err(AppError::Io)?;
    let mut d: Value = serde_json::from_str(&text).map_err(|e| AppError::Fail(format!("defaults.json 读不懂：{e}")))?;
    let root = root.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| {
        let wf = d.get("workflow_sample").and_then(|v| v.as_str()).unwrap_or("").replace('\\', "/");
        match wf.find("/ComfyUI/") {
            Some(i) if i > 0 => wf[..i].to_string(),
            _ => String::new(),
        }
    });
    if root.trim().is_empty() {
        return Err(AppError::Fail("推不出 ComfyUI 根目录：在菜单里先把它填进 comfy_root，或作为参数传进来".into()));
    }
    let list: Vec<(String, String, String)> = {
        let mut v = Vec::new();
        for (k, dir) in [("unet", "diffusion_models"), ("clip", "text_encoders"), ("vae", "vae")] {
            if let Some(rel) = d.get(k).and_then(|x| x.as_str()) {
                v.push((k.to_string(), rel.to_string(), dir.to_string()));
            }
        }
        if let Some(loras) = d.get("loras").and_then(|x| x.as_array()) {
            for (i, l) in loras.iter().enumerate() {
                if let Some(rel) = l.get("name").and_then(|x| x.as_str()) {
                    v.push((format!("lora:{i}"), rel.to_string(), "loras".to_string()));
                }
            }
        }
        v
    };
    let obj = d.as_object_mut().unwrap();
    let src = obj.entry("sources".to_string()).or_insert_with(|| Value::Object(Default::default()));
    for (key, rel, dir) in &list {
        let p = Path::new(&root).join("ComfyUI").join("models").join(dir).join(rel.replace('\\', std::path::MAIN_SEPARATOR_STR));
        if !p.is_file() {
            println!("  找不到 {key}：{}", p.display());
            continue;
        }
        let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        println!("  {key}  {}  {:.2} GB", p.file_name().and_then(|s| s.to_str()).unwrap_or("?"), size as f64 / 1073741824.0);
        let (sha, secs) = hash_file(&p)?;
        let prev = src.get(key).cloned().unwrap_or(Value::Null);
        let changed = prev.get("sha256").and_then(|v| v.as_str()).map(|old| old != sha).unwrap_or(false);
        let mut slot = prev.as_object().cloned().unwrap_or_default();
        slot.insert("file".into(), Value::String(rel.replace('\\', "/")));
        slot.insert("size".into(), Value::from(size as i64));
        slot.insert("sha256".into(), Value::String(sha.clone()));
        src.as_object_mut().unwrap().insert(key.clone(), Value::Object(slot));
        println!(
            "    sha256 {}…{}  用时 {secs}s{}",
            &sha[..12],
            &sha[sha.len() - 6..],
            if changed { "  ⚠ 与上次记录不同，文件被换过了" } else { "" }
        );
    }
    fs::write(&fp, serde_json::to_string_pretty(&d).map_err(|e| e.to_string())? + "\n").map_err(AppError::Io)?;
    println!("\n  已写回 {}", fp.display());
    println!("  注意：这些默认值是编译进 exe 的（include_str!），改完要重新 cargo build --release 才生效");
    Ok(())
}

/// 流式哈希：13GB 的权重不能往内存里塞
fn hash_file(p: &Path) -> std::result::Result<(String, u64), AppError> {

    let t0 = std::time::Instant::now();
    let mut f = fs::File::open(p).map_err(AppError::Io)?;
    let mut h = <sha2::Sha256 as sha2::Digest>::new();
    let mut buf = vec![0u8; 1 << 24];
    let mut done = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(AppError::Io)?;
        if n == 0 {
            break;
        }
        sha2::Digest::update(&mut h, &buf[..n]);
        done += n as u64;
        print!("\r    {:.1} GB", done as f64 / 1073741824.0);
        flush();
    }
    println!();
    let bytes = sha2::Digest::finalize(h);
    Ok((bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(), t0.elapsed().as_secs()))
}

fn flush() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn input() -> String {
    let mut s = String::new();
    if std::io::stdin().read_line(&mut s).is_err() {
        return String::new();
    }
    s.trim().to_string()
}
fn yes(s: &str) -> bool {
    matches!(s.trim(), "y" | "Y" | "是")
}

// ---------------------------------------------------------------- 菜单
fn menu() -> Result<()> {
    let port = want();
    let mut first = true;
    loop {
        if !first {
            println!();
        }
        first = false;
        println!("Synco · 运维菜单");
        status_line(port);
        println!(
            "\n  [1] 启动工坊并打开页面      [2] 只启动，不开页面\n  [3] 停掉端口上的工坊        [4] 重启工坊（先停再起）\n  [5] 补齐派生档（320/3072）  [6] 锁定权重指纹（defaults.json）\n  [7] 设 ComfyUI 根目录       [8] 体检：ComfyUI 目录与权重\n  [9] 切换生产（备份库 → 用 Rust 版顶掉 7861）\n  [0] 退出（正在跑的服务不会被打断）\n"
        );
        print!("  选一件：");
        flush();
        if !std::io::stdin().is_terminal() {
            println!("\n  这个窗口不是交互终端（比如被脚本重定向了），只打了状态就退出");
            return Ok(());
        }
        let pick = input();
        println!();
        match pick.as_str() {
            "1" => start(true)?,
            "2" => start(false)?,
            "3" => stop_port(port, false)?,
            "4" => {
                stop_port(port, false)?;
                start(true)?;
            }
            "5" => backfill()?,
            "6" => lock_weights(Some(ask_root()))?,
            "7" => set_root()?,
            "8" => check()?,
            "9" => switch_prod(true)?,
            "0" | "q" | "Q" => return Ok(()),
            other => println!("  没这一项：{other}"),
        }
    }
}

fn ask_root() -> String {
    print!("  ComfyUI 根目录（回车=用工作流路径推出来的那个）：");
    flush();
    input()
}

fn set_root() -> Result<()> {
    let data = data_dir();
    fs::create_dir_all(&data).map_err(AppError::Io)?;
    let conn = synco_server::repo::db::open(&data).map_err(AppError::Sql)?;
    let ctx = synco_server::state::Ctx::new(data, synco_server::public_dir(), conn);
    let want = ask_root();
    if !want.trim().is_empty() {
        println!("  记下了：{}", setup::set_root(&ctx, &want));
    }
    println!("  当前生效：{}", setup::root_setting(&ctx));
    Ok(())
}

fn check() -> Result<()> {
    let data = data_dir();
    let conn = synco_server::repo::db::open(&data).map_err(AppError::Sql)?;
    let ctx = synco_server::state::Ctx::new(data, synco_server::public_dir(), conn);
    let root = setup::root_setting(&ctx);
    if root.is_empty() {
        println!("  还没设 ComfyUI 根目录（菜单里选 [7]）");
        return Ok(());
    }
    let d = setup::detect(&ctx, &root);
    let rt = d.get("runtime").cloned().unwrap_or(Value::Null);
    let one = |k: &str, label: &str| println!("  {} {label}", if rt.get(k).and_then(Value::as_bool).unwrap_or(false) { "✓" } else { "✗" });
    one("comfyui", "ComfyUI 主目录");
    one("python", "内嵌 Python");
    one("qwen_nodes", "Qwen 计算图节点");
    let models = d.get("models").and_then(Value::as_array).cloned().unwrap_or_default();
    let miss: Vec<String> = models
        .iter()
        .filter(|m| !m.get("found").and_then(Value::as_bool).unwrap_or(false) && m.get("optional").and_then(Value::as_bool) != Some(true))
        .map(|m| m.get("label").and_then(Value::as_str).unwrap_or("?").to_string())
        .collect();
    println!("  权重 {} 个，缺 {} 个{}", models.len(), miss.len(), if miss.is_empty() { "".to_string() } else { format!("：{}", miss.join("、")) });
    println!("  要核对指纹（13GB 约十几秒）就选菜单里的 [6]");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("menu");
    match cmd {
        "menu" | "" => menu(),
        "status" => {
            status_line(args.get(1).and_then(|s| s.parse().ok()).unwrap_or_else(want));
            Ok(())
        }
        "start" => start(!args.contains(&"--no-browser".to_string())),
        "stop" => stop(args.get(1).map(|s| s.as_str()).unwrap_or("app"), true),
        "switch" => switch_prod(!args.contains(&"--no-browser".to_string())),
        "restart" => {
            stop_port(want(), true)?;
            start(true)
        }
        "backfill" => backfill(),
        "lock-weights" => lock_weights(args.get(1).cloned()),
        other => {
            println!("用法：synco-tools [menu|status [端口]|start|stop [app|comfy|comfy2|端口]|restart|switch|backfill|lock-weights [ComfyUI根目录]]");
            println!("不带参数就是双击菜单。没这一项：{other}");
            Ok(())
        }
    }
}
