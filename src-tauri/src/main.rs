//! Synco 桌面壳（M4）：Tauri 2 的窗口 + 同进程内嵌的 axum 服务。
//!
//! 三件要紧事：
//! 1. 服务先在 tauri 的 tokio 里起来，端口定了才建窗口——窗口加载
//!    `http://127.0.0.1:<port>`，前端一行都不用改（回环 HTTP 契约不变）。
//! 2. 数据目录不能默认换址：老库在 `<项目>/data`，装到 Program Files 之后
//!    直接算 LOCALAPPDATA 会让几百个项目"不见了"。所以先读记过的、再就近找，
//!    两边都没有才用默认值，并把这一页第一次就摊给用户看。
//! 3. 关窗即退进程：服务是进程内的任务，留着只会白占端口。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::{env, fs};
use tauri::{AppHandle, Manager, State};

/// 桌面壳的偏好：数据目录 + 界面底色档位
fn app_base() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Synco")
}
fn memory_file() -> PathBuf {
    app_base().join("desktop.json")
}
/// 窗口还没画第一帧时的底色。值必须跟 tokens.css 的 --bg-app 一致，
/// 否则墨黑档一启动就闪一下纸白——这一段是 Rust 管的，页面里的内联脚本再早也早不过它
fn bg_color(dark: bool) -> tauri::utils::config::Color {
    if dark {
        tauri::utils::config::Color(0x17, 0x16, 0x1a, 255)
    } else {
        tauri::utils::config::Color(0xf6, 0xf2, 0xea, 255)
    }
}
/// 改名之前那版把资料放在 LOCALAPPDATA\PixFish——老安装不能因为改名就找不到自己的库
fn legacy_base() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("PixFish")
}

#[derive(Serialize)]
struct DirInfo {
    path: String,
    source: &'static str,
    remembered: bool,
    has_db: bool,
    projects: i64,
    images: i64,
    bytes: u64,
    /// 换址与搬家都要重启才生效：服务已经开着旧库的连接了
    restart_required: bool,
}

struct Shell {
    data: PathBuf,
    remembered: bool,
    source: &'static str,
    restart_required: bool,
}

/// 记过的目录 > 环境变量 > 就近能找到的老库 > LOCALAPPDATA 默认
fn resolve_data() -> (PathBuf, bool, &'static str) {
    if let Some(p) = env::var_os("SYNCO_DATA") {
        return (PathBuf::from(p), true, "shell.src.env");
    }
    if let Some(s) = read_memory().get("data_dir").and_then(|x| x.as_str()) {
        let p = PathBuf::from(s);
        if p.join("app.db").is_file() {
            return (p, true, "shell.src.remembered");
        }
    }
    // 就近探测：从 exe 一层层往上找 data/app.db，跑在 target/release 或项目里都能命中老库
    if let Some(exe) = env::current_exe().ok() {
        let mut up = exe.parent().map(|p| p.to_path_buf());
        while let Some(d) = up {
            let cand = d.join("data");
            if cand.join("app.db").is_file() {
                return (cand, false, "shell.src.nearby");
            }
            up = d.parent().map(|p| p.to_path_buf());
        }
    }
    if let Ok(cwd) = env::current_dir() {
        let cand = cwd.join("data");
        if cand.join("app.db").is_file() {
            return (cand, false, "shell.src.cwd");
        }
    }
    // 改名之前的安装：LOCALAPPDATA\PixFish\data 里有库就接着用，别让人以为资料没了
    let old = legacy_base().join("data");
    if old.join("app.db").is_file() {
        return (old, false, "shell.src.legacy");
    }
    // 绿色模式：哪儿的老库都没有，就把 data/ 开在 exe 边上——把整个文件夹拷到哪，资料就跟到哪。
    // 装到 Program Files 时这里建不出来（没写权限），自然落到下面的 LOCALAPPDATA 默认值。
    if let Some(dir) = env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
        let cand = dir.join("data");
        if fs::create_dir_all(&cand).is_ok() {
            return (cand, false, "shell.src.portable");
        }
    }
    (app_base().join("data"), false, "shell.src.default")
}

/// desktop.json 现在有两样东西（目录 + 底色），读的时候按原样拿回来，
/// 坏文件当空的——它只是偏好，不值得为它让窗口起不来
fn read_memory() -> serde_json::Value {
    fs::read_to_string(memory_file())
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// 壳里的错误与"这台机器上的目录是哪来的"那一格都以钥匙出去：tauri 的错误通道只有一条字符串，
/// 所以带参数的就整条写成 `{code, args}` 的 JSON，前端在 desktop.js 那一处统一换成当前语言。
fn why(code: &str, args: serde_json::Value) -> String {
    let mut o = serde_json::Map::new();
    o.insert("code".into(), serde_json::Value::String(code.to_string()));
    if let serde_json::Value::Object(m) = args {
        if !m.is_empty() {
            o.insert("args".into(), serde_json::Value::Object(m));
        }
    }
    serde_json::Value::Object(o).to_string()
}

/// 只有一把钥匙、没有参数的那种理由
fn only(code: &str) -> String {
    why(code, serde_json::Value::Null)
}

/// 合并写：换一次目录不该把底色抹掉，切一次主题也不该把目录写没
fn write_memory(patch: serde_json::Value) -> std::result::Result<(), String> {
    let f = memory_file();
    if let Some(d) = f.parent() {
        fs::create_dir_all(d).map_err(|e| why("srv.shell.confMkdirFail", serde_json::json!({ "msg": e.to_string() })))?;
    }
    let mut v = read_memory();
    if let (Some(obj), Some(p)) = (v.as_object_mut(), patch.as_object()) {
        for (k, val) in p {
            obj.insert(k.clone(), val.clone());
        }
    }
    let body = serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?;
    fs::write(&f, body).map_err(|e| why("srv.shell.writeFail", serde_json::json!({ "path": f.display().to_string(), "msg": e.to_string() })))
}

fn remember_data(path: &Path) -> std::result::Result<(), String> {
    write_memory(serde_json::json!({ "data_dir": path.to_string_lossy().replace('\\', "/") }))
}

/// 目录体积：界面要说清"要搬走多大一坨"，读不到的条目按 0 计
fn dir_bytes(dir: &Path) -> u64 {
    let mut n = 0u64;
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            n += dir_bytes(&p);
        } else if let Ok(m) = e.metadata() {
            n += m.len();
        }
    }
    n
}

fn abs_path(s: &str) -> Option<PathBuf> {
    let t = s.trim().trim_matches('"');
    // 与导出目录同一判据：必须以盘符开头的绝对路径，免得把相对路径记进配置
    let drive_ok = t.as_bytes().first().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) && t.as_bytes().get(1) == Some(&b':');
    if drive_ok { Some(PathBuf::from(t)) } else { None }
}

impl Shell {
    fn info(&self, ctx: &synco_server::state::Shared) -> DirInfo {
        // 两条计数在 DAO 里各取一次连接就撒手：这把守卫不能在 dir_bytes 遍历整棵 data/ 期间还握着——
        // 连接全局只有一个，几十 GB 瓦片走一遍的工夫里，所有走库的 HTTP handler（含队列轮询）都在排队
        let (projects, images) = synco_server::repo::db::library_counts(ctx);
        DirInfo {
            path: self.data.to_string_lossy().replace('\\', "/"),
            source: self.source,
            remembered: self.remembered,
            has_db: self.data.join("app.db").is_file(),
            projects,
            images,
            bytes: dir_bytes(&self.data),
            restart_required: self.restart_required,
        }
    }
}

#[tauri::command]
fn data_dir_info(state: State<'_, Shell>, ctx: State<'_, synco_server::state::Shared>) -> DirInfo {
    state.info(ctx.inner())
}

/// 换址只改记忆，不动文件：搬不搬由用户再点一次「把资料复制过去」
#[tauri::command]
fn set_data_dir(path: String, state: State<'_, Shell>, ctx: State<'_, synco_server::state::Shared>) -> Result<serde_json::Value, String> {
    let p = abs_path(&path).ok_or_else(|| only("srv.shell.absNeeded"))?;
    let same = p == state.data;
    remember_data(&p)?;
    let next = Shell {
        data: p,
        remembered: true,
        source: if same { state.source } else { "shell.src.picked" },
        // 选回当前这个目录等于没换，不该显示"重启后生效"
        restart_required: !same,
    };
    Ok(serde_json::json!({ "dir": next.info(ctx.inner()) }))
}

/// 把当前数据目录整棵复制到新位置，然后记下新位置。
/// 只在目标目录还没有库的时候动手——覆盖别人的资料库这种事不能默认发生。
#[tauri::command]
fn copy_data_to(to: String, state: State<'_, Shell>, ctx: State<'_, synco_server::state::Shared>) -> Result<serde_json::Value, String> {
    let dst = abs_path(&to).ok_or_else(|| only("srv.shell.absDirNeeded"))?;
    if dst == state.data {
        return Err(only("srv.shell.sameDir"));
    }
    if dst.join("app.db").is_file() {
        return Err(only("srv.shell.targetHasLibrary"));
    }
    // 复制前把 WAL 合回主文件，否则拷过去的是半个库 + 一份没人认领的 -wal
    synco_server::repo::db::checkpoint_truncate(ctx.inner());
    fs::create_dir_all(&dst).map_err(|e| why("srv.shell.mkdirFail", serde_json::json!({ "msg": e.to_string() })))?;
    let (files, bytes) = copy_tree(&state.data, &dst)?;
    remember_data(&dst)?;
    let next = Shell { data: dst, remembered: true, source: "shell.src.copied", restart_required: true };
    Ok(serde_json::json!({ "dir": next.info(ctx.inner()), "copied": { "files": files, "bytes": bytes } }))
}

/// 逐条复制：跳过 runtime/（锁与日志）这类进程私有的东西，任何一步失败把错误原文带出去
fn copy_tree(from: &Path, to: &Path) -> std::result::Result<(usize, u64), String> {
    let mut n = (0usize, 0u64);
    for e in fs::read_dir(from).map_err(|e| why("srv.shell.dirReadFail", serde_json::json!({ "path": from.display().to_string(), "msg": e.to_string() })))? {
        let e = e.map_err(|e| e.to_string())?;
        let name = e.file_name();
        if ["runtime", "port.txt", "desktop.json", "instance.lock", "server.log"].contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let src = e.path();
        let dst = to.join(name);
        if src.is_dir() {
            fs::create_dir_all(&dst).map_err(|er| why("srv.shell.mkdirAt", serde_json::json!({ "path": dst.display().to_string(), "msg": er.to_string() })))?;
            let sub = copy_tree(&src, &dst)?;
            n.0 += sub.0;
            n.1 += sub.1;
        } else {
            fs::copy(&src, &dst).map_err(|er| why("srv.shell.copyFail", serde_json::json!({ "path": src.display().to_string(), "msg": er.to_string() })))?;
            n.0 += 1;
            n.1 += fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok(n)
}

/// 页面切了主题就把生效档位记下来，顺手把当前窗口的底色也换掉：
/// 记下的那份是给下一次启动的第一帧用的
#[tauri::command]
fn set_window_theme(app: AppHandle, mode: String) -> Result<(), String> {
    let dark = mode == "dark";
    write_memory(serde_json::json!({ "theme": if dark { "dark" } else { "light" } }))?;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_background_color(Some(bg_color(dark)));
    }
    Ok(())
}

#[tauri::command]
fn restart_app(app: AppHandle) {
    app.restart();
}

/// 在系统默认浏览器里打开一条外链：这个窗口跳去 GitHub 就等于把工坊关掉了。
///
/// 只接 http/https，并且逐字符过白名单：这条链接**不是常量**——仓库地址、每条 Release 的
/// 页面与安装包下载链都来自 GitHub API 回来的 JSON（`service::releases` → 设置页 → 这里），
/// 所以它是一条远端可控的字符串，两件事都要挡：跳到别的 scheme，和变成一条命令行。
///
/// Windows 上不走 `cmd /C start`：cmd 会把整行**再解析一遍**，`&`、`|`、`^`、`%VAR%` 在那里
/// 是第二层语义。`explorer.exe` 拿到的是 CreateProcess 交出去的原始参数，直接进 ShellExecute。
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    let u = url.trim();
    if let Some(blocked) = url_gate(u) {
        return Err(blocked.to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer.exe")
            .arg(u)
            // GUI 子系统进程里 spawn 控制台程序会新开一个黑框，与启动时取 git 信息同一套处理
            .creation_flags(0x0800_0000)
            .spawn()
            .map(|_| ())
            .map_err(|e| why("srv.shell.browserFail", serde_json::json!({ "msg": e.to_string() })))
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(u)
            .spawn()
            .map(|_| ())
            .map_err(|e| why("srv.shell.browserFail", serde_json::json!({ "msg": e.to_string() })))
    }
}

/// 一条能交给系统浏览器的链接：只认 http/https、不许超长，再逐字符过白名单。
/// 判不动就给出理由的钥匙（界面拿它去查当前语言那句话，而不是只回一句"失败"）。
fn url_gate(u: &str) -> Option<&'static str> {
    if u.len() > 500 {
        return Some("srv.shell.urlTooLong");
    }
    if !(u.starts_with("https://") || u.starts_with("http://")) {
        return Some("srv.shell.urlScheme");
    }
    if !url_chars_ok(u) {
        return Some("srv.shell.urlChars");
    }
    None
}

/// RFC 3986 里真能出现在一条 URL 上的字符。刻意不收的：空白、`"` `'` `` ` ``、`\`、`<` `>`、
/// `|` `;`（断开 URL 或喂给解析器的就是这几个），收 `%` 是因为资产名里的空格会编码成 `%20`。
fn url_chars_ok(u: &str) -> bool {
    u.chars().all(|c| match c {
        'a'..='z' | 'A'..='Z' | '0'..='9' => true,
        '-' | '.' | '_' | '~' | '%' | ':' | '/' | '?' | '#' | '[' | ']' | '@' => true,
        '!' | '$' | '(' | ')' | '*' | '+' | ',' | '=' => true,
        _ => false,
    })
}


/// 本地服务起不来时窗口就是空白的：理由（库被占、目录不可写）已经在 AppError 的钥匙里
fn startup_fail(e: synco_server::error::AppError) -> String {
    let why = e.text();
    eprintln!("  本地服务起不来，桌面窗口没法打开（看看数据目录是否可写）：{why}");
    why
}

fn main() {
    // 让 /api/version 说清这次是桌面版：「关于」分区和第一次的选址提示都看这个
    synco_server::DESKTOP.store(true, std::sync::atomic::Ordering::Relaxed);
    let (data, remembered, source) = resolve_data();
    let public = synco_server::public_dir();
    // 就近老库 / 绿色模式 / 默认位置这三条都是"我们替他选的"，选过一次就记下来——
    // 否则每次启动都算"没记过"，选址页会一遍遍地摊开（绿色模式那次尤其明显）。
    // 只有那个位置真的还没有库（第一次用）才弹；他自己换过址的判据在 remembered 里。
    let fresh = !data.join("app.db").is_file();
    if !remembered {
        let _ = remember_data(&data);
    }
    let prompt = !remembered && fresh;
    // 上一次生效的底色档位（前端每次切主题会记一份）；没记过就是纸白
    let dark = read_memory().get("theme").and_then(|x| x.as_str()) == Some("dark");

    tauri::Builder::default()
        // 单实例：服务与窗口同进程，双击两次就是两个进程共写同一个 app.db，
        // 而端口被占时服务会静默退到随机端口——第二份"看起来能开"，库却只有一个。
        // 第二次的动作改成把已经在跑的窗口叫到前面。
        //
        // 所以服务不能在 main 顶部起：插件的初始化发生在 Builder::build()，而这里写的
        // setup 要等事件循环的 Ready 才跑——早先 serve() 排在 Builder 之前，第二份进程已经
        // 开过库、按自己那份空白的在飞表把第一份正在跑的成图判成僵尸收掉了。
        // 挪进 setup 之后，第二份在碰库之前（更碰不到 runtime/ 那把锁）就 exit(0)。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![data_dir_info, set_data_dir, copy_data_to, set_window_theme, restart_app, open_url])
        .setup(move |app| {
            // 服务到这里才起来：窗口地址要带真实端口
            let boot = tauri::async_runtime::block_on(synco_server::serve(data.clone(), public, synco_server::want_port()))
                // 桌面版没有终端可读，这句同时写一份到进程的输出里；交给 tauri 的只剩那一句人话
                .map_err(startup_fail)?;
            let port = boot.port;
            app.manage(Shell { data, remembered, source, restart_required: false });
            app.manage(boot.ctx);
            let url = format!("http://127.0.0.1:{port}/{}", if prompt { "?firstrun=1" } else { "" });
            let url: tauri::Url = url.parse()?;
            let _w = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External(url))
                .title("Synco 新刻") // i18n-keep 品牌名与 productName 同源，不随界面语言换
                // 无边框：页面顶栏兼作标题区（data-tauri-drag-region），窗口三颗键由前端画。
                // 留着 shadow 与 resizable，否则贴到屏幕边上会像一块没有厚度的纸
                .decorations(false)
                .shadow(true)
                // WebView2 还在加载本机地址的时候窗口已经先出现了：没有背景色就是一块
                // 黑矩形闪一下，无边框连标题栏那条白边都不剩。底色跟着上次的档位走。
                .background_color(bg_color(dark))
                // 这一句是"拖照片进导入卡"能不能用的开关：Tauri 默认会顶掉 WebView2 自己的
                // 拖放处理，改从壳里发 DragDropEvent（只有文件路径，没有 File 对象），
                // 页面的 drop 于是永远不触发。这里不拦，HTML5 的 dataTransfer 才回得来。
                .disable_drag_drop_handler()
                .inner_size(1440.0, 900.0)
                .min_inner_size(920.0, 620.0)
                .center()
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("桌面壳建不起来：检查 src-tauri/tauri.conf.json 与 capabilities/default.json")
        .run(|_, _| {});
}

#[cfg(test)]
mod url_guard_tests {
    use super::url_gate;

    /// 「关于」那一页真会交下来的地址形状，一个都不能被误伤
    #[test]
    fn 真链接都放行() {
        for u in [
            "https://github.com/ShanGuoP/Synco",
            "https://github.com/ShanGuoP/Synco/releases/tag/v0.2.0",
            "https://github.com/ShanGuoP/Synco/releases/download/v0.2.0/Synco_0.2.0_x64-setup.exe",
            "https://github.com/a-b/repo.name/releases/download/v1.0/My%20App_1.0-setup.exe",
            "https://github.com/ShanGuoP",
            "http://127.0.0.1:7861/api/version",
        ] {
            assert_eq!(url_gate(u), None, "不该挡：{u}");
        }
    }

    /// 旧实现把这种串交给 `cmd /C start` 时，本机要多执行一条命令；
    /// 别的 scheme 则是"窗口跳去别处"或 file:// 直读本机盘
    #[test]
    fn 命令行形状与别的_scheme_都挡() {
        for u in [
            "https://x.example/&calc.exe",
            "https://x.example/|calc.exe",
            "https://x.example/;calc.exe",
            "https://x.example/`calc.exe`",
            "https://x.example/<nul",
            "https://x.example/\\..\\a.exe",
            "https://x.example/a b",
            "https://x.example/a\"b",
            "https://x.example/\nhttps://evil",
            "file:///C:/Windows/system32/calc.exe",
            "javascript:alert(1)",
            "javascript://x.example/%0aalert(1)",
        ] {
            assert!(url_gate(u).is_some(), "不该放行：{u}");
        }
        let long = format!("https://x.example/{}", "a".repeat(600));
        assert!(url_gate(&long).is_some(), "超长的那条没挡");
    }
}
