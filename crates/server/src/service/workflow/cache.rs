//! 参数基线合并与按路径、mtime 缓存；缓存归当前 Ctx 所有。
use crate::state::Ctx;
use serde_json::{json, Value};
use std::{path::Path, time::UNIX_EPOCH};
use super::parse::{builtin, crop_shape_ok, dget};
use super::{load_map, why, why_err};
fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

pub struct Cache {
    pub path: String,
    pub mtime: u128,
    pub cfg: Value,
}

pub fn workflow_path(ctx: &Ctx) -> String {
    crate::repo::settings::get(ctx, "workflow_path").unwrap_or_else(|| dget("workflow_sample").as_str().unwrap_or_default().to_string())
}

pub fn set_workflow_path(ctx: &Ctx, p: &str) -> String {
    let v = p.trim();
    let stored = if v.is_empty() { dget("workflow_sample").as_str().unwrap_or_default().to_string() } else { v.to_string() };
    let _ = crate::repo::settings::put(ctx, "workflow_path", &stored);
    invalidate(ctx);
    workflow_path(ctx)
}

pub fn invalidate(ctx: &Ctx) {
    *ctx.cfg.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn mtime_ms(p: &str) -> u128 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0))
        .unwrap_or(0)
}

/// 返回 `{ ...参数, cfgSource, workflowPath, workflowError }`，与工作流文件的 mtime 绑一起做缓存
pub fn get_cfg(ctx: &Ctx) -> Value {
    let p = workflow_path(ctx);
    let mtime = mtime_ms(&p);
    {
        let guard = ctx.cfg.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.as_ref() {
            if c.path == p && c.mtime == mtime {
                return c.cfg.clone();
            }
        }
    }

    let mut cfg = builtin();
    let mut source = "builtin";
    let mut error: Option<Value> = None;
    if mtime == 0 {
        error = Some(why("srv.wf.fileMissing", Value::Null));
    } else {
        match load_map(Path::new(&p)) {
            Ok((mut wf, fmt)) => {
                // 裁切控件错位不会抛错，只会静默出坏图：litegraph 那支只有位置没有名字，
                // 形状对不上就当这一项没读到（API 那支按名取，不需要这条兜底）
                if fmt == "litegraph" {
                    let crop = wf.get("crop_widgets").cloned().unwrap_or(Value::Null);
                    if !crop.is_null() && !crop_shape_ok(&crop, &dget("crop_widgets")) {
                        error = Some(why(
                            "srv.wf.cropShape",
                            json!({
                                "want": dget("crop_widgets").as_array().map(|a| a.len()).unwrap_or(0),
                                "got": crop.as_array().map(|a| a.len()).unwrap_or(0),
                            }),
                        ));
                        wf.insert("crop_widgets".into(), Value::Null);
                    }
                }
                let mut got = 0usize;
                for (k, v) in wf {
                    if !is_empty(&v) {
                        cfg.insert(k, v);
                        got += 1;
                    }
                }
                // 解析成功但一项都没读到 = 这个文件里没有本管线认识的节点。
                // 以前这里照样把 cfgSource 标成 workflow，界面就谎称参数读自你的文件。
                if got > 0 {
                    source = "workflow";
                } else {
                    error = Some(why(
                        "srv.wf.noParams",
                        json!({ "kind": fmt_name(fmt) }),
                    ));
                }
            }
            Err(e) => error = Some(why_err(&e)),
        }
    }
    cfg.insert("cfgSource".into(), Value::String(source.into()));
    cfg.insert("workflowPath".into(), Value::String(p.clone()));
    cfg.insert("workflowError".into(), error.unwrap_or(Value::Null));
    let out = Value::Object(cfg);
    *ctx.cfg.lock().unwrap_or_else(|e| e.into_inner()) = Some(Cache { path: p, mtime, cfg: out.clone() });
    out
}

/// `api` / `litegraph` 两种格式的名字也交钥匙（那句"认出了×格式"里要用）
fn fmt_name(fmt: &str) -> Value {
    why(if fmt == "api" { "wf.format.api" } else { "wf.format.ui" }, Value::Null)
}
