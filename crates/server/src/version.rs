//! 版本与更新日志：语义版本是常量，commit 与日期启动时现取 git，取不到就回退常量。
//! 打包态（没有 .git、没有 git.exe）必须安静退化，不能影响启动。

use serde::Serialize;
use std::process::Command;
use std::sync::OnceLock;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CHANGELOG_ENTRIES: usize = 12;

#[derive(Serialize, Clone, Debug)]
pub struct BuildInfo {
    pub version: String,
    pub commit: String,
    pub date: String,
    /// 最近若干条提交，作为设置「关于」分区的更新日志
    pub changelog: Vec<Commit>,
    /// git 不可用时前端要显示"来源：内置常量"，不能假装是实时值
    pub git_available: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Commit {
    pub hash: String,
    pub date: String,
    pub subject: String,
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
    // %h|%cI|%s 一次拿到三条字段，省两次进程启动
    let log = git(&["log", "--pretty=format:%h\x1f%cI\x1f%s", &format!("-n{CHANGELOG_ENTRIES}")]);
    match log {
        Some(raw) => {
            let mut entries = Vec::new();
            for line in raw.lines() {
                let mut f = line.split('\x1f');
                entries.push(Commit {
                    hash: f.next().unwrap_or("").to_string(),
                    date: f.next().unwrap_or("").to_string(),
                    subject: f.next().unwrap_or("").to_string(),
                });
            }
            let (commit, date) = entries
                .first()
                .map(|c| (c.hash.clone(), c.date.clone()))
                .unwrap_or_else(|| ("unknown".into(), "unknown".into()));
            BuildInfo { version: VERSION.into(), commit, date, changelog: entries, git_available: true }
        }
        None => BuildInfo {
            version: VERSION.into(),
            commit: "unknown".into(),
            date: "unknown".into(),
            changelog: Vec::new(),
            git_available: false,
        },
    }
}

pub fn info() -> &'static BuildInfo {
    static CACHE: OnceLock<BuildInfo> = OnceLock::new();
    CACHE.get_or_init(build)
}
