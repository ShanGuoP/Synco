//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{bad, body_of, ok};
use crate::service::workflow as cfg;
use crate::error::Result;
use crate::repo;
use crate::repo::{results as rres, settings as rset};
use crate::service::backend;
use crate::state::Shared;
use crate::util;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde_json::Value;

use std::path::{Path, PathBuf};

pub async fn api_settings(State(ctx): State<Shared>) -> Result<Response> {
    Ok(ok(serde_json::json!({
        "workflow_path": cfg::workflow_path(&ctx),
        "comfy": backend::active_url(&ctx),
        "proxy_edge": crate::service::imagesvc::proxy_edge(&ctx),
    })))
}

/// 档位旋钮：proxy 长边改了不用手动清档——档位写进文件名，旧 URL 自然失效，
/// 新档位由首次访问懒切出来
pub async fn proxy_edge_set(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let want = util::clamp_round(body.get("proxy_edge"), 1024, 8192, crate::service::imagesvc::DEFAULT_PROXY_EDGE as i64);
    rset::put(&ctx, "proxy_edge", &want.to_string())?;
    Ok(ok(serde_json::json!({ "proxy_edge": want })))
}

pub async fn workflow_set(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let p = cfg::set_workflow_path(&ctx, body.get("path").and_then(|v| v.as_str()).unwrap_or(""));
    let c = cfg::get_cfg(&ctx);
    Ok(ok(serde_json::json!({
        "workflow_path": p,
        "cfg_source": c.get("cfgSource").cloned().unwrap_or(Value::Null),
        "cfg_error": c.get("workflowError").cloned().unwrap_or(Value::Null),
    })))
}

pub async fn cfg_get(State(ctx): State<Shared>) -> Result<Response> {
    let c = cfg::get_cfg(&ctx);
    Ok(ok(serde_json::json!({
        "prompt_default": c["prompt_default"], "negative": c["negative"], "steps": c["steps"], "cfg": c["cfg"],
        "loras": c["loras"], "comfy": backend::active_url(&ctx),
        "cfg_source": c["cfgSource"], "workflow_path": c["workflowPath"], "workflow_error": c["workflowError"],
    })))
}

pub(crate) fn get_export_dir(ctx: &Shared) -> String {
    rset::get(ctx, "export_dir").unwrap_or_default()
}

/// 绝对路径 + 可写探测：建目录、写探针文件再删掉。U 盘拔了这类情况在导出时报错而不是保存时。
/// 盘符/UNC 前缀必须验原始字符串——先 resolve 再判 isAbsolute 的话，
/// 相对路径已经被补成 CWD 下的绝对路径，防线就失效了。
pub(crate) fn probe_export_dir(dir: &str) -> (bool, String, Option<PathBuf>) {
    let raw = dir.trim();
    let abs = {
        let b = raw.as_bytes();
        let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b.get(2).is_some_and(|c| *c == b'\\' || *c == b'/');
        let unc = raw.starts_with("\\\\");
        drive || unc
    };
    if !abs {
        return (false, "要填以盘符开头的绝对路径，例如 D:\\导出\\成图".into(), None);
    }
    let p = Path::new(raw).to_path_buf();
    match std::fs::create_dir_all(&p).and_then(|_| {
        let probe = p.join(".synco-write-test");
        std::fs::write(&probe, "ok")?;
        std::fs::remove_file(&probe)
    }) {
        Ok(_) => (true, String::new(), Some(p)),
        Err(e) => (false, format!("目录不可写：{}", e.to_string().chars().take(120).collect::<String>()), None),
    }
}

pub async fn export_get(State(ctx): State<Shared>) -> Response {
    let dir = get_export_dir(&ctx);
    let ready = if dir.is_empty() { false } else { probe_export_dir(&dir).0 };
    ok(serde_json::json!({ "dir": dir, "ready": ready }))
}

pub async fn export_dir_set(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let (okp, e, p) = probe_export_dir(body.get("dir").and_then(|v| v.as_str()).unwrap_or(""));
    if !okp {
        return Ok(bad(e));
    }
    let p = p.unwrap();
    rset::put(&ctx, "export_dir", &p.to_string_lossy()).map_err(|e| e.to_string())?;
    Ok(ok(serde_json::json!({ "ok": true, "dir": p.to_string_lossy() })))
}

pub async fn export_run(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let ids: Vec<i64> = body
        .get("result_ids")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| match v {
            Value::Number(n) => n.as_i64().filter(|x| *x != 0),
            Value::String(s) => s.parse::<i64>().ok().filter(|x| *x != 0),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        return Ok(bad("没有要导出的结果"));
    }
    let dir = get_export_dir(&ctx);
    if dir.is_empty() {
        return Ok(bad("还没设置导出目录（设置 → 导出目录）"));
    }
    let (okp, e, p) = probe_export_dir(&dir);
    if !okp {
        return Ok(bad(e));
    }
    let p = p.unwrap();
    let mut out: Vec<Value> = Vec::new();
    for id in ids {
        let Some(r) = rres::value_by_id(&ctx, id)? else {
            out.push(serde_json::json!({ "id": id, "skipped": true, "reason": "这条没有成图" }));
            continue;
        };
        let final_path = r.get("final_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if r.get("status").and_then(|v| v.as_str()) != Some("done") || final_path.is_empty() || !ctx.data.join(&final_path).is_file() {
            out.push(serde_json::json!({ "id": id, "skipped": true, "reason": "这条没有成图" }));
            continue;
        }
        let iid = r.get("image_id").and_then(|v| v.as_i64()).unwrap_or(0);
        let src = repo::one(&ctx, "SELECT name FROM images WHERE id=?", &[repo::i(iid)])?;
        let name = src.as_ref().and_then(|s| s.get("name").and_then(|v| v.as_str())).unwrap_or("photo").to_string();
        let stem: String = {
            let cleaned: String = util::stem_of(&name)
                .chars()
                .map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
                .collect();
            util::clip(&if cleaned.is_empty() { "photo".to_string() } else { cleaned }, 60)
        };
        // 重名自动 (2)(3) 递增
        let mut file = format!("{stem}_#{id}.png");
        let mut n = 2;
        while p.join(&file).exists() {
            file = format!("{stem}_#{id}({n}).png");
            n += 1;
        }
        match std::fs::copy(ctx.data.join(&final_path), p.join(&file)) {
            Ok(_) => out.push(serde_json::json!({ "id": id, "file": file })),
            Err(e) => out.push(serde_json::json!({
                "id": id, "skipped": true,
                "reason": e.to_string().chars().take(120).collect::<String>()
            })),
        }
    }
    Ok(ok(serde_json::json!({ "dir": p.to_string_lossy(), "files": out })))
}
