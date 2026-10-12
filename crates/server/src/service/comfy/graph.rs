//! 内置图构造、角色注入和 LoRA 接线；不执行网络请求。
use crate::error::AppError;
use crate::service::workflow::CROP_KEYS;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
/// 高分局部编辑管线（LoRA 链按启用状态动态串接，其余输出节点固定）
pub fn build_graph(photo: &str, mask: &str, settings: &Value, seed: i64, cfg: &Value) -> Value {
    let mut ci = Map::new();
    // 裁切参数只收 CROP_KEYS 白名单里的名字：API 导出的工作流按名给值（挪节点、加控件都不会错位），
    // UI 导出只有位置，仍按 CROP_KEYS 的顺序 zip
    match cfg.get("crop_inputs").and_then(|v| v.as_object()) {
        Some(named) => {
            for k in CROP_KEYS {
                if let Some(v) = named.get(k) {
                    ci.insert((*k).to_string(), v.clone());
                }
            }
        }
        None => {
            let widgets = cfg.get("crop_widgets").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            for (k, v) in CROP_KEYS.iter().zip(widgets.iter()) {
                ci.insert((*k).to_string(), v.clone());
            }
        }
    }
    ci.insert("output_resize_to_target_size".into(), Value::Bool(true));
    ci.insert("output_target_width".into(), Value::from(1024));
    ci.insert("output_target_height".into(), Value::from(1024));
    ci.insert("image".into(), serde_json::json!(["1", 0]));
    ci.insert("mask".into(), serde_json::json!(["2", 0]));

    let mut g = Map::new();
    g.insert("1".into(), serde_json::json!({"class_type":"LoadImage","inputs":{"image":photo}}));
    g.insert("2".into(), serde_json::json!({"class_type":"LoadImageMask","inputs":{"image":mask,"channel":"red"}}));
    g.insert(
        "3".into(),
        serde_json::json!({"class_type":"UNETLoader","inputs":{"unet_name":cfg["unet"],"weight_dtype":"default"}}),
    );

    let mut cur = serde_json::json!(["3", 0]);
    let mut idx = 0;
    for l in settings
        .get("loras")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|l| l.get("enabled").and_then(Value::as_bool).unwrap_or(false))
    {
        let id = format!("L{idx}");
        g.insert(
            id.clone(),
            serde_json::json!({"class_type":"LoraLoaderModelOnly","inputs":{
                "model": cur, "lora_name": l.get("name").cloned().unwrap_or(Value::String(String::new())),
                "strength_model": l.get("strength").cloned().unwrap_or(Value::from(0))
            }}),
        );
        cur = serde_json::json!([id, 0]);
        idx += 1;
    }
    g.insert(
        "5".into(),
        serde_json::json!({"class_type":"QwenImage21Cache","inputs":{"model":cur,"device":"auto","dtype":"default"}}),
    );
    g.insert(
        "6".into(),
        serde_json::json!({"class_type":"CLIPLoader","inputs":{"clip_name":cfg["clip"],"type":cfg["clip_type"],"device":"default"}}),
    );
    g.insert("7".into(), serde_json::json!({"class_type":"VAELoader","inputs":{"vae_name":cfg["vae"]}}));
    g.insert("10".into(), serde_json::json!({"class_type":"InpaintCropImproved","inputs":ci}));
    g.insert(
        "11".into(),
        serde_json::json!({"class_type":"TextEncodeQwenImage21","inputs":{
            "clip":["6",0],"vae":["7",0],
            "prompt": settings.get("prompt").cloned().unwrap_or(Value::String(String::new())),
            "negative_prompt": match settings.get("negative") {
                Some(v) if !v.is_null() && v.as_str().map(|s| !s.is_empty()).unwrap_or(true) => v.clone(),
                _ => cfg.get("negative").cloned().unwrap_or(Value::String(String::new())),
            },
            "resolution": 0, "images.image_1": ["10", 1]
        }}),
    );
    g.insert("12".into(), serde_json::json!({"class_type":"VAEEncode","inputs":{"pixels":["10",1],"vae":["7",0]}}));
    g.insert("13".into(), serde_json::json!({"class_type":"SetLatentNoiseMask","inputs":{"samples":["12",0],"mask":["10",2]}}));
    g.insert(
        "14".into(),
        serde_json::json!({"class_type":"KSampler","inputs":{
            "model":["5",0],"positive":["11",0],"negative":["11",1],"latent_image":["13",0],
            "seed": seed,
            "steps": settings.get("steps").cloned().unwrap_or(Value::from(0)),
            "cfg": settings.get("cfg").cloned().unwrap_or(Value::from(0)),
            "sampler_name": cfg["sampler"], "scheduler": cfg["scheduler"], "denoise": 1
        }}),
    );
    g.insert("15".into(), serde_json::json!({"class_type":"VAEDecode","inputs":{"samples":["14",0],"vae":["7",0]}}));
    g.insert(
        "16".into(),
        serde_json::json!({"class_type":"InpaintStitchImproved","inputs":{"stitcher":["10",0],"inpainted_image":["15",0]}}),
    );
    g.insert("17".into(), serde_json::json!({"class_type":"SaveImage","inputs":{"images":["16",0],"filename_prefix":"COSDemo"}}));
    g.insert(
        "18".into(),
        serde_json::json!({"class_type":"DrawMaskOnImage","inputs":{"image":["10",1],"mask":["10",2],"color":"0, 0, 255","device":"cpu"}}),
    );
    g.insert("19".into(), serde_json::json!({"class_type":"PreviewImage","inputs":{"images":["10",1]}}));
    g.insert("20".into(), serde_json::json!({"class_type":"PreviewImage","inputs":{"images":["18",0]}}));
    Value::Object(g)
}

/// 往**你那张图**里填本次提交的变量。动到的只有这几处：照片、遮罩、提示词、
/// 种子/步数/CFG、LoRA 开关。裁切参数、模型名、连线关系一概按文件里那样跑——
/// 这正是接管的意义：你在 ComfyUI 里看到什么，提交出去就是什么。
pub fn inject_workflow(graph: &Map<String, Value>, roles: &Map<String, Value>, photo: &str, mask: &str, settings: &Value, seed: i64) -> crate::error::Result<Value> {
    let mut g = graph.clone();
    let mut set = |role: &str, key: &str, val: Value| -> crate::error::Result<()> {
        let id = roles.get(role).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| AppError::bad_args("srv.comfy.roleNoNode", json!({ "role": role.to_string() })))?;
        let node = g.get_mut(id).ok_or_else(|| {
            AppError::bad_args("srv.comfy.roleNotInGraph", json!({ "role": role.to_string(), "id": id }))
        })?;
        let inputs = node.as_object_mut().and_then(|n| n.get_mut("inputs")).and_then(|i| i.as_object_mut()).ok_or_else(|| AppError::bad_args("srv.comfy.noInputs", json!({ "id": id })))?;
        if !inputs.contains_key(key) {
            return Err(AppError::bad_args("srv.comfy.noInput", json!({ "id": id, "key": key })));
        }
        inputs.insert(key.to_string(), val);
        Ok(())
    };
    set("load_image", "image", Value::String(photo.to_string()))?;
    set("load_mask", "image", Value::String(mask.to_string()))?;
    set("text_encode", "prompt", settings.get("prompt").cloned().unwrap_or(Value::String(String::new())))?;
    // 负面词只在面板给了内容时才覆盖：文件里自己写好的那句不该被空串抹掉
    if settings.get("negative").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false) {
        let _ = set("text_encode", "negative_prompt", settings["negative"].clone());
    }
    set("ksampler", "seed", Value::from(seed))?;
    set("ksampler", "steps", Value::from(crate::util::num_or(settings.get("steps"), 0.0).round() as i64))?;
    set("ksampler", "cfg", Value::from(crate::util::num_or(settings.get("cfg"), 0.0)))?;
    apply_loras(&mut g, settings);
    Ok(Value::Object(g))
}

/// 面板上的 LoRA 勾选要真的生效。做法是把关掉的节点**摘掉、把下游接回它的上游**，
/// 而不是给它加一个 `enabled` 字段——那个键只有较新的 ComfyUI 认，老版本会照跑。
fn apply_loras(g: &mut Map<String, Value>, settings: &Value) {
    let want: Vec<(String, Value, bool)> = settings
        .get("loras")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|l| {
            let name = l.get("name").and_then(|v| v.as_str())?;
            Some((name.to_string(), l.get("strength").cloned().unwrap_or(Value::from(1.0)), l.get("enabled").and_then(Value::as_bool).unwrap_or(true)))
        })
        .collect();
    if want.is_empty() {
        return;
    }
    let mut off: HashSet<String> = HashSet::new();
    let mut on: Vec<(String, Value)> = Vec::new();
    for (id, n) in g.iter() {
        if n["class_type"].as_str() != Some("LoraLoaderModelOnly") {
            continue;
        }
        let name = n["inputs"]["lora_name"].as_str().unwrap_or("");
        // 面板里没有这条 LoRA（挂在 CLIP 上、或不在这条模型链上）就完全不碰它
        match want.iter().find(|(w, _, _)| w == name) {
            Some((_, _, false)) => {
                off.insert(id.clone());
            }
            Some((_, strength, true)) => on.push((id.clone(), strength.clone())),
            None => {}
        }
    }
    for (id, strength) in on {
        if let Some(inputs) = g.get_mut(&id).and_then(|n| n.as_object_mut()).and_then(|n| n.get_mut("inputs")).and_then(|i| i.as_object_mut()) {
            inputs.insert("strength_model".into(), strength);
        }
    }
    if off.is_empty() {
        return;
    }
    // 指向被摘节点的输入改指它的上游；上游也被摘就继续往上（次数上限挡住畸形文件里的环）
    let mut patches: Vec<(String, String, String, usize)> = Vec::new();
    for (id, n) in g.iter() {
        if off.contains(id) {
            continue;
        }
        if let Some(o) = n.get("inputs").and_then(|v| v.as_object()) {
            for (k, v) in o {
                let Some(t) = v.as_array().and_then(|a| a.first()).and_then(|x| x.as_str()) else { continue };
                if !off.contains(t) {
                    continue;
                }
                if let Some((repl, slot)) = survive_upstream(g, &off, t) {
                    patches.push((id.clone(), k.clone(), repl, slot));
                }
            }
        }
    }
    for (id, key, repl, slot) in patches {
        if let Some(o) = g.get_mut(&id).and_then(|n| n.as_object_mut()).and_then(|n| n.get_mut("inputs")).and_then(|i| i.as_object_mut()) {
            o.insert(key, Value::Array(vec![Value::String(repl), Value::from(slot as i64)]));
        }
    }
    for id in &off {
        g.remove(id);
    }
}

/// 被摘掉的 LoRA 实际在替谁供货：沿它自己的 `model` 输入往上，找到第一个还留在图里的节点
fn survive_upstream(g: &Map<String, Value>, off: &HashSet<String>, from: &str) -> Option<(String, usize)> {
    let mut cur = from.to_string();
    for _ in 0..=g.len() {
        let up = g.get(&cur).and_then(|n| n["inputs"]["model"].as_array().cloned())?;
        let id = up.first()?.as_str()?.to_string();
        let slot = up.get(1).and_then(|x| x.as_u64()).map(|v| v as usize).unwrap_or(0);
        if !off.contains(&id) {
            return Some((id, slot));
        }
        cur = id;
    }
    None
}
