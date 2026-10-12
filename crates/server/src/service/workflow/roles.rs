//! 角色定义、图遍历与结构校验；仅依赖输入图。
use serde_json::{json, Map, Value};
use super::{role_ref, why};
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
