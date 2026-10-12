//! 工作流格式识别与参数解析；不读取文件或运行时设置。
use serde_json::{Map, Value};
pub const DEFAULTS_RAW: &str = include_str!("../../../defaults.json");

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

pub(super) fn dget(k: &str) -> Value {
    defaults().get(k).cloned().unwrap_or(Value::Null)
}

/// 内置基线：只带 buildGraph 用得上的键，sources/workflow_sample 不进这里
pub(super) fn builtin() -> Map<String, Value> {
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
pub(super) fn crop_shape_ok(wf: &Value, base: &Value) -> bool {
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
pub(super) fn read_litegraph(d: &Value, m: &mut Map<String, Value>) {
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
pub(super) fn read_api(nodes: &Map<String, Value>, m: &mut Map<String, Value>) {
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
pub(super) fn detect(d: &Value) -> Option<&'static str> {
    if d.get("nodes").and_then(|v| v.as_array()).is_some() {
        return Some("litegraph");
    }
    if api_nodes(d).is_some() {
        return Some("api");
    }
    None
}

/// 供 setup.rs 用：把 defaults.json 里的下载基准取出来
pub fn sources() -> Map<String, Value> {
    defaults().get("sources").and_then(|v| v.as_object()).cloned().unwrap_or_default()
}
