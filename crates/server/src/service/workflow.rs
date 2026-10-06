//! 工作流参数：优先读本机 Qwen 高分局部编辑工作流，读不到就退回随项目走的 defaults.json。
//! 没装 ComfyUI / 工作流路径不对都不能让服务起不来，所以这里全程不抛异常。

use crate::state::Ctx;
use serde_json::{Map, Value};
use std::path::Path;
use std::time::UNIX_EPOCH;

pub const DEFAULTS_RAW: &str = include_str!("../../defaults.json");

/// 裁切参数是按数组下标硬映射的（comfy.rs 用 CROP_KEYS[i] 取 crop_widgets[i]）。
/// 插件升级加一个控件就会整体错位——mask_expand_pixels 拿到 device_mode 的值，
/// 出图变样且全程没有报错，所以下面有形状校验兜底。
pub const CROP_KEYS: [&str; 24] = [
    "downscale_algorithm",
    "upscale_algorithm",
    "preresize",
    "preresize_mode",
    "preresize_min_width",
    "preresize_min_height",
    "preresize_max_width",
    "preresize_max_height",
    "mask_fill_holes",
    "mask_expand_pixels",
    "mask_invert",
    "mask_blend_pixels",
    "mask_hipass_filter",
    "extend_for_outpainting",
    "extend_up_factor",
    "extend_down_factor",
    "extend_left_factor",
    "extend_right_factor",
    "context_from_mask_extend_factor",
    "output_resize_to_target_size",
    "output_target_width",
    "output_target_height",
    "output_padding",
    "device_mode",
];

pub fn defaults() -> &'static Value {
    static D: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    D.get_or_init(|| serde_json::from_str(DEFAULTS_RAW).expect("defaults.json 必须是合法 JSON"))
}

fn dget(k: &str) -> Value {
    defaults().get(k).cloned().unwrap_or(Value::Null)
}

/// 内置基线：只带 buildGraph 用得上的键，sources/workflow_sample 不进这里
fn builtin() -> Map<String, Value> {
    let mut m = Map::new();
    for k in [
        "unet", "clip", "clip_type", "vae", "negative", "steps", "cfg", "sampler", "scheduler",
        "prompt_default", "crop_widgets",
    ] {
        m.insert(k.into(), dget(k));
    }
    m.insert("loras".into(), dget("loras"));
    m
}

fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "object",
        Value::Object(_) => "object",
    }
}

fn shape_of(arr: &Value) -> String {
    match arr.as_array() {
        Some(a) => a.iter().map(type_of).collect::<Vec<_>>().join(","),
        None => String::new(),
    }
}

/// 长度或逐项类型与内置基线对不上就判不匹配，宁可退回内置参数
fn crop_shape_ok(wf: &Value, base: &Value) -> bool {
    match (wf.as_array(), base.as_array()) {
        (Some(a), Some(b)) => a.len() == b.len() && shape_of(wf) == shape_of(base),
        _ => false,
    }
}

fn widgets(nodes: &[Value], t: &str) -> Vec<Value> {
    let mut active = nodes.iter().filter(|n| n["type"].as_str() == Some(t) && n["mode"].as_i64().unwrap_or(0) == 0);
    let hit = active.next().or_else(|| nodes.iter().find(|n| n["type"].as_str() == Some(t)));
    match hit.and_then(|n| n["widgets_values"].as_array()) {
        Some(a) => a.clone(),
        None => Vec::new(),
    }
}

/// 解析 litegraph 工作流：取关键节点控件值，并沿 MODEL 连接把 LoRA 链串出来
fn read_workflow(p: &Path) -> Result<Map<String, Value>, String> {
    let text = std::fs::read_to_string(p).map_err(|e| format!("读不到工作流：{e}"))?;
    let d: Value = serde_json::from_str(&text).map_err(|e| format!("工作流不是合法 JSON：{e}"))?;
    let nodes = d["nodes"].as_array().cloned().unwrap_or_default();
    let links = d["links"].as_array().cloned().unwrap_or_default();
    let w = |t: &str| widgets(&nodes, t);
    let nth = |v: &Vec<Value>, k: usize| v.get(k).cloned().unwrap_or(Value::Null);

    let link_by_id: Map<String, Value> = links
        .iter()
        .filter_map(|l| l.get(0).and_then(|id| id.as_i64()).map(|id| (id.to_string(), l.clone())))
        .collect();
    let model_src = |node: &Value| -> Option<i64> {
        let inp = node["inputs"].as_array()?.iter().find(|i| i["type"].as_str() == Some("MODEL") && i["link"].is_i64())?;
        let lid = inp["link"].as_i64()?.to_string();
        link_by_id
            .get(&lid)
            .and_then(|l| l.get(1).and_then(|x| x.as_i64()))
    };

    let loras: Vec<Value> = nodes.iter().filter(|n| n["type"].as_str() == Some("LoraLoaderModelOnly")).cloned().collect();
    let unet_id = nodes.iter().find(|n| n["type"].as_str() == Some("UNETLoader")).and_then(|n| n["id"].as_i64());
    let mut chain: Vec<Value> = Vec::new();
    let mut cur = unet_id;
    while let Some(id) = cur {
        match loras.iter().find(|l| model_src(l) == Some(id)) {
            Some(nx) => {
                chain.push(nx.clone());
                cur = nx["id"].as_i64();
            }
            None => break,
        }
    }

    let ps = nodes.iter().find(|n| n["type"].as_str() == Some("PrimitiveStringMultiline"));
    let prompt_default = ps
        .and_then(|n| n["widgets_values"].as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::String(String::new()));

    let ks = w("KSampler");
    let te = w("TextEncodeQwenImage21");
    let clipl = w("CLIPLoader");
    let mut m = Map::new();
    m.insert("unet".into(), nth(&w("UNETLoader"), 0));
    m.insert("clip".into(), nth(&clipl, 0));
    m.insert("clip_type".into(), nth(&clipl, 1));
    m.insert("vae".into(), nth(&w("VAELoader"), 0));
    m.insert("negative".into(), nth(&te, 1));
    m.insert("steps".into(), nth(&ks, 2));
    m.insert("cfg".into(), nth(&ks, 3));
    m.insert("sampler".into(), nth(&ks, 4));
    m.insert("scheduler".into(), nth(&ks, 5));
    m.insert("prompt_default".into(), prompt_default);
    m.insert("crop_widgets".into(), Value::Array(w("InpaintCropImproved")));
    m.insert(
        "loras".into(),
        Value::Array(
            chain
                .iter()
                .map(|l| {
                    let wv = l["widgets_values"].as_array().cloned().unwrap_or_default();
                    let mut o = Map::new();
                    o.insert("name".into(), wv.first().cloned().unwrap_or(Value::String(String::new())));
                    o.insert("strength".into(), wv.get(1).cloned().unwrap_or(Value::Null));
                    o.insert("enabled".into(), Value::Bool(l["mode"].as_i64().unwrap_or(0) == 0));
                    Value::Object(o)
                })
                .collect(),
        ),
    );
    Ok(m)
}

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
    let mut error: Option<String> = None;
    if mtime == 0 {
        error = Some("工作流文件不存在".into());
    } else {
        match read_workflow(Path::new(&p)) {
            Ok(wf) => {
                let mut wf = wf;
                // 裁切控件错位不会抛错，只会静默出坏图：形状对不上就当这一项没读到
                let crop = wf.get("crop_widgets").cloned().unwrap_or(Value::Null);
                if !crop.is_null() && !crop_shape_ok(&crop, &dget("crop_widgets")) {
                    error = Some(format!(
                        "工作流里 InpaintCropImproved 的控件与内置基线对不上（内置 {} 项，工作流 {} 项），裁切参数改用内置值",
                        dget("crop_widgets").as_array().map(|a| a.len()).unwrap_or(0),
                        crop.as_array().map(|a| a.len()).unwrap_or(0)
                    ));
                    wf.insert("crop_widgets".into(), Value::Null);
                }
                for (k, v) in wf {
                    if !is_empty(&v) {
                        cfg.insert(k, v);
                    }
                }
                source = "workflow";
            }
            Err(e) => error = Some(e.chars().take(200).collect()),
        }
    }
    cfg.insert("cfgSource".into(), Value::String(source.into()));
    cfg.insert("workflowPath".into(), Value::String(p.clone()));
    cfg.insert("workflowError".into(), match error { Some(e) => Value::String(e), None => Value::Null });
    let out = Value::Object(cfg);
    *ctx.cfg.lock().unwrap_or_else(|e| e.into_inner()) = Some(Cache { path: p, mtime, cfg: out.clone() });
    out
}

/// 供 setup.rs 用：把 defaults.json 里的下载基准取出来
pub fn sources() -> Map<String, Value> {
    defaults().get("sources").and_then(|v| v.as_object()).cloned().unwrap_or_default()
}
