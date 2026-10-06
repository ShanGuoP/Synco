//! ComfyUI 后端注册表：扫描本机端口、登记自定义接口、决定当前生效地址（对齐 backend.js）。

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
pub fn norm_url(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("地址不能为空".into());
    }
    let with_scheme = if has_scheme(s) { s.to_string() } else { format!("http://{s}") };
    let u = Url::parse(&with_scheme).map_err(|_| "地址格式不对".to_string())?;
    match u.scheme() {
        "http" | "https" => {}
        _ => return Err("只支持 http / https".into()),
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("地址里不要带账号密码".into());
    }
    if u.query().is_some() || u.fragment().is_some() {
        return Err("地址里不要带查询参数".into());
    }
    let path = u.path().trim_end_matches('/').to_string();
    Ok(format!("{}{path}", u.origin().ascii_serialization()))
}

/// cloud.normBase 与 backend.normUrl 的返回写法不同：这里保留 pathname 原样（含结尾斜杠），
/// 与 JS 的 `u.origin + u.pathname` 一字不差，否则双跑时设置里的 base 会对不上
pub fn norm_base(raw: &str) -> Result<String, String> {
    let s = raw.trim().trim_end_matches('/').to_string();
    if s.is_empty() {
        return Err("base_url 不能为空".into());
    }
    let with_scheme = if has_scheme(&s) { s.clone() } else { format!("https://{s}") };
    let u = Url::parse(&with_scheme).map_err(|_| "base_url 格式不对".to_string())?;
    match u.scheme() {
        "http" | "https" => {}
        _ => return Err("只支持 http / https".into()),
    }
    if u.host_str().unwrap_or("").is_empty() {
        return Err("base_url 缺主机名".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("base_url 里不要带账号密码".into());
    }
    if u.query().is_some() || u.fragment().is_some() {
        return Err("base_url 里不要带查询参数".into());
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

async fn get_json(ctx: &Ctx, url: &str) -> Result<Value, String> {
    let r = ctx
        .http
        .get(url)
        .timeout(Duration::from_millis(PROBE_MS))
        .send()
        .await
        .map_err(|e| e.strip_err())?;
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status().as_u16()));
    }
    r.json::<Value>().await.map_err(|e| e.strip_err())
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
            out.insert("error".into(), Value::String(e));
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
                    out.insert("version".into(), Value::String("未知（老版本，无版本接口）".into()));
                } else {
                    out.insert(
                        "error".into(),
                        Value::String(if head.is_empty() { "空应答".into() } else { "应答了，但不是 ComfyUI".into() }),
                    );
                }
            }
            Err(e) => {
                let msg = e.strip_err();
                out.insert("error".into(), Value::String(clip(&msg, 90)));
            }
        }
    }
    out.insert("ms".into(), Value::from(t0.elapsed().as_millis() as i64));
    Value::Object(out)
}

pub fn list_custom(ctx: &Ctx) -> Vec<Value> {
    repo::all(ctx, "SELECT url, label, added_at FROM backends ORDER BY added_at", &[]).unwrap_or_default()
}

pub fn add_custom(ctx: &Ctx, raw_url: &str, label: Option<&str>) -> Result<String, String> {
    let url = norm_url(raw_url)?;
    let lbl = label.map(|l| clip(l, 40)).filter(|l| !l.is_empty());
    repo::run(
        ctx,
        "INSERT INTO backends(url,label) VALUES(?,?) ON CONFLICT(url) DO UPDATE SET label=excluded.label",
        &[repo::s(&url), repo::si(lbl.as_deref())],
    )
    .map_err(|e| e.to_string())?;
    Ok(url)
}

/// 移除登记。生效中的那条要一起回落，否则之后所有提交与取图都往一个不存在的地址打
pub fn remove_custom(ctx: &Ctx, raw_url: &str) -> Result<String, String> {
    let url = norm_url(raw_url)?;
    repo::run(ctx, "DELETE FROM backends WHERE url=?", &[repo::s(&url)]).map_err(|e| e.to_string())?;
    if active_url(ctx) == url {
        repo::settings::del(ctx, "comfy_active").map_err(|e| e.to_string())?;
    }
    Ok(active_url(ctx))
}

pub fn active_url(ctx: &Ctx) -> String {
    repo::settings::get(ctx, "comfy_active").unwrap_or_else(|| DEFAULT_URL.into())
}

pub fn set_active(ctx: &Ctx, raw_url: &str) -> Result<String, String> {
    let url = norm_url(raw_url)?;
    repo::settings::put(ctx, "comfy_active", &url).map_err(|e| e.to_string())?;
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

trait StripErr {
    fn strip_err(self) -> String;
}
impl StripErr for reqwest::Error {
    fn strip_err(self) -> String {
        // reqwest 的 Display 会带上 URL，探活失败的文案要短，且不能把带 key 的 URL 漏出去
        let kind = if self.is_timeout() {
            "超时"
        } else if self.is_connect() {
            "连不上"
        } else {
            "请求失败"
        };
        format!("{kind}：{}", self).chars().take(200).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_url_与_js_同判() {
        assert_eq!(norm_url("127.0.0.1:8188").unwrap(), "http://127.0.0.1:8188");
        assert_eq!(norm_url("http://x.test/comfy/").unwrap(), "http://x.test/comfy");
        assert_eq!(norm_url("https://x.test:8443").unwrap(), "https://x.test:8443");
        assert_eq!(norm_url(""), Err("地址不能为空".to_string()));
        assert_eq!(norm_url("ftp://x"), Err("只支持 http / https".to_string()));
        assert_eq!(norm_url("http://u:p@x"), Err("地址里不要带账号密码".to_string()));
        assert_eq!(norm_url("http://x?a=1"), Err("地址里不要带查询参数".to_string()));
        assert_eq!(norm_url("http://x#f"), Err("地址里不要带查询参数".to_string()));
    }

    #[test]
    fn norm_base_保留结尾斜杠_norm_url_不保留() {
        assert_eq!(norm_base("api.example.com").unwrap(), "https://api.example.com/");
        assert_eq!(norm_base("https://api.example.com/v1/").unwrap(), "https://api.example.com/v1");
        assert_eq!(norm_base("file:///x"), Err("只支持 http / https".to_string()));
        assert_eq!(norm_base(""), Err("base_url 不能为空".to_string()));
    }

    #[test]
    fn join_不吃掉子路径() {
        assert_eq!(join("http://h/proxy/", "/prompt"), "http://h/proxy/prompt");
        assert_eq!(join("http://h", "/view"), "http://h/view");
    }
}
