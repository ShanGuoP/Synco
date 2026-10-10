//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{bad, bad_args, body_of, ok};
use crate::service::workflow as cfg;
use crate::error::{AppError, Result};
use crate::repo::{images as rimg, results as rres, settings as rset};
use crate::service::backend;
use crate::service::queue;
use crate::state::Shared;
use crate::util;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde_json::{json, Value};

use std::path::{Path, PathBuf};

pub async fn api_settings(State(ctx): State<Shared>) -> Result<Response> {
    Ok(ok(serde_json::json!({
        "workflow_path": cfg::workflow_path(&ctx),
        "comfy": backend::active_url(&ctx),
        "proxy_edge": crate::service::imagesvc::proxy_edge(&ctx),
        "lang": lang_of(&ctx),
    })))
}

/// 界面语言只认 `zh` / `en`。别的（拼错的、字典里还没有的）一律夹回 zh：
/// 装一个不存在的花名会让前端拿不到字典，比退回默认糟得多。与 proxy_edge 同一惯例——夹住并回显。
pub fn normalize_lang(v: &str) -> &'static str {
    if v.eq_ignore_ascii_case("en") { "en" } else { "zh" }
}

pub fn lang_of(ctx: &crate::state::Ctx) -> String {
    rset::get(ctx, "lang").unwrap_or_else(|| "zh".to_string())
}

pub async fn lang_set(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let want = normalize_lang(body.get("lang").and_then(|v| v.as_str()).unwrap_or(""));
    rset::put(&ctx, "lang", want)?;
    Ok(ok(serde_json::json!({ "lang": want })))
}

/// 档位旋钮：proxy 长边改了不用手动清档——档位写进文件名，旧 URL 自然失效，
/// 新档位由首次访问懒切出来
pub async fn proxy_edge_set(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let want = util::clamp_round(body.get("proxy_edge"), 1024, 8192, crate::service::imagesvc::DEFAULT_PROXY_EDGE as i64);
    rset::put(&ctx, "proxy_edge", &want.to_string())?;
    Ok(ok(serde_json::json!({ "proxy_edge": want })))
}

/// 可再生档那一层（`runtime/cache/`）的体检：现在占多少、上限多少。
/// 数目录是磁盘活（一次可能上百个子目录、上千个文件），过阻塞池，别按住 worker
pub async fn cache_get(State(ctx): State<Shared>) -> Result<Response> {
    let c = ctx.clone();
    let (bytes, cap) = util::blocking(move || {
        Ok((crate::service::imagesvc::cache_bytes(&c), crate::service::imagesvc::cache_cap(&c)))
    })
    .await?;
    Ok(ok(json!({ "bytes": bytes, "cap_mb": cap / 1048576 })))
}

/// 手动清一次。丢掉的随时能重算回来，所以这一颗按钮不需要"确认后再删"之外的犹豫
pub async fn cache_clear(State(ctx): State<Shared>) -> Result<Response> {
    let c = ctx.clone();
    let (gone, freed) = util::blocking(move || Ok(crate::service::imagesvc::cache_clear(&c))).await?;
    Ok(ok(json!({ "removed": gone, "bytes": freed })))
}

/// 上限（MB，0 = 不限）。夹住并回显，改完立刻扫一次——"调小就马上腾地方"才成立
pub async fn cache_cap_set(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let mb = util::clamp_round(body.get("mb"), 0, 65536, 4096);
    rset::put(&ctx, "cache_cap_mb", &mb.to_string())?;
    let c = ctx.clone();
    let (gone, freed) = util::blocking(move || Ok(crate::service::imagesvc::cache_sweep(&c, mb as u64 * 1048576))).await?;
    Ok(ok(json!({ "cap_mb": mb, "removed": gone, "bytes": freed })))
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

/// 把工作流文件里的节点摊出来。角色映射接管提交之前，人得先看得见自己文件里有什么。
/// 只认库里存着的那条路径：这个接口不接路径参数，免得变成一个任意本地文件的读取口。
pub async fn workflow_inspect(State(ctx): State<Shared>) -> Result<Response> {
    let p = cfg::workflow_path(&ctx);
    if p.trim().is_empty() {
        return Ok(bad("srv.settings.noWorkflowPath"));
    }
    Ok(ok(cfg::inspect(Path::new(&p))?))
}

/// 角色映射表：这个文件里有哪些节点、自动认出了谁、你手指过谁、还差什么。
/// 接管提交之前，人得先看得见这张表对不对。
pub async fn workflow_roles_get(State(ctx): State<Shared>) -> Result<Response> {
    Ok(ok(cfg::roles_view(&ctx)?))
}

/// 存手指覆盖。类名对不上的一律不入库并逐条给理由——存进去一个错类名的节点，
/// 下一次提交就会往不该填的输入里写值。
pub async fn workflow_roles_post(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let p = cfg::workflow_path(&ctx);
    let roles = body.get("roles").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let (kept, rejected) = cfg::set_saved_roles(&ctx, &p, &roles);
    let view = cfg::roles_view(&ctx)?;
    let mut out = view.as_object().cloned().unwrap_or_default();
    out.insert("saved".into(), serde_json::Value::Object(kept));
    out.insert("rejected".into(), serde_json::json!(rejected));
    Ok(ok(serde_json::Value::Object(out)))
}

pub async fn cfg_get(State(ctx): State<Shared>) -> Result<Response> {    let c = cfg::get_cfg(&ctx);
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
pub(crate) fn probe_export_dir(dir: &str) -> (bool, &'static str, Option<Value>, Option<PathBuf>) {
    let raw = dir.trim();
    let abs = {
        let b = raw.as_bytes();
        let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b.get(2).is_some_and(|c| *c == b'\\' || *c == b'/');
        let unc = raw.starts_with("\\\\");
        drive || unc
    };
    if !abs {
        return (false, "srv.settings.exportAbsNeeded", None, None);
    }
    let p = Path::new(raw).to_path_buf();
    match std::fs::create_dir_all(&p).and_then(|_| {
        let probe = p.join(".synco-write-test");
        std::fs::write(&probe, "ok")?;
        std::fs::remove_file(&probe)
    }) {
        Ok(_) => (true, "", None, Some(p)),
        Err(e) => (false, "srv.settings.exportNotWritable",
            Some(json!({ "msg": e.to_string().chars().take(120).collect::<String>() })), None),
    }
}

/// 只读地把当前配置说一下：这条是 GET，不建目录也不写探针——
/// 可写性由 `export_dir_set` 与 `export_run` 那两次真写去验，网页用一个 `<img>` 不该替用户建目录
pub async fn export_get(State(ctx): State<Shared>) -> Response {
    let dir = get_export_dir(&ctx);
    let ready = !dir.is_empty() && Path::new(dir.trim()).is_dir();
    ok(serde_json::json!({ "dir": dir, "ready": ready }))
}

pub async fn export_dir_set(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let (okp, code, args, p) = probe_export_dir(body.get("dir").and_then(|v| v.as_str()).unwrap_or(""));
    if !okp {
        return Ok(bad_args(code, args.unwrap_or(Value::Null)));
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
        return Ok(bad("srv.settings.noExportIds"));
    }
    // 一次导出的张数上限：body 那 80MB 限的是字节不是条数，一万个 id 能把 CPU 与磁盘 IO 排满
    const EXPORT_MAX: usize = 500;
    if ids.len() > EXPORT_MAX {
        return Ok(bad_args("srv.export.batchMax", json!({ "cap": EXPORT_MAX, "got": ids.len() })));
    }
    let dir = get_export_dir(&ctx);
    if dir.is_empty() {
        return Ok(bad("srv.settings.exportUnset"));
    }
    let (okp, code, args, p) = probe_export_dir(&dir);
    if !okp {
        return Ok(bad_args(code, args.unwrap_or(Value::Null)));
    }
    let p = p.unwrap();
    let mut out: Vec<Value> = Vec::new();
    for id in ids {
        let Some(r) = rres::value_by_id(&ctx, id)? else {
            out.push(serde_json::json!({ "id": id, "skipped": true, "reason": "srv.settings.noResultFile" }));
            continue;
        };
        let final_path = r.get("final_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        // 库里的 rel 走统一校验再取路径：状态对得上但文件指在 data/ 外（被改过的库、链接）也不导
        let final_src = util::data_file(&ctx.data, &final_path);
        if r.get("status").and_then(|v| v.as_str()) != Some("done") || final_src.is_none() {
            out.push(serde_json::json!({ "id": id, "skipped": true, "reason": "srv.settings.noResultFile" }));
            continue;
        }
        let iid = r.get("image_id").and_then(|v| v.as_i64()).unwrap_or(0);
        let name = rimg::name_and_size(&ctx, iid)?.map(|(n, _, _)| n).unwrap_or_else(|| "photo".to_string());
        let stem: String = {
            let cleaned: String = util::stem_of(&name)
                .chars()
                .map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
                .collect();
            util::clip(&if cleaned.is_empty() { "photo".to_string() } else { cleaned }, 60)
        };
        /* 导出要的是无损那一张：按这一行的快照把当年那次缝合重算一遍（同一批纯函数、同一份参数）。
           重算不成才交成图那一档 q95，并把原因随这一条带回——批处理里一张降级不该让整批停下，
           但也不能不给个说法：用户会以为手里那张 PNG 与屏幕上是同一回事 */
        let row = crate::models::entity::ResultRow::from_value(&r);
        let ctx2 = ctx.clone();
        let got = match util::blocking(move || queue::lossless_bytes(&ctx2, &row)).await {
            Ok(g) => g,
            Err(e) => {
                out.push(serde_json::json!({ "id": id, "skipped": true, "reason": e.text().chars().take(120).collect::<String>() }));
                continue;
            }
        };
        let ext = got.ext;
        let bytes = got.bytes;
        let lossless = got.lossless;
        let why = got.why;
        /* 名字用 create_new 原子占，不再"先看存在不存在再复制"：两个导出请求同时看到没这个名字，
           就会都挑第一个，后落的那个把前一个覆盖掉。
           挑名与写盘都是同步磁盘活（一次最多 500 张），整段过阻塞池 */
        let copied = {
            let p = p.clone();
            util::blocking(move || -> Result<String> {
                let mut file = format!("{stem}_#{id}.{ext}");
                let mut n = 2;
                loop {
                    let cand = p.join(&file);
                    match std::fs::OpenOptions::new().create_new(true).write(true).open(&cand) {
                        Ok(_) => break,
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                            if n >= 1000 {
                                return Err(AppError::fail_args("srv.settings.nameTaken", json!({ "file": file, "n": n })));
                            }
                            file = format!("{stem}_#{id}({n}).{ext}");
                            n += 1;
                        }
                        Err(e) => return Err(AppError::Fail(e.to_string())),
                    }
                }
                std::fs::write(&p.join(&file), &bytes).map_err(|e| AppError::Fail(e.to_string()))?;
                Ok(file)
            })
            .await
        };
        match copied {
            Ok(file) => out.push(serde_json::json!({ "id": id, "file": file, "lossless": lossless, "why": why })),
            Err(e) => out.push(serde_json::json!({
                "id": id, "skipped": true,
                "reason": e.to_string().chars().take(120).collect::<String>()
            })),
        }
    }
    Ok(ok(serde_json::json!({ "dir": p.to_string_lossy(), "files": out })))
}
