//! GitHub Releases：「关于 → 更新日志」的唯一来源。
//!
//! 以前这一栏读的是本机 `.git` 的提交历史，装到别人机器上就是空的（那句"打包态不带 .git"
//! 的解释就是这么来的）。改成从仓库的 Releases 拉，装了安装包的人也知道每一版改了什么。

use serde_json::{json, Map, Value};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 仓库地址：更新日志、下载、提 issue 都从这一处出
pub const REPO: &str = "ShanGuoP/Synco";
pub const REPO_URL: &str = "https://github.com/ShanGuoP/Synco";

const API: &str = "https://api.github.com/repos/ShanGuoP/Synco/releases?per_page=12";
/// 十分钟内的重复打开不再敲 GitHub；面板上的「重新读取」也走这个缓存，
/// 拉不到时才绕开它重试一次
const TTL: i64 = 600;

static CACHE: Mutex<Option<(i64, Vec<Value>)>> = Mutex::new(None);

/// 与 `ctx.http` 分开一个客户端：那个是 `no_proxy` 的（本机 ComfyUI 走代理会被拐慢），
/// 而 GitHub 这一路恰恰需要跟着系统/环境变量代理走。
fn gh() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    // GitHub 的 API 对没有 User-Agent 的请求直接 403，这一句不是礼貌是给它的
    CLIENT.get_or_init(|| reqwest::Client::builder().user_agent("Synco").timeout(Duration::from_secs(8)).build().unwrap_or_default())
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `-setup.exe` 结尾的那个资产：面板上「下载这一版」指的就是它
fn setup_url(rel: &Value) -> Option<String> {
    rel.get("assets")?
        .as_array()?
        .iter()
        .filter_map(|a| {
            let name = a.get("name").and_then(Value::as_str)?;
            let url = a.get("browser_download_url").and_then(Value::as_str)?;
            name.ends_with("-setup.exe").then(|| url.to_string())
        })
        .next()
}

fn shape(rel: &Value) -> Value {
    let date = rel.get("published_at").and_then(Value::as_str).unwrap_or("").chars().take(10).collect::<String>();
    Value::Object(Map::from_iter([
        ("tag".into(), Value::String(rel.get("tag_name").and_then(Value::as_str).unwrap_or("").to_string())),
        ("name".into(), Value::String(rel.get("name").and_then(Value::as_str).unwrap_or("").to_string())),
        ("url".into(), Value::String(rel.get("html_url").and_then(Value::as_str).unwrap_or("").to_string())),
        ("date".into(), Value::String(date)),
        ("notes".into(), Value::String(rel.get("body").and_then(Value::as_str).unwrap_or("").chars().take(600).collect())),
        ("prerelease".into(), rel.get("prerelease").cloned().unwrap_or(Value::Bool(false))),
        ("download".into(), setup_url(rel).map(Value::String).unwrap_or(Value::Null)),
    ]))
}

/// 拉一次 Releases。失败**不进缓存**，所以面板上的「重新读取」在离线之后点一次就是真重试一次；
/// 成功则十分钟内的重复打开都直接吃缓存。
pub async fn list() -> Value {
    let cached = {
        let guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().filter(|(at, _)| now() - *at < TTL).map(|(_, v)| v.clone())
    };
    let rows = match cached {
        Some(v) => v,
        None => match gh().get(API).send().await {
            Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                Ok(Value::Array(items)) => {
                    let v: Vec<Value> = items.iter().filter(|i| !i.get("draft").and_then(Value::as_bool).unwrap_or(false)).map(shape).collect();
                    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((now(), v.clone()));
                    v
                }
                Ok(_) => return out("srv.release.notList", json!({})),
                Err(e) => return out("srv.release.badBody", json!({ "msg": e.to_string() })),
            },
            Ok(r) => return out("srv.release.http", json!({ "code": r.status().as_u16() })),
            Err(e) => return out("srv.release.connect", json!({ "msg": e.to_string() })),
        },
    };
    let latest = rows.iter().find(|r| !r.get("prerelease").and_then(Value::as_bool).unwrap_or(false)).cloned().unwrap_or(Value::Null);
    Value::Object(Map::from_iter([
        ("repo".into(), Value::String(REPO_URL.into())),
        ("releases".into(), Value::Array(rows)),
        ("latest".into(), latest),
        ("error".into(), Value::String(String::new())),
    ]))
}

/// 拉取失败的三种形状分开说；界面拿钥匙查当前语言那一句
fn out(code: &str, args: Value) -> Value {
    let mut o = Map::from_iter([
        ("repo".into(), Value::String(REPO_URL.into())),
        ("releases".into(), Value::Array(Vec::new())),
        ("latest".into(), Value::Null),
        ("error".into(), Value::String(code.to_string())),
    ]);
    if let Value::Object(m) = args {
        if !m.is_empty() {
            o.insert("error_args".into(), Value::Object(m));
        }
    }
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 资产里只认_setup_exe_那个下载链() {
        let rel = serde_json::json!({
            "tag_name": "v0.2.0", "name": "0.2.0", "html_url": "https://github.com/x/y/releases/tag/v0.2.0",
            "published_at": "2026-10-07T11:33:00Z", "prerelease": false,
            "assets": [{ "name": "Synco_0.2.0_x64-setup.exe", "browser_download_url": "https://g/d/Synco_0.2.0_x64-setup.exe" },
                       { "name": "latest.json", "browser_download_url": "https://g/d/latest.json" }]
        });
        let s = shape(&rel);
        assert_eq!(s["download"], "https://g/d/Synco_0.2.0_x64-setup.exe");
        assert_eq!(s["date"], "2026-10-07");
        assert_eq!(s["tag"], "v0.2.0");
        // 没有 setup 资产时是 null，不是空串：面板据此决定要不要给「下载」这个链接
        let bare = shape(&serde_json::json!({ "tag_name": "v1", "assets": [] }));
        assert!(bare["download"].is_null());
    }
}
