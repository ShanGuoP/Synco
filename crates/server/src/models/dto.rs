//! 响应形状：表行直展 + 派生键。前端按字段名读，加键要一并想清楚谁在消费它，
//! 只有 M3 新增的 `thumb_url` / `proxy_url`（缺失为 null，前端回退 orig_url）是 Rust 版独有。

use crate::models::entity::{Image, ResultRow};
use crate::state::Ctx;
use crate::util;
use serde_json::{Map, Value};

fn url(rel: Option<&String>) -> Value {
    match rel {
        Some(r) => Value::String(format!("/file/{r}")),
        None => Value::Null,
    }
}
fn dead(ctx: &Ctx, rel: Option<&String>) -> Value {
    Value::Bool(rel.map(|r| !util::file_alive(&ctx.data, &Value::String(r.clone()))).unwrap_or(false))
}

/// 项目详情的图片列表带 created_at，单图接口不带（Node 侧就是这么分的）
pub fn image_json(ctx: &Ctx, img: &Image, with_created: bool) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(img.id));
    o.insert("project_id".into(), Value::from(img.project_id));
    o.insert("name".into(), Value::String(img.name.clone()));
    o.insert("w".into(), Value::from(img.w));
    o.insert("h".into(), Value::from(img.h));
    if with_created {
        o.insert("created_at".into(), img.created_at.clone().map(Value::String).unwrap_or(Value::Null));
    }
    o.insert("orig_url".into(), url(Some(&img.orig_path)));
    o.insert("mask_url".into(), url(img.mask_path.as_ref()));
    o.insert("has_mask".into(), Value::Bool(img.has_mask()));
    o.insert("orig_dead".into(), dead(ctx, Some(&img.orig_path)));
    o.insert("thumb_url".into(), url(img.thumb_path.as_ref()));
    o.insert("proxy_url".into(), url(img.proxy_path.as_ref()));
    // 瓦片清单是懒生成的，所以给的是接口地址而不是具体某一张
    o.insert("tiles_url".into(), Value::String(format!("/api/images/{}/tiles", img.id)));
    Value::Object(o)
}

/// 结果行 → 接口形状：路径转 URL，并标出"库里有这行、盘上没这个文件"。
/// 键集合刻意跟随 `SELECT` 的列：单图接口只选了 15 列、`/api/results/:id` 是全列，
/// Node 那边就是 `{...row}` 直展，固定成一套键会凭空多出字段来。
pub fn result_json(ctx: &Ctx, row: &Value) -> Value {
    let r = ResultRow::from_value(row);
    let mut o = row.as_object().cloned().unwrap_or_default();
    for (col, rel) in [
        ("final", &r.final_path),
        ("crop", &r.crop_path),
        ("maskoverlay", &r.maskoverlay_path),
    ] {
        o.insert(format!("{col}_url"), url(rel.as_ref()));
        o.insert(format!("{col}_dead"), dead(ctx, rel.as_ref()));
    }
    // M3 才有这一列；老库里没成图缩略图时回 null，前端回落到 final_url
    if o.contains_key("thumb_path") || r.thumb_path.is_some() {
        o.insert("thumb_url".into(), url(r.thumb_path.as_ref()));
    }
    Value::Object(o)
}

/// 预设只装提示词/负面/步数/CFG/LoRA 链——种子和遮罩笔迹不进预设
pub fn preset_json(p: &Value) -> Value {
    serde_json::json!({
        "id": p.get("id").cloned().unwrap_or(Value::Null),
        "name": p.get("name").cloned().unwrap_or(Value::Null),
        "project_id": p.get("project_id").cloned().unwrap_or(Value::Null),
        "prompt": p.get("prompt").cloned().unwrap_or(Value::Null),
        "negative": p.get("negative").cloned().unwrap_or(Value::Null),
        "steps": p.get("steps").cloned().unwrap_or(Value::Null),
        "cfg": p.get("cfg").cloned().unwrap_or(Value::Null),
        "loras": Value::Array(util::safe_arr(p.get("loras_json").and_then(|v| v.as_str()))),
        "updated_at": p.get("updated_at").cloned().unwrap_or(Value::Null),
    })
}

pub struct PresetFields {
    pub name: String,
    pub prompt: String,
    pub negative: String,
    pub steps: f64,
    pub cfg: f64,
    pub loras_json: String,
}

pub fn preset_fields(body: &Value) -> PresetFields {
    let loras = match body.get("loras").and_then(|v| v.as_array()) {
        Some(arr) => arr
            .iter()
            .map(|l| {
                serde_json::json!({
                    "name": l.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "strength": util::num_or(l.get("strength"), 0.0),
                    "enabled": l.get("enabled").and_then(Value::as_bool).unwrap_or(true)
                })
            })
            .collect::<Vec<Value>>(),
        None => Vec::new(),
    };
    PresetFields {
        name: util::clip(body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim(), 40),
        prompt: body.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        negative: body.get("negative").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        steps: util::num_or(body.get("steps"), 20.0),
        cfg: util::num_or(body.get("cfg"), 3.0),
        loras_json: Value::Array(loras).to_string(),
    }
}
