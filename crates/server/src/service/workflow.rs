//! 工作流参数：优先读本机 Qwen 高分局部编辑工作流，读不到就退回随项目走的 defaults.json。
//! 没装 ComfyUI / 工作流路径不对都不能让服务起不来，所以这里全程不抛异常。

use crate::state::Ctx;
use crate::error::AppError;
use serde_json::{json, Map, Value};
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
fn read_api(nodes: &Map<String, Value>, m: &mut Map<String, Value>) {
    let class_of = |want: &str| -> Option<&Value> {
        nodes.values().filter(|n| n["class_type"].as_str() == Some(want)).next()
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
    let by_id = nodes;
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

/// API 导出的节点表。既认 ComfyUI「Save (API Format)」写出的裸 `{节点号:{class_type,inputs}}`，
/// 也认有人直接把 `/prompt` 请求体存下来的 `{"prompt":{…}}` 形态。
pub fn api_nodes(d: &Value) -> Option<&Map<String, Value>> {
    fn table(o: &Map<String, Value>) -> Option<&Map<String, Value>> {
        (!o.is_empty() && o.values().any(|v| v.get("class_type").is_some())).then_some(o)
    }
    let o = d.as_object()?;
    table(o).or_else(|| o.get("prompt").and_then(|p| p.as_object()).and_then(table))
}

/// 认格式：UI/litegraph 导出有 `nodes` 数组；API 导出是一张 `{节点号: {class_type, inputs}}` 的表
fn detect(d: &Value) -> Option<&'static str> {
    if d.get("nodes").and_then(|v| v.as_array()).is_some() {
        return Some("litegraph");
    }
    if api_nodes(d).is_some() {
        return Some("api");
    }
    None
}

// 界面之外没人拼句子：一条理由 = `{code, args}`，前端逐条按当前语言查字典。
// 清单项、角色名都是这个形状，所以「加载图找不到节点」里的角色名也跟着语言走。
fn why(code: &str, args: Value) -> Value {
    let mut o = Map::new();
    o.insert("code".into(), Value::String(code.to_string()));
    if let Value::Object(m) = args {
        if !m.is_empty() {
            o.insert("args".into(), Value::Object(m));
        }
    }
    Value::Object(o)
}

/// 已经是一条带钥匙的错误了就直接用它自己的钥匙，别再包一句
fn why_err(e: &AppError) -> Value {
    let (code, args) = e.reason();
    let mut o = Map::new();
    o.insert("code".into(), Value::String(code));
    if let Some(a) = args {
        o.insert("args".into(), Value::Object(a));
    }
    Value::Object(o)
}

/// 角色名：`label` 存的现在就是钥匙（UNet/CLIP/VAE 这些没进字典，查不到就原样显示）
fn role_ref(key: &str) -> Value {
    why(role(key).map(|r| r.label).unwrap_or(key), Value::Null)
}

/// 读并解析一个工作流文件，返回（参数表，格式）。
/// 认不出格式时给的是**明确失败**而不是空表——以前 API 导出会解析成"零项"，
/// 而 cfgSource 照样标成 "workflow"，等于谎称参数读自你的文件。
pub fn load_map(p: &Path) -> crate::error::Result<(Map<String, Value>, &'static str)> {
    let text = std::fs::read_to_string(p).map_err(|e| AppError::detail("srv.wf.readFail", e))?;
    let d: Value = serde_json::from_str(&text).map_err(|e| AppError::detail("srv.wf.badJson", e))?;
    let fmt = detect(&d).ok_or_else(|| AppError::bad("srv.wf.unknownFormat"))?;
    let mut m = Map::new();
    match api_nodes(&d) {
        Some(nodes) if fmt == "api" => read_api(nodes, &mut m),
        _ => read_litegraph(&d, &mut m),
    }
    Ok((m, fmt))
}

/// 本管线会用的节点类名。清点端点按这张表报"在不在位"，角色表就从这里长出来。
pub const KNOWN_CLASSES: [&str; 14] = [
    "LoadImage", "LoadImageMask", "UNETLoader", "CLIPLoader", "VAELoader",
    "TextEncodeQwenImage21", "KSampler", "InpaintCropImproved", "InpaintStitchImproved",
    "LoraLoaderModelOnly", "SaveImage", "PreviewImage", "PrimitiveStringMultiline", "DrawMaskOnImage",
];

// ---------------------------------------------------------------- 角色映射（M7 第二步）

pub struct Role {
    pub key: &'static str,
    pub class: &'static str,
    pub label: &'static str,
    /// 缺了就**不能**用这张图提交。D14：认不出就硬失败，不回退到内置图——
    /// 回退等于把"参数读自你的文件"那句谎从参数层搬到结构层。
    pub required: bool,
}

/// Synco 要往工作流里填的东西全按角色寻址，不认节点号：用户挪节点、改编号都不影响。
pub const ROLES: [Role; 13] = [
    Role { key: "load_image", class: "LoadImage", label: "wf.role.loadImage", required: true },
    Role { key: "load_mask", class: "LoadImageMask", label: "wf.role.loadMask", required: true },
    Role { key: "unet", class: "UNETLoader", label: "UNet", required: true },
    Role { key: "clip", class: "CLIPLoader", label: "CLIP", required: true },
    Role { key: "vae", class: "VAELoader", label: "VAE", required: true },
    Role { key: "text_encode", class: "TextEncodeQwenImage21", label: "wf.role.textEncode", required: true },
    Role { key: "ksampler", class: "KSampler", label: "wf.role.ksampler", required: true },
    Role { key: "crop", class: "InpaintCropImproved", label: "wf.role.crop", required: true },
    Role { key: "stitch", class: "InpaintStitchImproved", label: "wf.role.stitch", required: true },
    Role { key: "out_final", class: "SaveImage", label: "wf.role.outFinal", required: true },
    Role { key: "out_crop", class: "PreviewImage", label: "wf.role.outCrop", required: false },
    Role { key: "mask_viz", class: "DrawMaskOnImage", label: "wf.role.maskViz", required: false },
    Role { key: "out_overlay", class: "PreviewImage", label: "wf.role.outOverlay", required: false },
];

pub fn role(key: &str) -> Option<&'static Role> {
    ROLES.iter().find(|r| r.key == key)
}

/// 读一个 API 导出的工作流，返回 `{节点号: {class_type, inputs}}`。不是 API 导出时给 None。
pub fn load_api_graph(p: &Path) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(p).ok()?;
    let d: Value = serde_json::from_str(&text).ok()?;
    api_nodes(&d).cloned()
}

/// 要注入的那几样东西分别从哪个输入名进去。写在这里而不是散在 comfy.rs 里，
/// 是为了让"角色 → 输入名"这张对照只有一处：名字对不上就提交不了，而不是填进不该填的地方。
pub const INJECT_KEYS: [(&str, &str); 6] = [
    ("load_image", "image"),
    ("load_mask", "image"),
    ("text_encode", "prompt"),
    ("ksampler", "seed"),
    ("ksampler", "steps"),
    ("ksampler", "cfg"),
];

/// 输入值是 `["节点号", 引脚]` 时给出上游节点号
fn upstream(v: &Value) -> Option<&str> {
    v.as_array().and_then(|a| a.first()).and_then(|x| x.as_str())
}

/// `a` 的输出（直接或间接）是否流到 `b` 的输入。带 visited：环与菱形都不会转死。
/// 从 `b` 的上游开始走，所以 `feeds(x, x)` 只有在 x 真的吃自己的输出时才为真——
/// 把起点算进去会让"两个角色指到同一个节点"这种畸形映射看起来合法。
pub fn feeds(g: &Map<String, Value>, from: &str, to: &str) -> bool {
    let ups = |id: &str| -> Vec<String> {
        g.get(id)
            .and_then(|n| n.get("inputs"))
            .and_then(|v| v.as_object())
            .map(|o| o.values().filter_map(upstream).filter(|u| *u != id).map(str::to_string).collect())
            .unwrap_or_default()
    };
    let mut seen = std::collections::HashSet::new();
    seen.insert(to.to_string());
    let mut stack = ups(to);
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if id == from {
            return true;
        }
        stack.extend(ups(&id));
    }
    false
}

/// 整张图有没有环（自环也算）：有环的话 ComfyUI 自己会拒，但报错在对面，不如这里说清
pub fn has_cycle(g: &Map<String, Value>) -> bool {
    fn walk(g: &Map<String, Value>, id: &str, stack: &mut Vec<String>, done: &mut std::collections::HashSet<String>) -> bool {
        if stack.iter().any(|s| s == id) {
            return true;
        }
        if done.contains(id) {
            return false;
        }
        stack.push(id.to_string());
        if let Some(o) = g.get(id).and_then(|n| n.get("inputs")).and_then(|v| v.as_object()) {
            for v in o.values() {
                if let Some(u) = upstream(v) {
                    if walk(g, u, stack, done) {
                        return true;
                    }
                }
            }
        }
        stack.pop();
        done.insert(id.to_string());
        false
    }
    let mut stack = Vec::new();
    let mut done = std::collections::HashSet::new();
    g.keys().any(|id| walk(g, id, &mut stack, &mut done))
}

/// 按类名自动预填；输出类角色按"数据从哪来"判（同一个图里可能有好几个 SaveImage）。
/// 认不出来的角色留空，交给设置里的手指覆盖。
pub fn auto_roles(g: &Map<String, Value>) -> Map<String, Value> {
    let ids_of = |cls: &str| -> Vec<String> {
        g.iter().filter(|(_, n)| n["class_type"].as_str() == Some(cls)).map(|(k, _)| k.clone()).collect()
    };
    let mut out = Map::new();
    for (key, cls) in [
        ("load_image", "LoadImage"),
        ("load_mask", "LoadImageMask"),
        ("unet", "UNETLoader"),
        ("clip", "CLIPLoader"),
        ("vae", "VAELoader"),
        ("text_encode", "TextEncodeQwenImage21"),
        ("ksampler", "KSampler"),
        ("crop", "InpaintCropImproved"),
        ("stitch", "InpaintStitchImproved"),
        ("mask_viz", "DrawMaskOnImage"),
    ] {
        if let Some(id) = ids_of(cls).into_iter().next() {
            out.insert(key.into(), Value::String(id));
        }
    }
    // 三个输出角色挑的是"哪一路数据要读回来"：成图必须是缝合那一路，
    // 随便抓一个 SaveImage 存的可能直接是采样结果（那张图看着正常，但语义已经错了）。
    // 可选那两路只认"另有一路"：指到成图那个节点上就等于没指——那一路只会回来同一张图，
    // 界面上"裁切图/遮罩叠加"两个下载按钮点开的是成图。
    let pick = |src: Option<&str>, prefer: &[&str], skip: &[String]| -> Option<String> {
        let s = src?;
        for cls in prefer {
            if let Some(id) = ids_of(cls).into_iter().find(|id| feeds(g, s, id) && !skip.contains(id)) {
                return Some(id);
            }
        }
        None
    };
    let stitch = out.get("stitch").and_then(|v| v.as_str()).map(str::to_string);
    let crop = out.get("crop").and_then(|v| v.as_str()).map(str::to_string);
    let viz = out.get("mask_viz").and_then(|v| v.as_str()).map(str::to_string);
    let final_id = pick(stitch.as_deref(), &["SaveImage", "PreviewImage"], &[]);
    let taken = final_id.clone().into_iter().collect::<Vec<_>>();
    let crop_id = pick(crop.as_deref(), &["PreviewImage"], &taken);
    let mut taken2 = taken.clone();
    taken2.extend(crop_id.clone());
    let overlay = pick(viz.as_deref(), &["PreviewImage", "SaveImage"], &taken2);
    for (key, id) in [("out_final", final_id), ("out_crop", crop_id), ("out_overlay", overlay)] {
        if let Some(id) = id {
            out.insert(key.into(), Value::String(id));
        }
    }
    out
}

/// 生效映射 = 类名自动预填 ← 库里存的手指覆盖（空串与不认识的键都忽略）
pub fn merge_roles(g: &Map<String, Value>, saved: Option<&str>) -> Map<String, Value> {
    let mut roles = auto_roles(g);
    let parsed: Map<String, Value> = saved
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for r in ROLES.iter() {
        if let Some(node) = parsed.get(r.key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            roles.insert(r.key.into(), Value::String(node.to_string()));
        }
    }
    roles
}

/// 结构校验。返回问题清单；空 = 这张图可以接管提交。
///
/// 这一条是 M7 全部的赌注所在：缺了裁切-缝合那一对、或者成图不是从缝合出来的，
/// 回来的是一张**重绘过的整图**，画面看着完全正常，没人会发现语义已经变了。
pub fn validate(g: &Map<String, Value>, roles: &Map<String, Value>) -> Vec<Value> {
    let mut errs: Vec<Value> = Vec::new();
    let id = |k: &str| roles.get(k).and_then(|v| v.as_str()).map(str::to_string);
    for r in ROLES.iter().filter(|r| r.required) {
        let Some(nid) = id(r.key) else {
            errs.push(why("srv.wf.roleMissing", json!({ "role": role_ref(r.key), "class": r.class })));
            continue;
        };
        if !g.contains_key(&nid) {
            errs.push(why("srv.wf.roleNotInGraph", json!({ "role": role_ref(r.key), "id": nid })));
        }
    }
    if !errs.is_empty() {
        return errs;
    }
    // 注入全靠输入名，名字对不上就是"提交时照片送不进去"，比缺节点更隐蔽
    for (key, input) in INJECT_KEYS {
        if let Some(nid) = id(key) {
            let node = match g.get(&nid) {
                Some(n) => n,
                None => continue, // 上面已经报过
            };
            if !node.get("inputs").and_then(|v| v.as_object()).map(|o| o.contains_key(input)).unwrap_or(false) {
                errs.push(why("srv.wf.roleNoInput", json!({ "role": role_ref(key), "id": nid, "input": input })));
            }
        }
    }
    if has_cycle(g) {
        errs.push(why("srv.wf.cycle", Value::Null));
    }
    if let (Some(c), Some(s)) = (id("crop"), id("stitch")) {
        if !feeds(g, &c, &s) {
            errs.push(why("srv.wf.stitchOrder", Value::Null));
        }
    }
    if let (Some(s), Some(f)) = (id("stitch"), id("out_final")) {
        if !feeds(g, &s, &f) {
            errs.push(why("srv.wf.outOfStitch", Value::Null));
        }
    }
    if let (Some(li), Some(c)) = (id("load_image"), id("crop")) {
        if !feeds(g, &li, &c) {
            errs.push(why("srv.wf.cropNoPhoto", json!({ "role": role_ref("load_image") })));
        }
    }
    if let (Some(lm), Some(c)) = (id("load_mask"), id("crop")) {
        if !feeds(g, &lm, &c) {
            errs.push(why("srv.wf.cropNoMask", json!({ "role": role_ref("load_mask") })));
        }
    }
    if let (Some(te), Some(ks)) = (id("text_encode"), id("ksampler")) {
        if !feeds(g, &te, &ks) {
            errs.push(why("srv.wf.noPrompt", Value::Null));
        }
    }
    // 三个输出角色指向同一个节点：那一路只会回来一张图，另外两个下载按钮是骗人的
    for (a, b) in [("out_final", "out_crop"), ("out_final", "out_overlay"), ("out_crop", "out_overlay")] {
        if let (Some(x), Some(y)) = (id(a), id(b)) {
            if x == y {
                errs.push(why("srv.wf.sameNode", json!({ "a": role_ref(a), "b": role_ref(b), "id": x })));
            }
        }
    }
    errs
}


/// 角色映射按工作流文件各存一份（换文件不能沿用上一份的节点号）
pub fn roles_key(path: &str) -> String {
    format!("workflow_roles:{path}")
}

pub fn saved_roles(ctx: &Ctx, path: &str) -> Option<String> {
    crate::repo::settings::get(ctx, &roles_key(path))
}

/// 手指覆盖先按类名校验再落库：存进去一个错类名的节点，下一次提交就会往不该填的输入里写。
/// 返回（存下的映射，被拒的项）
pub fn set_saved_roles(ctx: &Ctx, path: &str, roles: &Map<String, Value>) -> (Map<String, Value>, Vec<Value>) {
    let g = load_api_graph(Path::new(path));
    let mut kept = Map::new();
    let mut rejected = Vec::new();
    for r in ROLES.iter() {
        let Some(v) = roles.get(r.key) else { continue };
        let Some(node) = v.as_str().map(str::trim) else {
            rejected.push(why("srv.wf.roleValue", json!({ "role": role_ref(r.key) })));
            continue;
        };
        if node.is_empty() {
            continue; // 清空 = 交回自动预填
        }
        match g.as_ref().and_then(|g| g.get(node)).and_then(|n| n["class_type"].as_str()) {
            Some(cls) if cls == r.class => {
                kept.insert(r.key.into(), Value::String(node.to_string()));
            }
            Some(cls) => rejected.push(why("srv.wf.roleClass", json!({ "role": role_ref(r.key), "class": r.class, "id": node, "found": cls }))),
            None => rejected.push(why("srv.wf.roleNotInGraph", json!({ "role": role_ref(r.key), "id": node }))),
        }
    }
    let key = roles_key(path);
    if kept.is_empty() {
        let _ = crate::repo::settings::del(ctx, &key);
    } else if let Ok(text) = serde_json::to_string(&Value::Object(kept.clone())) {
        let _ = crate::repo::settings::put(ctx, &key, &text);
    }
    (kept, rejected)
}

/// 一张文件 + 一份手指 → 该不该用它提交。`graph` 是 None 表示这文件根本当不了计算图
/// （读不到、或不是 API 导出），此时只能走内置图，且原因要显示出来——不做静默替换。
struct Assessment {
    graph: Option<Map<String, Value>>,
    roles: Map<String, Value>,
    errors: Vec<Value>,
    /// 当不了计算图时那句原话：钥匙给界面查，参数随钥匙走
    reason: Value,
}

fn assess(path: &str, saved: Option<&str>) -> Assessment {
    let graph = load_api_graph(Path::new(path));
    match graph {
        Some(g) => {
            let roles = merge_roles(&g, saved);
            Assessment { errors: validate(&g, &roles), graph: Some(g), roles, reason: Value::Null }
        }
        None => Assessment {
            graph: None,
            roles: Map::new(),
            errors: Vec::new(),
            reason: if Path::new(path).exists() {
                why("srv.wf.uiExport", Value::Null)
            } else {
                why("srv.wf.fileGone", json!({ "path": path.to_string() }))
            },
        },
    }
}

/// 提交用哪张计算图。**不回退**是设计的一部分（D14）：
/// 角色认不出却改用内置图，等于把"参数读自你的文件"那句谎从参数层搬到结构层。
#[derive(Debug)]
pub enum Plan {
    /// 用你文件里这张图提交（已按角色校验通过）
    Workflow { graph: Map<String, Value>, roles: Map<String, Value> },
    /// 文件不是 API 导出（或读不到）：用内置图，并把原因显示出来
    Builtin { reason: Value },
    /// 是你的图，但角色/结构对不上：拒绝提交
    Refused { errors: Vec<Value> },
}

pub fn plan_at(path: &str, saved: Option<&str>) -> Plan {
    match assess(path, saved) {
        Assessment { graph: Some(g), roles, errors, .. } if errors.is_empty() => Plan::Workflow { graph: g, roles },
        Assessment { errors, .. } if !errors.is_empty() => Plan::Refused { errors },
        Assessment { reason, .. } => Plan::Builtin { reason },
    }
}

pub fn plan(ctx: &Ctx) -> Plan {
    let p = workflow_path(ctx);
    plan_at(&p, saved_roles(ctx, &p).as_deref())
}

/// 设置里那张角色表：节点清单 + 自动预填 + 存过的手指 + 生效值 + 校验结论
pub fn roles_view(ctx: &Ctx) -> crate::error::Result<Value> {
    let p = workflow_path(ctx);
    let saved_raw = saved_roles(ctx, &p);
    let a = assess(&p, saved_raw.as_deref());
    let mut nodes: Vec<Value> = Vec::new();
    if let Some(g) = &a.graph {
        for (id, n) in g {
            nodes.push(serde_json::json!({
                "id": id,
                "class_type": n["class_type"].as_str().unwrap_or(""),
                "title": n["_meta"]["title"].as_str().unwrap_or(""),
            }));
        }
        nodes.sort_by(|x, y| (x["class_type"].as_str().unwrap_or("").to_string(), x["id"].as_str().unwrap_or("").to_string()).cmp(&(
            y["class_type"].as_str().unwrap_or("").to_string(),
            y["id"].as_str().unwrap_or("").to_string(),
        )));
    }
    let saved_map: Map<String, Value> = saved_raw
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let can_takeover = a.graph.is_some() && a.errors.is_empty();
    Ok(serde_json::json!({
        "workflow_path": p,
        "is_api": a.graph.is_some(),
        "nodes": nodes,
        "roles": ROLES.iter().map(|r| serde_json::json!({"key": r.key, "class": r.class, "label": r.label, "required": r.required})).collect::<Vec<_>>(),
        "auto": Value::Object(a.graph.as_ref().map(auto_roles).unwrap_or_default()),
        "saved": Value::Object(saved_map),
        "effective": Value::Object(a.roles),
        "errors": a.errors,
        "reason": a.reason.get("code").cloned().unwrap_or(Value::Null),
        "reason_args": a.reason.get("args").cloned().unwrap_or(Value::Null),
        "can_takeover": can_takeover,
    }))
}

/// 把文件里的节点摊平成可比对的 `(节点号, 类名, 输入名)` 列表，两种格式同一套出口
pub fn inspect(p: &Path) -> crate::error::Result<Value> {
    let text = std::fs::read_to_string(p).map_err(|e| AppError::detail("srv.wf.readFail", e))?;
    let d: Value = serde_json::from_str(&text).map_err(|e| AppError::detail("srv.wf.badJson", e))?;
    let fmt = detect(&d).ok_or_else(|| AppError::bad("srv.wf.unknownFormatShort"))?;
    let mut rows: Vec<Value> = Vec::new();
    if fmt == "api" {
        if let Some(o) = api_nodes(&d) {
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

/// 供 setup.rs 用：把 defaults.json 里的下载基准取出来
pub fn sources() -> Map<String, Value> {
    defaults().get("sources").and_then(|v| v.as_object()).cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 清单比对钥匙而不是句子：措辞改了不该让单测红
    fn codes(list: &[Value]) -> Vec<String> {
        list.iter().map(|e| e["code"].as_str().unwrap_or("").to_string()).collect()
    }

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
        assert!(load_map(&p).unwrap_err().text().contains("合法 JSON"));
        assert!(inspect(&p).is_err());
        std::fs::remove_file(p).ok();
    }

    /// 一张能接管提交的最小图：形状照 ComfyUI 的 API 导出，节点号刻意不是内置图那套
    const WF: &str = r#"{
      "1":{"class_type":"LoadImage","inputs":{"image":"占位.png"}},
      "2":{"class_type":"LoadImageMask","inputs":{"image":"占位m.png","channel":"red"}},
      "3":{"class_type":"UNETLoader","inputs":{"unet_name":"u.safetensors"}},
      "4":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["3",0],"lora_name":"A.safetensors","strength_model":0.9}},
      "5":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["4",0],"lora_name":"B.safetensors","strength_model":0.5}},
      "6":{"class_type":"CLIPLoader","inputs":{"clip_name":"c.safetensors"}},
      "7":{"class_type":"VAELoader","inputs":{"vae_name":"v.safetensors"}},
      "10":{"class_type":"InpaintCropImproved","inputs":{"image":["1",0],"mask":["2",0],"mask_expand_pixels":64}},
      "11":{"class_type":"TextEncodeQwenImage21","inputs":{"clip":["6",0],"vae":["7",0],"prompt":"文件里的正向","negative_prompt":"文件里的负面","images.image_1":["10",1]}},
      "12":{"class_type":"VAEEncode","inputs":{"pixels":["10",1],"vae":["7",0]}},
      "13":{"class_type":"SetLatentNoiseMask","inputs":{"samples":["12",0],"mask":["10",2]}},
      "14":{"class_type":"KSampler","inputs":{"model":["5",0],"positive":["11",0],"negative":["11",1],"latent_image":["13",0],"seed":7,"steps":18,"cfg":2.5,"sampler_name":"euler","scheduler":"simple","denoise":1}},
      "15":{"class_type":"VAEDecode","inputs":{"samples":["14",0],"vae":["7",0]}},
      "16":{"class_type":"InpaintStitchImproved","inputs":{"stitcher":["10",0],"inpainted_image":["15",0]}},
      "17":{"class_type":"SaveImage","inputs":{"images":["16",0],"filename_prefix":"Mine"}},
      "18":{"class_type":"DrawMaskOnImage","inputs":{"image":["10",1],"mask":["10",2]}},
      "19":{"class_type":"PreviewImage","inputs":{"images":["10",1]}},
      "20":{"class_type":"PreviewImage","inputs":{"images":["18",0]}},
      "99":{"class_type":"SaveImage","inputs":{"images":["14",0],"filename_prefix":"别的图"}}
    }"#;

    fn wf() -> Map<String, Value> {
        serde_json::from_str::<Value>(WF).unwrap().as_object().cloned().unwrap()
    }

    /// `{"prompt":{…}}` 这种把 /prompt 请求体存下来的形态也认
    #[test]
    fn prompt_包裹形态也当_api_导出() {
        let p = tmp_json("wrapped", &format!(r#"{{"prompt":{WF}}}"#));
        let g = load_api_graph(&p).expect("应该认出被 prompt 包起来的节点表");
        assert_eq!(g["17"]["class_type"], Value::String("SaveImage".into()));
        let (m, fmt) = load_map(&p).unwrap();
        assert_eq!(fmt, "api");
        assert_eq!(m["steps"], Value::from(18));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn 角色按类名自动认出_输出走缝合那一路() {
        let g = wf();
        let r = auto_roles(&g);
        assert_eq!(r["load_image"], Value::String("1".into()));
        assert_eq!(r["crop"], Value::String("10".into()));
        assert_eq!(r["stitch"], Value::String("16".into()));
        // 99 号那个 SaveImage 存的是采样结果，不是缝合结果——成图角色不能挑它
        assert_eq!(r["out_final"], Value::String("17".into()));
        assert_eq!(r["out_crop"], Value::String("19".into()));
        assert_eq!(r["out_overlay"], Value::String("20".into()));
        assert!(validate(&g, &r).is_empty(), "{:?}", validate(&g, &r));
    }

    /// 缝合没接在裁切后面 = 回来的图不会被贴回原图；成图直接存采样结果 = 整图重绘。
    /// 两种都不会报错、画面也都正常，语义却已经变了，所以必须在提交前拦住。
    /// 图里没有预览节点时，可选角色必须留空。把"输出裁切区"顺手指到成图那个 SaveImage 上，
    /// 读回来的就是一张与成图重复的文件——下载按钮写着"裁切图"，点开是成图。
    #[test]
    fn 没有预览节点时可选角色留空() {
        let mut g = wf();
        for id in ["18", "19", "20"] {
            g.remove(id);
        }
        let r = auto_roles(&g);
        assert_eq!(r["out_final"], Value::String("17".into()));
        assert!(!r.contains_key("out_crop"), "裁切区没节点可认就该空着");
        assert!(!r.contains_key("out_overlay"));
        assert!(validate(&g, &r).is_empty(), "{:?}", validate(&g, &r));

        // 手工把两路输出指到同一个节点也要拦
        let dup = merge_roles(&g, Some(r#"{"out_crop":"17"}"#));
        let errs = validate(&g, &dup);
        assert!(codes(&errs).contains(&"srv.wf.sameNode".into()), "{errs:?}");
    }

    #[test]
    fn 接错线的图被拒绝接管() {
        let mut g = wf();
        // 裁切经提示词编码喂到采样器，所以"接错线"要接一个与这条链无关的源才测得出来
        g.insert("77".into(), serde_json::json!({"class_type":"LoadImage","inputs":{"image":"另一张.png"}}));
        g.insert("16".into(), serde_json::json!({"class_type":"InpaintStitchImproved","inputs":{"stitcher":["77",0],"inpainted_image":["77",1]}}));
        let errs = validate(&g, &auto_roles(&g));
        assert!(codes(&errs).contains(&"srv.wf.stitchOrder".into()), "{errs:?}");

        // 99 号那个 SaveImage 存的是没缝回去的采样结果：手指到它头上就得拦住
        let g2 = wf();
        let roles = merge_roles(&g2, Some(r#"{"out_final":"99"}"#));
        let errs2 = validate(&g2, &roles);
        assert!(codes(&errs2).contains(&"srv.wf.outOfStitch".into()), "{errs2:?}");
    }

    #[test]
    fn 缺缝合节点就硬失败而不是回退() {
        let mut g = wf();
        g.remove("16");
        let p = tmp_json("nostitch", &Value::Object(g).to_string());
        match plan_at(&p.to_string_lossy(), None) {
            Plan::Refused { errors } => {
                assert!(errors.iter().any(|e| e["code"] == "srv.wf.roleMissing" && e["args"]["role"]["code"] == "wf.role.stitch"), "{errors:?}")
            }
            other => panic!("缺必需角色应该拒绝，拿到的是 {other:?}"),
        }
        std::fs::remove_file(p).ok();
    }

    /// UI 导出的文件当不了计算图：走内置图，但要把原因带出去（不做静默替换）
    #[test]
    fn ui_导出走内置图并把原因带出来() {
        let p = tmp_json("uilite", LITE);
        match plan_at(&p.to_string_lossy(), None) {
            Plan::Builtin { reason } => assert_eq!(reason["code"], "srv.wf.uiExport", "{reason}"),
            other => panic!("UI 导出应该是 Builtin，拿到 {other:?}"),
        }
        std::fs::remove_file(p).ok();
    }

    /// 手指过的节点号只在同一种类里收；指错类名的必须当场拒，不然提交时往不该填的输入里写值
    #[test]
    fn 角色映射的手指覆盖按类名收() {
        let mut g = wf();
        // 把成图角色改成手动指到 99 号（那是"直接存采样结果"那一路）：类名对得上，但结构校验要拦
        let roles = merge_roles(&g, Some(r#"{"out_final":"99"}"#));
        assert_eq!(roles["out_final"], Value::String("99".into()));
        let errs = validate(&g, &roles);
        assert!(codes(&errs).contains(&"srv.wf.outOfStitch".into()), "{errs:?}");
        // 指到不存在的节点：merge_roles 照收（它是手指的入口），由 validate 判死
        let roles2 = merge_roles(&g, Some(r#"{"out_final":"777"}"#));
        let errs2 = validate(&g, &roles2);
        assert!(errs2.iter().any(|e| e["code"] == "srv.wf.roleNotInGraph" && e["args"]["id"] == "777"), "{errs2:?}");
        // 指错类名（把采样器指到 LoadImage 上）：注入用的输入名根本不存在
        let roles3 = merge_roles(&g, Some(r#"{"ksampler":"1"}"#));
        let errs3 = validate(&g, &roles3);
        assert!(errs3.iter().any(|e| e["code"] == "srv.wf.roleNoInput" && e["args"]["input"] == "seed"), "{errs3:?}");
        // 空串 = 交回自动认出；这里 17 已被摘掉，所以那个键干脆不存在
        g.remove("17");
        g.remove("99");
        let roles4 = merge_roles(&g, Some(r#"{"out_final":""}"#));
        assert!(!roles4.contains_key("out_final"), "清空后不该留这个键");
    }

    #[test]
    fn 有环的图在提交前就说清() {
        let mut g = wf();
        g.insert("14".into(), serde_json::json!({"class_type":"KSampler","inputs":{"model":["14",0],"positive":["11",0],"negative":["11",1],"latent_image":["13",0],"seed":7,"steps":18,"cfg":2.5}}));
        assert!(has_cycle(&g));
        let errs = validate(&g, &auto_roles(&g));
        assert!(codes(&errs).contains(&"srv.wf.cycle".into()), "{errs:?}");
    }

    #[test]
    fn feeds_认间接连线也不转死在菱形里() {
        let g = wf();
        assert!(feeds(&g, "1", "10"), "直连");
        assert!(feeds(&g, "3", "14"), "要穿两条 LoRA");
        assert!(!feeds(&g, "17", "14"), "下游不该反过来喂上游");
        assert!(!feeds(&g, "1", "1"), "自己不算喂给自己");
    }
}
