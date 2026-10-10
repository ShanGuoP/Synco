//! ComfyUI 后端注册表：扫描本机端口、登记自定义接口、决定当前生效地址（对齐 backend.js）。

use crate::error::AppError;
use serde_json::json;
use crate::repo;
use crate::state::Ctx;
use crate::util::clip;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::time::Duration;
use url::Url;

pub const DEFAULT_URL: &str = "http://127.0.0.1:8188";
pub const PROBE_MS: u64 = 1500;
/// 常见 ComfyUI 端口；18188 是 Hub 托管那套
pub const SCAN_PORTS: [u16; 13] = [8188, 8189, 8187, 8186, 8185, 8182, 8180, 8080, 8081, 8000, 8888, 18188, 18189];

/// 只接受 http/https，允许带子路径（挂在反向代理后面时用得上）
pub fn norm_url(raw: &str) -> crate::error::Result<String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(AppError::bad("srv.url.empty"));
    }
    let with_scheme = if has_scheme(s) { s.to_string() } else { format!("http://{s}") };
    let u = Url::parse(&with_scheme).map_err(|_| AppError::bad("srv.url.bad"))?;
    match u.scheme() {
        "http" | "https" => {}
        _ => return Err(AppError::bad("srv.url.scheme")),
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(AppError::bad("srv.url.credentials"));
    }
    if u.query().is_some() || u.fragment().is_some() {
        return Err(AppError::bad("srv.url.query"));
    }
    let path = u.path().trim_end_matches('/').to_string();
    Ok(format!("{}{path}", u.origin().ascii_serialization()))
}

/// cloud.normBase 与 backend.normUrl 的返回写法不同：这里保留 pathname 原样（含结尾斜杠），
/// 与 JS 的 `u.origin + u.pathname` 一字不差，否则双跑时设置里的 base 会对不上
pub fn norm_base(raw: &str) -> crate::error::Result<String> {
    let s = raw.trim().trim_end_matches('/').to_string();
    if s.is_empty() {
        return Err(AppError::bad("srv.url.baseEmpty"));
    }
    let with_scheme = if has_scheme(&s) { s.clone() } else { format!("https://{s}") };
    let u = Url::parse(&with_scheme).map_err(|_| AppError::bad("srv.url.baseBad"))?;
    match u.scheme() {
        "http" | "https" => {}
        _ => return Err(AppError::bad("srv.url.scheme")),
    }
    if u.host_str().unwrap_or("").is_empty() {
        return Err(AppError::bad("srv.url.noHost"));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(AppError::bad("srv.url.baseCredentials"));
    }
    if u.query().is_some() || u.fragment().is_some() {
        return Err(AppError::bad("srv.url.baseQuery"));
    }
    Ok(format!("{}{}", u.origin().ascii_serialization(), u.path()))
}

/// 认「任意 scheme://」再判协议：只测 https?:// 会把 file:///x 当成域名拼到 https 后面蒙混过关
fn has_scheme(s: &str) -> bool {
    match s.find("://") {
        Some(i) if i > 0 => s[..i].chars().enumerate().all(|(k, c)| {
            if k == 0 {
                c.is_ascii_alphabetic()
            } else {
                c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')
            }
        }),
        _ => false,
    }
}

pub fn join(base: &str, p: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), p)
}

async fn get_json(ctx: &Ctx, url: &str) -> crate::error::Result<Value> {
    let r = ctx
        .http
        .get(url)
        .timeout(Duration::from_millis(PROBE_MS))
        .send()
        .await
        .map_err(|e| send_err(url, e))?;
    if !r.status().is_success() {
        return Err(AppError::bad_args("srv.backend.http", json!({ "code": r.status().as_u16() })));
    }
    r.json::<Value>().await.map_err(|e| send_err(url, e))
}

/// 认后端：新版读 /system_stats，老版退回 /version，再退回归根页面的标题
pub async fn probe(ctx: &Ctx, raw_url: &str) -> Value {
    let t0 = std::time::Instant::now();
    let mut out = Map::new();
    let url = match norm_url(raw_url) {
        Ok(u) => u,
        Err(e) => {
            out.insert("url".into(), Value::String(raw_url.to_string()));
            out.insert("ok".into(), Value::Bool(false));
            out.extend(err_fields(e));
            out.insert("ms".into(), Value::from(0));
            return Value::Object(out);
        }
    };
    out.insert("url".into(), Value::String(url.clone()));
    out.insert("ok".into(), Value::Bool(false));
    out.insert("version".into(), Value::Null);
    out.insert("device".into(), Value::Null);
    out.insert("vram_total".into(), Value::from(0));
    out.insert("vram_free".into(), Value::from(0));
    out.insert("os".into(), Value::Null);
    out.insert("python".into(), Value::Null);
    out.insert("torch".into(), Value::Null);

    if let Ok(j) = get_json(ctx, &join(&url, "/system_stats")).await {
        let sys = j.get("system").cloned().unwrap_or(Value::Null);
        let dev = j.get("devices").and_then(|d| d.as_array().and_then(|a| a.first().cloned())).unwrap_or(Value::Null);
        out.insert("ok".into(), Value::Bool(true));
        out.insert(
            "version".into(),
            sys.get("comfyui_version")
                .or_else(|| sys.get("ComfyUI Version"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        out.insert("os".into(), sys.get("os").cloned().unwrap_or(Value::Null));
        out.insert("python".into(), sys.get("python_version").cloned().unwrap_or(Value::Null));
        out.insert(
            "torch".into(),
            sys.get("torch_version").or_else(|| j.get("torch_version")).cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "device".into(),
            dev.get("device_name").or_else(|| dev.get("name")).cloned().unwrap_or(Value::Null),
        );
        out.insert("vram_total".into(), dev.get("vram_total").cloned().unwrap_or(Value::from(0)));
        out.insert("vram_free".into(), dev.get("vram_free").cloned().unwrap_or(Value::from(0)));
    } else if let Ok(v) = get_json(ctx, &join(&url, "/version")).await {
        out.insert("ok".into(), Value::Bool(true));
        out.insert(
            "version".into(),
            v.get("comfyui_version").or_else(|| v.get("version")).cloned().unwrap_or(Value::Null),
        );
    } else {
        match ctx.http.get(join(&url, "/")).timeout(Duration::from_millis(PROBE_MS)).send().await {
            Ok(r) => {
                let head = r.text().await.unwrap_or_default();
                let head = head.chars().take(4096).collect::<String>();
                if head.to_lowercase().contains("comfyui") {
                    out.insert("ok".into(), Value::Bool(true));
                    out.insert("version".into(), Value::String("srv.backend.versionOld".into()));
                } else {
                    out.insert(
                        "error".into(),
                        Value::String(if head.is_empty() { "srv.backend.emptyAnswer" } else { "srv.backend.notComfy" }.into()),
                    );
                }
            }
            Err(e) => {
                out.extend(err_fields(send_err(&url, e)));
            }
        }
    }
    out.insert("ms".into(), Value::from(t0.elapsed().as_millis() as i64));
    Value::Object(out)
}

pub fn list_custom(ctx: &Ctx) -> Vec<Value> {
    repo::all(ctx, "SELECT url, label, added_at FROM backends ORDER BY added_at", &[]).unwrap_or_default()
}

pub fn add_custom(ctx: &Ctx, raw_url: &str, label: Option<&str>) -> crate::error::Result<String> {
    let url = norm_url(raw_url)?;
    let lbl = label.map(|l| clip(l, 40)).filter(|l| !l.is_empty());
    repo::run(
        ctx,
        "INSERT INTO backends(url,label) VALUES(?,?) ON CONFLICT(url) DO UPDATE SET label=excluded.label",
        &[repo::s(&url), repo::si(lbl.as_deref())],
    )
    ?;
    Ok(url)
}

/// 移除登记。生效中的那条要一起回落，否则之后所有提交与取图都往一个不存在的地址打
pub fn remove_custom(ctx: &Ctx, raw_url: &str) -> crate::error::Result<String> {
    let url = norm_url(raw_url)?;
    repo::run(ctx, "DELETE FROM backends WHERE url=?", &[repo::s(&url)])?;
    if active_url(ctx) == url {
        repo::settings::del(ctx, "comfy_active")?;
    }
    Ok(active_url(ctx))
}

pub fn active_url(ctx: &Ctx) -> String {
    repo::settings::get(ctx, "comfy_active").unwrap_or_else(|| DEFAULT_URL.into())
}

pub fn set_active(ctx: &Ctx, raw_url: &str) -> crate::error::Result<String> {
    let url = norm_url(raw_url)?;
    repo::settings::put(ctx, "comfy_active", &url)?;
    Ok(active_url(ctx))
}

/// 扫描 = 预置端口 + 已登记的自定义地址，并行探活后按延迟排序
pub async fn scan(ctx: &Ctx) -> Vec<Value> {
    let custom = list_custom(ctx);
    let mut urls: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for p in SCAN_PORTS {
        let u = format!("http://127.0.0.1:{p}");
        if seen.insert(u.clone()) {
            urls.push(u);
        }
    }
    let known: HashSet<String> = custom
        .iter()
        .filter_map(|b| b.get("url").and_then(|u| u.as_str()).map(str::to_string))
        .collect();
    for u in &known {
        if seen.insert(u.clone()) {
            urls.push(u.clone());
        }
    }
    let probes = futures_util::future::join_all(urls.iter().map(|u| probe(ctx, u))).await;
    let mut found: Vec<Value> = probes
        .into_iter()
        .map(|mut v| {
            let url = v.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
            if let Some(o) = v.as_object_mut() {
                o.insert("saved".into(), Value::Bool(known.contains(&url)));
            }
            v
        })
        .filter(|v| v.get("ok").and_then(Value::as_bool).unwrap_or(false))
        .collect();
    found.sort_by_key(|v| v.get("ms").and_then(|m| m.as_i64()).unwrap_or(i64::MAX));
    found
}

/// 探活的结论写进 JSON，所以交钥匙 + 参数：界面按当前语言查这一句
fn err_fields(e: AppError) -> Map<String, Value> {
    let (code, args) = e.reason();
    let mut o = Map::new();
    o.insert("error".into(), Value::String(code));
    if let Some(a) = args {
        o.insert("error_args".into(), Value::Object(a));
    }
    o
}

/// 传输层失败：种类在这里判、句子在字典里。URL 已经在 `error_args` 里单独占一格。
fn send_err(url: &str, e: reqwest::Error) -> AppError {
    let code = if e.is_timeout() {
        "srv.backend.timeout"
    } else if e.is_connect() {
        "srv.backend.connect"
    } else {
        "srv.backend.send"
    };
    AppError::detailed_args(502, code, json!({ "url": url.to_string() }), crate::util::reqwest_detail(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 断言看的是钥匙不是句子：措辞改了不该让单测红
    fn why(r: crate::error::Result<String>) -> String {
        match r {
            Ok(_) => String::new(),
            Err(e) => e.reason().0,
        }
    }

    #[test]
    fn norm_url_与_js_同判() {
        assert_eq!(norm_url("127.0.0.1:8188").unwrap(), "http://127.0.0.1:8188");
        assert_eq!(norm_url("http://x.test/comfy/").unwrap(), "http://x.test/comfy");
        assert_eq!(norm_url("https://x.test:8443").unwrap(), "https://x.test:8443");
        assert_eq!(why(norm_url("")), "srv.url.empty");
        assert_eq!(why(norm_url("ftp://x")), "srv.url.scheme");
        assert_eq!(why(norm_url("http://u:p@x")), "srv.url.credentials");
        assert_eq!(why(norm_url("http://x?a=1")), "srv.url.query");
        assert_eq!(why(norm_url("http://x#f")), "srv.url.query");
    }

    #[test]
    fn norm_base_保留结尾斜杠_norm_url_不保留() {
        assert_eq!(norm_base("api.example.com").unwrap(), "https://api.example.com/");
        assert_eq!(norm_base("https://api.example.com/v1/").unwrap(), "https://api.example.com/v1");
        assert_eq!(why(norm_base("file:///x")), "srv.url.scheme");
        assert_eq!(why(norm_base("")), "srv.url.baseEmpty");
    }

    #[test]
    fn join_不吃掉子路径() {
        assert_eq!(join("http://h/proxy/", "/prompt"), "http://h/proxy/prompt");
        assert_eq!(join("http://h", "/view"), "http://h/view");
    }
}
