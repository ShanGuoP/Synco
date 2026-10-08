//! 版本与构建标识：语义版本是常量，commit 与日期启动时现取 git，取不到就回退常量。
//! 打包态（没有 .git、没有 git.exe）必须安静退化，不能影响启动。
//! 更新日志不在这一页——那一栏读的是 GitHub Releases，见 `service::releases`。

use serde::Serialize;
use std::process::Command;
use std::sync::OnceLock;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize, Clone, Debug)]
pub struct BuildInfo {
    pub version: String,
    /// 读不到 git 时是 "unknown"，前端据此决定这一格显示不显示（不再解释为什么读不到）
    pub commit: String,
    pub date: String,
}

fn git(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    // 发布态的桌面壳是 GUI 子系统进程，没有控制台；在这种进程里 spawn git.exe 这种
    // 控制台程序，Windows 会给它新开一个控制台窗口——就是"打开软件黑框一闪"。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn build() -> BuildInfo {
    BuildInfo {
        version: VERSION.into(),
        commit: git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into()),
        date: git(&["log", "-1", "--format=%cI"]).unwrap_or_else(|| "unknown".into()),
    }
}

pub fn info() -> &'static BuildInfo {
    static CACHE: OnceLock<BuildInfo> = OnceLock::new();
    CACHE.get_or_init(build)
}
