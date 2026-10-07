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
    o.insert("kind".into(), Value::String(img.kind.clone()));
    // 派生谱系是这一行自己的事实，不是聚合：单图接口与派生列表都要读得到
    o.insert("derived_from".into(), img.derived_from.map(Value::from).unwrap_or(Value::Null));
    o.insert("derived_result".into(), img.derived_result.map(Value::from).unwrap_or(Value::Null));
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

/// 项目详情的一行图片 = `image_json` + 角标要的聚合。
/// 结果数由 `repo::images::list_for_project_rows` 在同一条 SELECT 里算出来：
/// 前端那个 `has_result` 只有本次会话写得进去，刷新一次卡片就全说"没出过图"，这里补上真实计数。
pub fn image_row_json(ctx: &Ctx, row: &Value) -> Value {
    let img = Image::from_value(row);
    let mut o = match image_json(ctx, &img, true).as_object().cloned() {
        Some(m) => m,
        None => return Value::Null,
    };
    let num = |k: &str| row.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    // 成图小档没切出来时回落到成图本体；文件已经不在盘上的当没有，别让角标挂一张碎图
    let alive = |k: &str| {
        row.get(k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty() && util::file_alive(&ctx.data, &Value::String(s.to_string())))
            .map(str::to_string)
    };
    let latest = alive("result_thumb").or_else(|| alive("result_final"));
    o.insert("result_count".into(), Value::from(num("result_count")));
    o.insert("result_done".into(), Value::from(num("result_done")));
    o.insert("latest_result_url".into(), url(latest.as_ref()));
    // 角标的第二个数：这张图另存出去了几张子图（父子关系本身由 image_json 带）
    o.insert("derived_count".into(), Value::from(num("derived_count")));
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
    // 画布那一版的线稿快照：照片那一路永远没有这一项，所以只在有的时候给
    if o.contains_key("sketch_path") || r.sketch_path.is_some() {
        o.insert("sketch_url".into(), url(r.sketch_path.as_ref()));
        o.insert("sketch_dead".into(), dead(ctx, r.sketch_path.as_ref()));
    }
    // 这一版**实际带走**的参考图（行上的快照，不是此刻的槽位）：历史条目要数得出几张、
    // 点开要看得见是哪几张，文件丢了也得说"丢了"而不是摆一张碎图
    let ref_rels = crate::service::refs::row_refs(&r);
    if !ref_rels.is_empty() {
        let list: Vec<Value> = ref_rels
            .iter()
            .map(|rel| {
                serde_json::json!({
                    "url": format!("/file/{rel}"),
                    "name": util::stem_of(rel),
                    "dead": !util::file_alive(&ctx.data, &Value::String(rel.to_string())),
                })
            })
            .collect();
        o.insert("refs".into(), Value::Array(list));
    }
    Value::Object(o)
}

/// 预设只装提示词/负面/步数/CFG/LoRA 链——种子和遮罩笔迹不进预设
pub fn preset_json(p: &Value) -> Value {
    serde_json::json!({
        "id": p.get("id").cloned().unwrap_or(Value::Null),
        "name": p.get("name").cloned().unwrap_or(Value::Null),
        "kind": p.get("kind").cloned().unwrap_or_else(|| Value::String("preset".into())),
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

/// 指令与负面词的上限：以前这两列一个字都没截过，粘贴进来的一篇长文会整份存进库并原样发给云端
pub const PROMPT_MAX: usize = 4000;
pub const NEG_MAX: usize = 2000;

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
        prompt: util::clip(body.get("prompt").and_then(|v| v.as_str()).unwrap_or(""), PROMPT_MAX),
        negative: util::clip(body.get("negative").and_then(|v| v.as_str()).unwrap_or(""), NEG_MAX),
        steps: util::num_or(body.get("steps"), 20.0),
        cfg: util::num_or(body.get("cfg"), 3.0),
        loras_json: Value::Array(loras).to_string(),
    }
}
