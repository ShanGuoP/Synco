//! 小工具：时间基准、随机后缀、路径归属判断、base64 解码、文件名清洗。
//! 这些看着琐碎，但落盘文件名与库里的字符串都要逐字节对得上。

use base64::Engine as _;
use rusqlite::Connection;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 文件名里的时间戳与云端请求计时都用这个：毫秒级 Unix 时间
pub fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// 库里每个 created_at 的 DEFAULT 都是 datetime('now','localtime')，
/// 写侧再自己拼一套就会差出一个时区——所以这里直接问 SQLite，不自己格式化。
pub fn now_localtime(conn: &Connection) -> String {
    conn.query_row("SELECT datetime('now','localtime')", [], |r| r.get::<_, String>(0))
        .unwrap_or_default()
}

/// `Math.floor(Math.random()*65536).toString(16).padStart(4,'0')`
pub fn r4() -> String {
    format!("{:04x}", rand::random::<u16>())
}

/// p 是否在 base 之内（含 base 自身）。前缀比较会放行 public_backup 这种同层兄弟目录，
/// 所以一律算相对路径再判，语义对齐 path.relative。
pub fn inside(base: &Path, p: &Path) -> bool {
    match pathdiff::diff_paths(p, base) {
        // 一个绝对一个相对时 diff_paths 给 None——判不了就算不在里面，宁可拒
        None => false,
        Some(rel) => {
            !rel.starts_with("..") && !rel.is_absolute()
        }
    }
}

/// URL 里来的相对路径能不能落在根内。**不能只判开头**：`/public/js%5c..%5c..%5c..` 解出来是
/// `js\..\..\..`，首段是 `js`，`starts_with("..")` 放行，而 Windows 把 `\` 也当分隔符，
/// join 之后真的跳出去了。所以这里按 `/` 与 `\` 两种分隔符逐段查，并挡掉绝对形式与盘符。
pub fn rel_ok(rel: &str) -> bool {
    let drive = matches!(rel.as_bytes(), [c, b':', ..] if c.is_ascii_alphabetic());
    !rel.is_empty() && !drive && !rel.starts_with('/') && !rel.starts_with('\\')
        && rel.split(['/', '\\']).all(|seg| seg != "..")
}

/// 盘上路径一律以 `projects/1/xxx.png` 这种正斜杠相对形式入库
pub fn rel_slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// 只是给人看的地址归一：把 `a/./b/../c` 收成 `a/c`，不碰文件系统。
/// 界面与横幅里显示的目录必须是这种形式，否则 `crates/server/../../public` 这种串会让人以为指错了地方。
pub fn neat_path(p: &Path) -> String {
    let s = rel_slash(p);
    // 盘符单独留着，剩下的按 / 逐段收
    let (head, rest) = match s.as_bytes().get(1) {
        Some(&b':') => (&s[..2], &s[2..]),
        _ => ("", s.as_str()),
    };
    let rooted = rest.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().map(|x| *x != "..").unwrap_or(false) {
                    parts.pop();
                } else if !rooted {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    format!("{head}{}{}", if rooted { "/" } else { "" }, parts.join("/"))
}

pub fn file_alive(data: &Path, rel: &Value) -> bool {
    match rel.as_str().filter(|s| !s.is_empty()) {
        Some(r) => data.join(r).is_file(),
        None => false,
    }
}

/// 库里的相对路径 → data/ 内一个**真实存在**的文件路径。
///
/// 词法判（[`rel_ok`]）之后还要看一次真实路径：`fs::read`/`remove_file` 都跟随符号链接与
/// junction，只判字面 `..` 挡不住重解析点。返回的是 `canonicalize` 之后的路径，调用方拿它
/// 去读/删/复制，就不会出现"判的是 A、开的是 B"那种中间被人换掉的窗口。
/// 解不开（不存在、被删、指到根外）一律 `None`，语义等同"盘上没这个文件"。
pub fn data_file(data: &Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() || !rel_ok(rel) {
        return None;
    }
    let root = data.canonicalize().ok()?;
    let real = data.join(rel).canonicalize().ok()?;
    inside(&root, &real).then_some(real)
}

/// 与 Buffer.from(x,'base64') 一样宽容：丢掉字母表以外的字符再解，不要求规范填充
pub fn decode_b64(s: &str) -> Vec<u8> {
    let body = match s.find(";base64,") {
        Some(i) => &s[i + 8..],
        None => s,
    };
    let cleaned: String = body
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'))
        .collect();
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::general_purpose::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    // URL-safe 变体的 -_ 换回标准字母表，前端偶发会带
    let mut normalized = cleaned.replace('-', "+").replace('_', "/");
    // 尾部不足 4 个字符的补上填充再解：Node 的 Buffer.from(x,'base64') 对未填充输入是照解不误的
    while normalized.len() % 4 != 0 {
        normalized.push('=');
    }
    engine.decode(&normalized).unwrap_or_default()
}

/// `String(v||'').replace(/[\\/:*?"<>|#%]/g,'_')` 再取尾 80 个字符
pub fn safe_name(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '%') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let chars: Vec<char> = mapped.chars().collect();
    chars[chars.len().saturating_sub(80)..].iter().collect()
}

/// JS 的 slice(0, n)：按字符截，不是按字节
pub fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// reqwest 的 Display 会把完整 URL 带进来（那条 URL 可能写着凭据），所以细节优先取错误链上的
/// 下一层：连接被拒时露出的是 io 层的 os error。链上什么都没有才退回 Display。
pub fn reqwest_detail(e: &reqwest::Error) -> String {
    let mut detail = String::new();
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(x) = src {
        let t = x.to_string();
        if !t.is_empty() && !t.contains("http") {
            detail = t;
        }
        src = x.source();
    }
    if detail.is_empty() {
        detail = clip(&e.to_string(), 180);
    }
    detail
}

/// `Number(x) || fallback`，其中 null/空串/空白/非数字都按 NaN 处理
pub fn num_or(v: Option<&Value>, fb: f64) -> f64 {
    number_of(v).unwrap_or(fb)
}

/// 单独取数字：区分"没填"（None）与"填了 0"（Some(0.0)）
pub fn number_of(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                t.parse::<f64>().ok()
            }
        }
        _ => None,
    }
}

/// CLAMP(v, lo, hi, fb)：NaN 时回退，否则四舍五入后夹在区间内
pub fn clamp_round(v: Option<&Value>, lo: i64, hi: i64, fb: i64) -> i64 {
    match number_of(v) {
        Some(n) if n.is_finite() => (n.round() as i64).clamp(lo, hi),
        _ => fb,
    }
}

/// 从库里读出来的字符串列 → 数组；坏 JSON 或非数组一律按空数组
pub fn safe_arr(s: Option<&str>) -> Vec<Value> {
    serde_json::from_str::<Value>(s.unwrap_or("[]")).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

/// `path.join(dir, ...)` 的相对片段拼装，统一正斜杠
pub fn rel_path(parts: &[String]) -> String {
    let mut p = PathBuf::new();
    for part in parts {
        p.push(part);
    }
    rel_slash(&p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 路径归一只按字面不碰文件系统() {
        assert_eq!(neat_path(Path::new("D:/a/crates/server/../../public")), "D:/a/public");
        assert_eq!(neat_path(Path::new("D:/a/./b/c.png")), "D:/a/b/c.png");
        // 退过头的 .. 原样留着：它可能真的有效（相对当前目录），不能瞎吞
        assert_eq!(neat_path(Path::new("../a/../b")), "../b");
        assert_eq!(neat_path(Path::new("projects/1/x.png")), "projects/1/x.png");
    }

    #[test]
    fn inside_挡住同层兄弟目录() {
        let base = Path::new("D:/AI/x/data");
        assert!(inside(base, Path::new("D:/AI/x/data/projects/1/a.png")));
        assert!(inside(base, base));
        assert!(!inside(base, Path::new("D:/AI/x/data_backup/a.png")));
        assert!(!inside(base, Path::new("D:/AI/x/other/a.png")));
        assert!(!inside(base, Path::new("D:/AI/x/data/../etc/passwd")));
    }

    #[test]
    fn rel_ok_逐段查_连反斜杠一起() {
        assert!(rel_ok("projects/1/a.png"));
        assert!(rel_ok("js/app.js"));
        // 实测过的绕过形态：解出来首段是 js，只判开头就会放行
        assert!(!rel_ok("js\\..\\..\\..\\outside\\canary.txt"));
        assert!(!rel_ok("a/../../b"));
        assert!(!rel_ok(".."));
        assert!(!rel_ok("/etc/passwd"));
        assert!(!rel_ok("\\\\server\\share\\x"));
        assert!(!rel_ok("C:/Windows/win.ini"));
        assert!(!rel_ok(""));
    }

    #[test]
    fn base64_容忍_data_url_与非法字符() {
        assert_eq!(decode_b64("aGVsbG8="), b"hello");
        assert_eq!(decode_b64("data:image/png;base64,aGVsbG8="), b"hello");
        assert_eq!(decode_b64("aGVsbG8"), b"hello");
        assert_eq!(decode_b64("aG Vs bG8="), b"hello");
        assert!(decode_b64("").is_empty());
    }

    #[test]
    fn 文件名清洗与尾截() {
        assert_eq!(safe_name("a#b%c:d.jpg"), "a_b_c_d.jpg");
        let long = "x".repeat(120);
        assert_eq!(safe_name(&long).len(), 80);
        // slice(-80) 取的是尾部
        let mut s = String::from("AAAA");
        s.push_str(&"B".repeat(100));
        assert!(safe_name(&s).starts_with('B'));
    }

    #[test]
    fn 数字解析区分没填和填了零() {
        assert_eq!(number_of(Some(&Value::String("".into()))), None);
        assert_eq!(number_of(Some(&Value::String("0".into()))), Some(0.0));
        assert_eq!(number_of(Some(&Value::Null)), None);
        assert_eq!(clamp_round(Some(&Value::String("".into())), 30000, 600000, 180000), 180000);
        // 历史脏值 5000 会被抬到下限，这是刻意的下限钳制，不是漏判
        assert_eq!(clamp_round(Some(&Value::String("5000".into())), 30000, 600000, 180000), 30000);
    }
}

/// 去扩展名去目录，等价 path.basename(p, path.extname(p))
pub fn stem_of(p: &str) -> String {
    let base = p.rsplit(['/', '\\']).next().unwrap_or(p);
    match base.rfind('.') {
        Some(0) | None => base.to_string(),
        Some(i) => base[..i].to_string(),
    }
}

/// 同步重活（整文件读写、全分辨率解码与重编码）一律走这里，别压在 async handler 上：
/// 界面轮询与接口共用这个进程，一个 tokio worker 被占住的表现是整站跟着卡，不是某个请求慢。
///
/// 闭包里还能 `tokio::spawn`——阻塞池线程建起来时就 `rt.enter()` 过，runtime 上下文在。
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> crate::error::Result<T> + Send + 'static) -> crate::error::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| Err(crate::error::AppError::fail("srv.util.poolDead")))
}

#[cfg(test)]
mod blocking_tests {
    use super::blocking;
    use crate::error::{AppError, Result};

    /// 这条锁住上面那句"闭包里还能 tokio::spawn"：真把 spawn 放进阻塞闭包跑一次，
    /// 上下文丢了它就是 panic，而 panic 会以 JoinError 形式变成一条错，不会静默。
    #[tokio::test]
    async fn 阻塞闭包里能起后台任务也能透传错误() {
        let n = blocking(|| {
            tokio::spawn(async {});
            Ok::<_, AppError>(7u8)
        })
        .await
        .unwrap();
        assert_eq!(n, 7);

        let e: Result<u8> = blocking(|| Err(AppError::bad("srv.util.poolDead"))).await;
        // 钥匙要能穿过 spawn_blocking 原样回来，前端才查得到字典
        assert!(matches!(e, Err(AppError::Msg { status: 400, .. })), "{e:?}");
    }
}
