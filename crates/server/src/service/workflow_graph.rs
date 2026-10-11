//! API 工作流导入、离线预览与字段映射。图按内容保存，原文件移动不影响配置。
use crate::{error::{AppError, Result}, models::workflow::Profile, repo::settings, state::Ctx};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};

const MAX_NODES: usize = 2000;
const MAX_BYTES: usize = 8 * 1024 * 1024;

pub fn parse(raw: &Value) -> Result<Map<String, Value>> {
    // 节点 ID 也可能叫 prompt；先认裸节点表，再认请求包裹。
    let bare = raw.as_object().filter(|g| !g.is_empty() && g.values().all(|n| n.get("class_type").is_some()));
    let g = bare.or_else(|| raw.get("prompt").and_then(Value::as_object)).filter(|g| !g.is_empty() && g.len() <= MAX_NODES)
        .ok_or_else(|| AppError::bad("srv.graph.format"))?;
    if g.values().any(|n| n.get("class_type").and_then(Value::as_str).filter(|s| !s.is_empty()).is_none()
        || n.get("inputs").and_then(Value::as_object).is_none()) {
        return Err(AppError::bad("srv.graph.format"));
    }
    Ok(g.clone())
}

// 只有二元组、非负整数端口和已存在节点同时成立才推断连线；不把一般数组误当连接。
pub fn link<'a>(g: &Map<String, Value>, v: &'a Value) -> Option<(&'a str, u64)> {
    let a = v.as_array()?;
    if a.len() != 2 { return None; }
    let id = a[0].as_str()?;
    let port = a[1].as_u64()?;
    g.contains_key(id).then_some((id, port))
}

fn read(path: &str) -> Result<Map<String, Value>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES { return Err(AppError::bad("srv.graph.size")); }
    parse(&serde_json::from_slice(&bytes).map_err(|e| AppError::detail("srv.graph.json", e))?)
}

pub fn import(ctx: &Ctx, raw: &Value) -> Result<Value> {
    let graph = parse(raw)?;
    let bytes = serde_json::to_vec(&Value::Object(graph)).map_err(|e| AppError::detail("srv.graph.json", e))?;
    if bytes.len() > MAX_BYTES { return Err(AppError::bad("srv.graph.size")); }
    let hash: String = Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect();
    let dir = ctx.data.join("workflows");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{hash}.json"));
    // 不接受用户文件名作路径，且已导入的同内容文件不会重写。
    if !file.exists() { std::fs::write(&file, bytes)?; }
    settings::put(ctx, "workflow_path", &file.to_string_lossy())?;
    *ctx.cfg.lock().unwrap() = None;
    view(ctx)
}

fn key(path: &str) -> String { format!("workflow_profile:{path}") }

pub fn profile(ctx: &Ctx, path: &str) -> Result<Option<Profile>> {
    settings::get(ctx, &key(path)).map(|s| serde_json::from_str(&s)
        .map_err(|e| AppError::detail("srv.graph.profile", e))).transpose()
}

fn same_type(a: &Value, b: &Value) -> bool {
    matches!((a, b), (Value::String(_), Value::String(_)) | (Value::Number(_), Value::Number(_)) | (Value::Bool(_), Value::Bool(_)))
}

pub fn validate(g: &Map<String, Value>, p: &Profile) -> Result<()> {
    parse(&Value::Object(g.clone()))?;
    let mut targets = std::collections::HashSet::new();
    for b in &p.bindings {
        let v = g.get(&b.node_id).and_then(|n| n["inputs"].get(&b.field))
            .ok_or_else(|| AppError::bad("srv.graph.binding"))?;
        if !targets.insert((&b.node_id, &b.field)) || link(g, v).is_some() { return Err(AppError::bad("srv.graph.binding")); }
        let expect = match b.role.as_str() {
            "source_image" | "source_image_mask" | "mask" | "prompt" | "negative" => Value::String(String::new()),
            "seed" | "steps" | "cfg" => Value::from(0),
            _ => return Err(AppError::bad("srv.graph.binding")),
        };
        if !same_type(v, &expect) { return Err(AppError::bad("srv.graph.binding")); }
        if matches!(b.role.as_str(), "source_image" | "source_image_mask" | "mask") {
            let cls = g[&b.node_id]["class_type"].as_str().unwrap_or_default();
            if b.field != "image" || !matches!(cls, "LoadImage" | "LoadImageMask") || (b.role == "source_image_mask" && cls != "LoadImage") { return Err(AppError::bad("srv.graph.binding")); }
        }
    }
    let out = g.get(&p.output_node).and_then(|n| n["class_type"].as_str());
    if !matches!(out, Some("SaveImage" | "PreviewImage")) { return Err(AppError::bad("srv.graph.output")); }
    // Kahn 排序避免递归处理大图时溢栈；预览可展示环，但运行映射不能带环。
    let mut indegree: HashMap<&str, usize> = g.keys().map(|id| (id.as_str(), 0)).collect();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for (id, n) in g {
        for v in n["inputs"].as_object().unwrap().values() {
            if let Some((source, _)) = link(g, v) {
                *indegree.get_mut(id.as_str()).unwrap() += 1;
                children.entry(source).or_default().push(id);
            }
        }
    }
    let mut queue: VecDeque<&str> = indegree.iter().filter(|(_, degree)| **degree == 0).map(|(id, _)| *id).collect();
    let mut count = 0;
    while let Some(id) = queue.pop_front() {
        count += 1;
        for child in children.get(id).into_iter().flatten() {
            let degree = indegree.get_mut(child).unwrap();
            *degree -= 1;
            if *degree == 0 { queue.push_back(child); }
        }
    }
    if count != g.len() { return Err(AppError::bad("srv.wf.cycle")); }
    Ok(())
}

pub fn save(ctx: &Ctx, raw: Value) -> Result<Value> {
    let path = super::workflow::workflow_path(ctx);
    let mut body = raw.as_object().cloned().ok_or_else(|| AppError::bad("srv.graph.binding"))?;
    if body.remove("workflow_path").and_then(|v| v.as_str().map(str::to_string)).as_deref() != Some(path.as_str()) {
        return Err(AppError::coded(409, "srv.graph.changed"));
    }
    let p: Profile = serde_json::from_value(Value::Object(body)).map_err(|e| AppError::detail("srv.graph.profile", e))?;
    validate(&read(&path)?, &p)?;
    let text = serde_json::to_string(&p).map_err(|e| AppError::detail("srv.graph.profile", e))?;
    settings::put(ctx, &key(&path), &text)?;
    view(ctx)
}

pub fn view(ctx: &Ctx) -> Result<Value> {
    let path = super::workflow::workflow_path(ctx);
    let g = read(&path)?;
    let mut edges = Vec::new();
    for (id, n) in &g {
        for (field, v) in n["inputs"].as_object().unwrap() {
            if let Some((source, port)) = link(&g, v) {
                edges.push(json!({"source":source,"port":port,"target":id,"field":field}));
            }
        }
    }
    Ok(json!({"path":path,"graph":g,"edges":edges,"profile":profile(ctx, &path)?,"verified":false}))
}

pub fn compile(g: &Map<String, Value>, p: &Profile, photo: &str, mask: &str, values: &Value, seed: i64) -> Result<Value> {
    validate(g, p)?;
    let mut copy = g.clone();
    for b in &p.bindings {
        let value = match b.role.as_str() {
            "source_image" | "source_image_mask" => Value::String(photo.into()),
            "mask" => Value::String(mask.into()),
            "seed" => Value::from(seed),
            role => match values.get(role) { Some(v) => v.clone(), None => continue },
        };
        if !same_type(&g[&b.node_id]["inputs"][&b.field], &value) { return Err(AppError::bad("srv.graph.binding")); }
        copy.get_mut(&b.node_id).unwrap()["inputs"][&b.field] = value;
    }
    Ok(Value::Object(copy))
}

pub fn alpha_image(photo: &[u8], mask: &[u8]) -> Result<Vec<u8>> {
    let mut image = crate::img::codec::decode(photo)?;
    let mask = crate::img::codec::decode(mask)?;
    if !image.same_size(&mask) { return Err(AppError::bad("srv.graph.maskSize")); }
    // Synco 的蒙版强度存在 alpha；LoadImage 返回的 MASK 是 1 - 图片 alpha。
    for (pixel, mask_pixel) in image.px.chunks_exact_mut(4).zip(mask.px.chunks_exact(4)) { pixel[3] = 255 - mask_pixel[3]; }
    Ok(crate::img::codec::encode_png(&image))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_node_and_prompt_wrapper_are_distinct() {
        let g = json!({"prompt":{"class_type":"PrimitiveStringMultiline","inputs":{"value":"text"}}});
        assert_eq!(parse(&g).unwrap().len(), 1);
        assert_eq!(parse(&json!({"prompt":g})).unwrap().len(), 1);
        assert!(parse(&json!({"nodes":[]})).is_err());
    }
    #[test]
    fn arrays_are_not_links_and_compilation_preserves_source() {
        let g = parse(&json!({"a":{"class_type":"Text","inputs":{"value":"original","array":["a",-1]}},"b":{"class_type":"SaveImage","inputs":{"images":["a",0]}}})).unwrap();
        assert!(link(&g, &g["a"]["inputs"]["array"]).is_none());
        let p = Profile { bindings: vec![crate::models::workflow::Binding { node_id:"a".into(),field:"value".into(),role:"prompt".into() }], output_node:"b".into() };
        let result = compile(&g,&p,"p","m",&json!({"prompt":"new"}),1).unwrap();
        assert_eq!(result["a"]["inputs"]["value"],"new");
        assert_eq!(g["a"]["inputs"]["value"],"original");
        let invalid = Profile { bindings:vec![crate::models::workflow::Binding { node_id:"b".into(),field:"images".into(),role:"prompt".into() }], ..p };
        assert!(validate(&g,&invalid).is_err());
    }

    #[test]
    fn alpha_mask_keeps_rgb_and_inverts_mask_strength() {
        use crate::img::codec;
        let photo = stitch_core::Rgba::from_pixels(3, 1, vec![10,20,30,255,40,50,60,255,70,80,90,255]);
        let mask = stitch_core::Rgba::from_pixels(3, 1, vec![255,255,255,0,255,255,255,128,255,255,255,255]);
        let output = codec::decode(&alpha_image(&codec::encode_png(&photo), &codec::encode_png(&mask)).unwrap()).unwrap();
        assert_eq!(output.px, vec![10,20,30,255,40,50,60,127,70,80,90,0]);
    }

    #[test]
    fn duplicate_targets_and_cycles_are_rejected() {
        let mut g = parse(&json!({"a":{"class_type":"Text","inputs":{"value":"old"}},"b":{"class_type":"SaveImage","inputs":{"images":["a",0]}}})).unwrap();
        let b = crate::models::workflow::Binding { node_id:"a".into(), field:"value".into(), role:"prompt".into() };
        let mut p = Profile { bindings:vec![b.clone(),b], output_node:"b".into() };
        assert_eq!(validate(&g,&p).unwrap_err().reason().0,"srv.graph.binding");
        p.bindings.clear();
        g.get_mut("a").unwrap()["inputs"]["value"] = json!(["b",0]);
        assert_eq!(validate(&g,&p).unwrap_err().reason().0,"srv.wf.cycle");
    }
}
