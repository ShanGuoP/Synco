//! Synco 服务端库：把 `main.rs` 的启动流程暴露成可被桌面壳内嵌的一次调用。
//!
//! 桌面形态（M4）要求单文件分发：同一个 axum 应用跑在 Tauri 进程的 tokio 里，
//! 端口与数据目录由调用方决定，其余语义与命令行版完全一致。

pub mod api;
pub mod error;
pub mod img;
pub mod models;
pub mod repo;
pub mod service;
pub mod state;
pub mod text;
pub mod util;
pub mod version;
pub mod web;

use axum::extract::DefaultBodyLimit;
use axum::Router;
use error::{AppError, Result};
use repo::db;
use service::{backend, imagesvc, queue};
use state::{Ctx, Shared};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::{env, fs};
use web::{files, guard};

/// 数据目录：`SYNCO_DATA` 优先。
///
/// 开发态退回仓库根的 `data/`；发布态退回 **exe 同目录的 `data/`**——`env!` 是编译期宏，
/// 把它留在发布路径里等于把构建机的绝对路径烧进二进制，换台机器要么写失败，
/// 要么在别人的盘上凭空建出 `D:\AI\qwen\mask_demo\data`。
/// 桌面壳不走这条：它按"记过的 > 就近老库 > 绿色目录 > LOCALAPPDATA"自己算完再传进来。
pub fn data_dir() -> PathBuf {
    if let Some(p) = env::var_os("SYNCO_DATA") {
        return PathBuf::from(p);
    }
    if cfg!(debug_assertions) {
        // 必须是上跳两层（crates/server → 仓库根），少跳一层会在 crates/data 里凭空长出第二个空库
        return PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("data");
    }
    env::current_exe().ok()
        .and_then(|e| e.parent().map(|d| d.join("data")))
        .unwrap_or_else(|| PathBuf::from("data"))
}

/// 开发态直接用项目里的 public/；打包态盘上没有这个目录时，由 `assets` 的内嵌资源兜底
pub fn public_dir() -> PathBuf {
    if let Some(p) = env::var_os("SYNCO_PUBLIC") {
        return PathBuf::from(p);
    }
    let exe = env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("public")));
    if let Some(p) = exe.filter(|p| p.is_dir()) {
        return p;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../public")
}

/// 想用的端口：SYNCO_PORT，默认 7861
pub fn want_port() -> u16 {
    env::var("SYNCO_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(7861)
}

/// 运行时文件的家：锁、日志这些进程活着才有意义的东西全收在这里，
/// 别摊在数据根目录上跟 `projects/`、`app.db` 混成一堆。
pub fn runtime_dir(data: &Path) -> PathBuf {
    data.join("runtime")
}

/// 一个数据目录只许一个进程动。锁是 `<data>/runtime/instance.lock` 的独占句柄，必须在开库之前拿到。
///
/// 第二份进程若照常启动，它会按**自己进程**的在飞表收尸——第一份真正在飞的那一张在它眼里是僵尸，
/// 判成 error 之后，第一份回来落盘时状态守卫不认，刚生成的成图和缩略档被当成没人认领的孤儿删掉。
/// 钱花了、图没了、库里写着 error。
///
/// 句柄故意不关：进程活着锁就在，进程被 kill 时由系统回收，盘上不留脏状态。
/// 只加 Windows 锁：发布形态是 Windows 安装包，没有别的平台多进程共目录的用法。
fn lock_data_dir(data: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let dir = runtime_dir(data);
        fs::create_dir_all(&dir).map_err(AppError::Io)?;
        // 这一版起锁挪进 runtime/，但根目录那份**也要拿**：还开着的老版本只认根目录那把，
        // 我们若只锁新位置，两版就会同时开库、互相把对方的在飞任务判成僵尸收尸——正是要挡的那场事故。
        for p in [dir.join("instance.lock"), data.join("instance.lock")] {
            match hold_exclusive(&p) {
                Ok(f) => std::mem::forget(f),
                // 32 = ERROR_SHARING_VIOLATION：另一个进程正持有这个目录
                Err(e) if e.raw_os_error() == Some(32) => {
                    return Err(AppError::fail("srv.app.dataLocked"))
                }
                Err(e) => return Err(AppError::Io(e)),
            }
        }
    }
    #[cfg(not(windows))]
    let _ = data;
    Ok(())
}

/// 独占打开一个文件：`share_mode(0)` 让别人连开都开不了，锁的凭据就是这个还握着的句柄。
#[cfg(windows)]
fn hold_exclusive(p: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().read(true).write(true).create(true).share_mode(0).open(p)
}

/// 清掉不再生成的老文件。只碰确定没人再写的：`port.txt` 已经从"落盘给人猜端口"改成"起服务的人自己知道端口"。
/// 老的 `instance.lock` 要留着当第二把锁，老的 `server.log` 可能还被谁开着——都不动。
fn sweep_legacy_runtime_files(data: &Path) {
    let _ = std::fs::remove_file(data.join("port.txt"));
}

pub struct Boot {
    pub port: u16,
    pub ctx: Shared,
    /// axum 的监听任务；桌面壳 join 它，命令行版直接等它跑完
    pub server: tokio::task::JoinHandle<()>,
}

/// 跑在桌面壳里吗——命令行版永远是 false，由桌面壳在启动时置一下。
/// 「关于」分区要据此说清界面是从盘上还是从 exe 里取的。
pub static DESKTOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 起服务：先独占数据目录（同目录的第二份进程在这里就被挡下），再建库、绑端口（占用就退随机）、收尸、补派生档。
/// 实际端口只从 `Boot.port` 走：谁起服务谁就知道端口，不再往盘上落一个 port.txt 让别人来猜。
/// 返回的 server 一旦完成，端口与队列就已经在跑了。
pub async fn serve(data: PathBuf, public: PathBuf, want: u16) -> Result<Boot> {
    fs::create_dir_all(&data).map_err(AppError::Io)?;
    lock_data_dir(&data)?;
    sweep_legacy_runtime_files(&data);
    let ctx = Ctx::new(data.clone(), public, db::open(&data)?);

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), want);
    // 端口被占就退随机：桌面壳拿的是 Boot.port，跟着走就行
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if want != 0 => {
            println!("  {want} 端口被占用（{e}），改用随机端口");
            tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?
        }
        Err(e) => return Err(AppError::Io(e)),
    };
    let real = listener.local_addr()?.port();

    let app = Router::new()
        .merge(api::router())
        .fallback(files::fallback)
        // 一批 8 张 24MP 的 base64 有几十 MB，默认 2MB 上限会把导入直接挡掉；
        // 超限由 extractor 兜住（状态码 413），不让超大 body 把内存打爆
        .layer(DefaultBodyLimit::max(api::BODY_LIMIT))
        .layer(axum::middleware::from_fn(guard::loopback_only))
        .layer(axum::middleware::from_fn(web::csp::apply))
        .with_state(ctx.clone());

    println!("Synco 新刻（Rust {}）  SYNCO_URL=http://127.0.0.1:{real}", version::VERSION);
    println!("  数据目录 {}", data.display());
    let c = service::workflow::get_cfg(&ctx);
    // 横幅写在一行里：控制台归控制台，但"中文只在语言包里"这条判据是按行认的
    if c.get("cfgSource").and_then(|v| v.as_str()) == Some("workflow") {
        println!("  后端 {} · 参数读自工作流", backend::active_url(&ctx));
    } else {
        println!("  后端 {} · 参数用内置默认（工作流读不到：{}）", backend::active_url(&ctx), c.get("workflowError").and_then(text::render).unwrap_or_else(|| "未知".into()));
    }

    // 上一轮没跑完的 running 记录此时没人轮询了，先收一遍，别让状态栏一直撒谎
    let reclaim_ctx = ctx.clone();
    tokio::spawn(async move {
        match service::reclaim::reclaim_running(&reclaim_ctx).await {
            Ok(n) if n > 0 => println!("  收回 {n} 条 ComfyUI 已经不认的 running 记录"),
            Err(e) => eprintln!("  回收 running 记录失败：{}", e.text()),
            _ => {}
        }
        // 收完尸再叫队列：上一轮留下的 queued 行接着跑，关页面那批不该因为重启就消失
        let n = queue::pump(&reclaim_ctx).await;
        if n > 0 {
            println!("  重新叫起 {n} 张排队中的云端任务");
        }
    });
    // 存量库的派生档（升级前导入的照片没有 320/3072 两档）在后台补，不挡首屏
    imagesvc::spawn_backfill(&ctx);
    // 在飞行的状态由服务端推进（前端只读），关页面不该让已经出图的那张永远挂着
    service::reclaim::spawn_advancer(&ctx);

    let server = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("  服务停了：{e}");
        }
    });
    Ok(Boot { port: real, ctx, server })
}
