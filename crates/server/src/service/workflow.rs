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

/// 一个文件里 LoRA 节点的上限：链是沿 MODEL 连接串的，环或畸形文件不能让它无限长
pub const LORA_MAX: usize = 16;

fn lora_json(chain: &[Value], name_of: &impl Fn(&Value) -> Value, strength_of: &impl Fn(&Value) -> Value, enabled_of: &impl Fn(&Value) -> bool) -> Value {
    Value::Array(
        chain
            .iter()
            .map(|l| {
                serde_json::json!({
                    "name": name_of(l),
                    "strength": strength_of(l),
                    "enabled": enabled_of(l),
                })
            })
            .collect(),
    )
}

/// UI/litegraph 导出：`{nodes:[{id,type,widgets_values,inputs:[{type,link}]}], links:[[id,from,…]]}`
fn read_litegraph(d: &Value, m: &mut Map<String, Value>) {
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
        link_by_id.get(&lid).and_then(|l| l.get(1).and_then(|x| x.as_i64()))
    };

    let loras: Vec<Value> = nodes.iter().filter(|n| n["type"].as_str() == Some("LoraLoaderModelOnly")).cloned().collect();
    let unet_id = nodes.iter().find(|n| n["type"].as_str() == Some("UNETLoader")).and_then(|n| n["id"].as_i64());
    // 从 unet 出发，每次找"上游是当前节点"的那个 LoRA。带 visited 与深度上限：
    // 合并过的手改 json 里两个 LoRA 可以有同一个 id，旧写法会在这里无限 push
    let mut chain: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut cur = unet_id;
    while let Some(id) = cur {
        if !seen.insert(id) || chain.len() >= LORA_MAX {
            break;
        }
        match loras.iter().find(|l| model_src(l) == Some(id)) {
            Some(nx) => {
                cur = nx["id"].as_i64();
                chain.push(nx.clone());
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
    // litegraph 只有位置没有名字，这一条仍是按 CROP_KEYS 的顺序约定的（有形状校验兜底）
    m.insert("crop_widgets".into(), Value::Array(w("InpaintCropImproved")));
    m.insert(
        "loras".into(),
        lora_json(
            &chain,
            &|l| l["widgets_values"].as_array().and_then(|a| a.first().cloned()).unwrap_or(Value::String(String::new())),
            &|l| l["widgets_values"].as_array().and_then(|a| a.get(1).cloned()).unwrap_or(Value::Null),
            &|l| l["mode"].as_i64().unwrap_or(0) == 0,
        ),
    );
}

/// API 导出：`{"<节点号>":{"class_type":"…","inputs":{名字: 值 或 [来源节点, 引脚]}}}`。
/// 按**名字**取参数，工作流里挪动节点顺序、加控件都不会让参数错位。
fn read_api(d: &Value, m: &mut Map<String, Value>) {
    let nodes = d.as_object();
    let class_of = |want: &str| -> Option<&Value> {
        nodes?.values().filter(|n| n["class_type"].as_str() == Some(want)).next()
    };
    // 输入项是 ["10", 0] 这种连接引用时它不是参数值
    let input = |n: Option<&Value>, key: &str| -> Value {
        match n.and_then(|x| x["inputs"].get(key)) {
            Some(Value::Array(_)) => Value::Null,
            Some(v) => v.clone(),
            None => Value::Null,
        }
    };
    let unet = class_of("UNETLoader");
    let clipl = class_of("CLIPLoader");
    let te = class_of("TextEncodeQwenImage21");
    let ks = class_of("KSampler");
    let crop = class_of("InpaintCropImproved");
    m.insert("unet".into(), input(unet, "unet_name"));
    m.insert("clip".into(), input(clipl, "clip_name"));
    m.insert("clip_type".into(), input(clipl, "type"));
    m.insert("vae".into(), input(class_of("VAELoader"), "vae_name"));
    m.insert("negative".into(), input(te, "negative_prompt"));
    m.insert("steps".into(), input(ks, "steps"));
    m.insert("cfg".into(), input(ks, "cfg"));
    m.insert("sampler".into(), input(ks, "sampler_name"));
    m.insert("scheduler".into(), input(ks, "scheduler"));
    m.insert(
        "prompt_default".into(),
        class_of("PrimitiveStringMultiline").and_then(|n| n["inputs"].get("value")).cloned().unwrap_or(Value::String(String::new())),
    );
    // 裁切参数按名直接给一张表，comfy.rs 不再靠位置 zip
    match crop.and_then(|n| n["inputs"].as_object()) {
        Some(o) => {
            let named: Map<String, Value> = o
                .iter()
                .filter(|(_, v)| !v.is_array())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            m.insert("crop_inputs".into(), Value::Object(named));
        }
        None => {
            m.insert("crop_inputs".into(), Value::Null);
        }
    }
    // LoRA 链：API 里 `inputs.model: ["<上游节点号>", 0]` 就是上游。
    // 建一张"上游节点号 → 挂在其后的 LoRA"的反向索引，顺链走，带 visited 与深度上限。
    let by_id: Map<String, Value> = nodes.map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    let upstream_model = |n: &Value| -> Option<String> {
        n["inputs"]["model"].as_array().and_then(|a| a.first()).and_then(|x| x.as_str()).map(str::to_string)
    };
    let is_lora = |n: &Value| n["class_type"].as_str() == Some("LoraLoaderModelOnly");
    let mut next_lora: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (id, n) in by_id.iter() {
        if is_lora(n) {
            if let Some(src) = upstream_model(n) {
                // 同一个上游挂多条时取第一条，与 litegraph 那支 find() 的语义一致
                next_lora.entry(src).or_insert_with(|| id.clone());
            }
        }
    }
    let seed = by_id.iter().find(|(_, n)| n["class_type"].as_str() == Some("UNETLoader")).map(|(k, _)| k.clone());
    let mut chain: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cur = seed;
    while let Some(id) = cur {
        if !seen.insert(id.clone()) || chain.len() >= LORA_MAX {
            break;
        }
        match next_lora.get(&id) {
            Some(nid) => {
                if let Some(n) = by_id.get(nid) {
                    chain.push(n.clone());
                    cur = Some(nid.clone());
                } else {
                    break;
                }
            }
            None => break,
        }
    }
    m.insert(
        "loras".into(),
        lora_json(
            &chain,
            &|l| l["inputs"]["lora_name"].clone(),
            &|l| l["inputs"]["strength_model"].clone(),
            // API 导出里节点可能被标成停用（enabled: 0），停用的不该进默认链
            &|l| match &l["enabled"] {
                Value::Bool(b) => *b,
                Value::Number(n) => n.as_f64().unwrap_or(1.0) != 0.0,
                _ => true,
            },
        ),
    );
}

/// 认格式：UI/litegraph 导出有 `nodes` 数组；API 导出是一张 `{节点号: {class_type, inputs}}` 的表
fn detect(d: &Value) -> Option<&'static str> {
    if d.get("nodes").and_then(|v| v.as_array()).is_some() {
        return Some("litegraph");
    }
    if d.as_object().map(|o| !o.is_empty() && o.values().any(|v| v.get("class_type").is_some())).unwrap_or(false) {
        return Some("api");
    }
    None
}

/// 读并解析一个工作流文件，返回（参数表，格式）。
/// 认不出格式时给的是**明确失败**而不是空表——以前 API 导出会解析成"零项"，
/// 而 cfgSource 照样标成 "workflow"，等于谎称参数读自你的文件。
pub fn load_map(p: &Path) -> Result<(Map<String, Value>, &'static str), String> {
    let text = std::fs::read_to_string(p).map_err(|e| format!("读不到工作流：{e}"))?;
    let d: Value = serde_json::from_str(&text).map_err(|e| format!("工作流不是合法 JSON：{e}"))?;
    let fmt = detect(&d).ok_or("这个 JSON 既没有 UI 导出的 nodes/links，也不是 API 导出的节点表（认不出是哪种工作流）")?;
    let mut m = Map::new();
    if fmt == "api" {
        read_api(&d, &mut m);
    } else {
        read_litegraph(&d, &mut m);
    }
    Ok((m, fmt))
}

/// 本管线会用的节点类名。清点端点按这张表报"在不在位"，第二步的角色表就从这里长出来。
pub const KNOWN_CLASSES: [&str; 13] = [
    "LoadImage", "LoadImageMask", "UNETLoader", "CLIPLoader", "VAELoader",
    "TextEncodeQwenImage21", "KSampler", "InpaintCropImproved", "InpaintStitchImproved",
    "LoraLoaderModelOnly", "SaveImage", "PreviewImage", "PrimitiveStringMultiline",
];

/// 把文件里的节点摊平成可比对的 `(节点号, 类名, 输入名)` 列表，两种格式同一套出口
pub fn inspect(p: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(p).map_err(|e| format!("读不到工作流：{e}"))?;
    let d: Value = serde_json::from_str(&text).map_err(|e| format!("工作流不是合法 JSON：{e}"))?;
    let fmt = detect(&d).ok_or("这个 JSON 既不是 ComfyUI 的 UI 导出，也不是 API 导出")?;
    let mut rows: Vec<Value> = Vec::new();
    if fmt == "api" {
        if let Some(o) = d.as_object() {
            for (id, n) in o {
                rows.push(serde_json::json!({
                    "id": id,
                    "class_type": n["class_type"].as_str().unwrap_or(""),
                    "inputs": n["inputs"].as_object().map(|i| i.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
                    "enabled": match &n["enabled"] { Value::Bool(b) => Value::Bool(*b), Value::Number(x) => Value::Bool(x.as_f64().unwrap_or(1.0) != 0.0), _ => Value::Bool(true) },
                }));
            }
        }
    } else if let Some(arr) = d["nodes"].as_array() {
        for n in arr {
            rows.push(serde_json::json!({
                "id": n["id"].to_string(),
                "class_type": n["type"].as_str().unwrap_or(""),
                "inputs": n["widgets_values"].as_array().map(|a| a.iter().enumerate().map(|(i, _)| format!("#{i}")).collect::<Vec<_>>()).unwrap_or_default(),
                "enabled": n["mode"].as_i64().unwrap_or(0) == 0,
            }));
        }
    }
    rows.sort_by(|a, b| a["class_type"].as_str().unwrap_or("").cmp(b["class_type"].as_str().unwrap_or("")));
    let present: Vec<&str> = KNOWN_CLASSES
        .iter()
        .filter(|c| rows.iter().any(|r| r["class_type"].as_str() == Some(*c)))
        .copied()
        .collect();
    let missing: Vec<&str> = KNOWN_CLASSES.iter().filter(|c| !present.contains(c)).copied().collect();
    // 缝合那一对是"蒙版外逐像素不动"的全部依赖，缺了它回来的就是一张重绘过的整图
    let pair = ["InpaintCropImproved", "InpaintStitchImproved"];
    let stitch_pair_ok = pair.iter().all(|c| present.contains(c));
    Ok(serde_json::json!({
        "format": fmt,
        "total": rows.len(),
        "nodes": rows,
        "known_present": present,
        "known_missing": missing,
        "stitch_pair_ok": stitch_pair_ok,
    }))
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
        match load_map(Path::new(&p)) {
            Ok((mut wf, fmt)) => {
                // 裁切控件错位不会抛错，只会静默出坏图：litegraph 那支只有位置没有名字，
                // 形状对不上就当这一项没读到（API 那支按名取，不需要这条兜底）
                if fmt == "litegraph" {
                    let crop = wf.get("crop_widgets").cloned().unwrap_or(Value::Null);
                    if !crop.is_null() && !crop_shape_ok(&crop, &dget("crop_widgets")) {
                        error = Some(format!(
                            "工作流里 InpaintCropImproved 的控件与内置基线对不上（内置 {} 项，工作流 {} 项），裁切参数改用内置值",
                            dget("crop_widgets").as_array().map(|a| a.len()).unwrap_or(0),
                            crop.as_array().map(|a| a.len()).unwrap_or(0)
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
                    error = Some(format!(
                        "认出了{}格式，但文件里没有本管线认识的节点参数（UNETLoader / KSampler / TextEncodeQwenImage21 这些一个都没找到）",
                        if fmt == "api" { "API 导出" } else { "UI 导出" }
                    ));
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_json(name: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("synco-wf-{name}-{}.json", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    /// UI/litegraph 导出：只有 widgets_values 的位置，没有名字
    const LITE: &str = r#"{"nodes":[
      {"id":3,"type":"UNETLoader","mode":0,"widgets_values":["qwen_bf16.safetensors","default"]},
      {"id":4,"type":"LoraLoaderModelOnly","mode":0,"widgets_values":["loraA.safetensors",0.8],"inputs":[{"type":"MODEL","link":11}]},
      {"id":5,"type":"KSampler","mode":0,"widgets_values":["123","randomize",20,3.5,"euler","normal",true]},
      {"id":6,"type":"TextEncodeQwenImage21","mode":0,"widgets_values":["默认正向","默认负面"]},
      {"id":8,"type":"CLIPLoader","mode":0,"widgets_values":["qwen3vl.safetensors","qwen"]},
      {"id":9,"type":"VAELoader","mode":0,"widgets_values":["qwen_vae.safetensors"]}
    ],"links":[[11,3,0,4,1,"MODEL"]]}"#;

    /// API 导出：inputs 按名字给值，`["节点号", 引脚]` 是连接
    const API: &str = r#"{
      "1":{"class_type":"LoadImage","inputs":{"image":"a.png"}},
      "3":{"class_type":"UNETLoader","inputs":{"unet_name":"qwen_bf16.safetensors","weight_dtype":"default"}},
      "4":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["3",0],"lora_name":"loraA.safetensors","strength_model":0.7}},
      "7":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["4",0],"lora_name":"loraB.safetensors","strength_model":0.4}},
      "8":{"class_type":"CLIPLoader","inputs":{"clip_name":"qwen3vl.safetensors","type":"qwen"}},
      "9":{"class_type":"VAELoader","inputs":{"vae_name":"qwen_vae.safetensors"}},
      "11":{"class_type":"TextEncodeQwenImage21","inputs":{"prompt":"写成 API 形态","negative_prompt":"不要的东西"}},
      "14":{"class_type":"KSampler","inputs":{"model":["7",0],"seed":1,"steps":28,"cfg":4.5,"sampler_name":"dpmpp_2m","scheduler":"karras","denoise":1}},
      "10":{"class_type":"InpaintCropImproved","inputs":{"image":["1",0],"mask":["2",0],"mask_expand_pixels":64,"device_mode":"auto"}}
    }"#;

    #[test]
    fn ui_导出按位置读得出关键参数() {
        let p = tmp_json("lite", LITE);
        let (m, fmt) = load_map(&p).unwrap();
        assert_eq!(fmt, "litegraph");
        assert_eq!(m["unet"], Value::String("qwen_bf16.safetensors".into()));
        assert_eq!(m["steps"], Value::from(20));
        assert_eq!(m["cfg"], Value::from(3.5));
        assert_eq!(m["sampler"], Value::String("euler".into()));
        assert_eq!(m["negative"], Value::String("默认负面".into()));
        let loras = m["loras"].as_array().unwrap();
        assert_eq!(loras.len(), 1);
        assert_eq!(loras[0]["name"], Value::String("loraA.safetensors".into()));
        std::fs::remove_file(p).ok();
    }

    /// API 导出的 `inputs` 是按名字的：节点顺序、控件个数都不再能让参数错位
    #[test]
    fn api_导出按名字读参数与裁切() {
        let p = tmp_json("api", API);
        let (m, fmt) = load_map(&p).unwrap();
        assert_eq!(fmt, "api");
        assert_eq!(m["steps"], Value::from(28));
        assert_eq!(m["cfg"], Value::from(4.5));
        assert_eq!(m["sampler"], Value::String("dpmpp_2m".into()));
        assert_eq!(m["scheduler"], Value::String("karras".into()));
        assert_eq!(m["clip"], Value::String("qwen3vl.safetensors".into()));
        assert_eq!(m["clip_type"], Value::String("qwen".into()));
        assert_eq!(m["negative"], Value::String("不要的东西".into()));
        // 链是 3 → 4 → 7，顺序不能反（先挂的在前）
        let loras = m["loras"].as_array().unwrap();
        assert_eq!(loras.iter().map(|l| l["name"].as_str().unwrap().to_string()).collect::<Vec<_>>(), vec!["loraA.safetensors", "loraB.safetensors"]);
        assert_eq!(loras[1]["strength"], Value::from(0.4));
        // 裁切按名给值，且连接引用（数组形态）不会被当成参数值
        let ci = m["crop_inputs"].as_object().unwrap();
        assert_eq!(ci["mask_expand_pixels"], Value::from(64));
        assert_eq!(ci["device_mode"], Value::String("auto".into()));
        assert!(!ci.contains_key("image"), "连接引用不该被当成分支参数");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn api_导出里停用的_lora_保持停用() {
        let body = API.replace("\"7\":{\"class_type\":\"LoraLoaderModelOnly\"", "\"7\":{\"enabled\":0,\"class_type\":\"LoraLoaderModelOnly\"");
        let p = tmp_json("off", &body);
        let (m, _) = load_map(&p).unwrap();
        let loras = m["loras"].as_array().unwrap();
        assert_eq!(loras[1]["enabled"], Value::Bool(false));
        assert_eq!(loras[0]["enabled"], Value::Bool(true));
        std::fs::remove_file(p).ok();
    }

    /// 合并/手改过的 json 里两个 LoRA 可以有同一个 id：旧写法在这里会无限 push
    #[test]
    fn 环形_lora_链被_visited_与深度上限夹住() {
        let lite = r#"{"nodes":[
          {"id":3,"type":"UNETLoader","mode":0,"widgets_values":["u.safetensors","default"]},
          {"id":7,"type":"LoraLoaderModelOnly","mode":0,"widgets_values":["A.safetensors",1],"inputs":[{"type":"MODEL","link":11}]},
          {"id":7,"type":"LoraLoaderModelOnly","mode":0,"widgets_values":["B.safetensors",1],"inputs":[{"type":"MODEL","link":12}]}
        ],"links":[[11,3,0,7,1,"MODEL"],[12,7,0,7,1,"MODEL"]]}"#;
        let p = tmp_json("ring", lite);
        let (m, _) = load_map(&p).unwrap();
        let n = m["loras"].as_array().unwrap().len();
        assert!(n <= LORA_MAX, "LoRA 链没被上限夹住：{n}");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn 认不出的格式明确失败而不是回空表() {
        for body in ["{}", r#"{"foo":{"bar":1}}"#, r#"{"nodes":{}}"#] {
            let p = tmp_json("junk", body);
            assert!(load_map(&p).is_err(), "{body} 应该被拒");
            std::fs::remove_file(p).ok();
        }
    }

    #[test]
    fn 清点数得出节点与缝合那一对() {
        let p = tmp_json("insp", API);
        let v = inspect(&p).unwrap();
        assert_eq!(v["format"], Value::String("api".into()));
        assert_eq!(v["total"], Value::from(9));
        // 这个样本有 InpaintCropImproved 但没有 InpaintStitchImproved —— 缺一个就不能算"能缝合"
        assert_eq!(v["stitch_pair_ok"], Value::Bool(false));
        assert!(v["known_missing"].as_array().unwrap().iter().any(|x| x == "InpaintStitchImproved"));
        assert!(v["nodes"].as_array().unwrap().iter().any(|n| n["class_type"] == "KSampler"));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn 坏文件不抛只回错误串() {
        let p = tmp_json("noperm", "not json at all");
        assert!(load_map(&p).unwrap_err().contains("合法 JSON"));
        assert!(inspect(&p).is_err());
        std::fs::remove_file(p).ok();
    }
}
