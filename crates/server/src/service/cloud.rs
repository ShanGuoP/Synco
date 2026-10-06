//! 云端局部重绘：没装 ComfyUI 的机器走这条。base_url / 模型 / 尺寸 / 画质全部由用户自己填。
//! 明文 key 只存在库里，任何 GET 都不回显（只回 key_saved + 末四位）。

use crate::backend::{join, norm_base};
use crate::repo::settings::{del as del_setting, get_raw, put as put_setting};
use crate::state::Ctx;
use crate::util::{clamp_round, clip};
use serde::Serialize;
use serde_json::{Map, Value};
use std::time::Duration;

pub const TIMEOUT_MIN: i64 = 30_000;

#[derive(Clone, Debug, Serialize)]
pub struct CloudSettings {
    pub kind: String,
    pub base: String,
    pub model: String,
    pub size: String,
    pub quality: String,
    #[serde(skip_serializing)]
    pub key: String,
    pub timeout_ms: i64,
    pub concurrency: i64,
    pub stitch_expand: i64,
    pub stitch_feather: i64,
    pub stitch_edge: i64,
}

const KEY: (&str, &str, &str, &str, &str, &str, &str, &str, &str, &str, &str) = (
    "cloud_kind",
    "cloud_base",
    "cloud_model",
    "cloud_size",
    "cloud_quality",
    "cloud_key",
    "cloud_timeout",
    "cloud_concurrency",
    "stitch_expand",
    "stitch_feather",
    "stitch_edge",
);

/// 没存过的键取到 None：把"没填"和"填了 0"混在一起，默认值会被夹到下限
fn text(ctx: &Ctx, key: &str, fb: &str) -> String {
    match get_raw(ctx, key) {
        Some(v) => v.trim().to_string(),
        None => fb.to_string(),
    }
}

fn raw(ctx: &Ctx, key: &str) -> Option<Value> {
    get_raw(ctx, key).map(Value::String)
}

/// 服务端内部用：带明文 key，绝不允许直接回给前端
pub fn settings(ctx: &Ctx) -> CloudSettings {
    let kind = if get_raw(ctx, KEY.0).as_deref() == Some("cloud") { "cloud" } else { "comfyui" };
    let mut s = CloudSettings {
        kind: kind.to_string(),
        base: get_raw(ctx, KEY.1).unwrap_or_default(),
        model: get_raw(ctx, KEY.2).unwrap_or_default(),
        size: text(ctx, KEY.3, "1024"),
        quality: text(ctx, KEY.4, "medium"),
        key: get_raw(ctx, KEY.5).unwrap_or_default(),
        // 下限 30s：早期版本的空值处理把 timeout 写成 5000，而生图没有 5 秒能回来的
        timeout_ms: clamp_round(raw(ctx, KEY.6).as_ref(), TIMEOUT_MIN, 600_000, 180_000),
        concurrency: clamp_round(raw(ctx, KEY.7).as_ref(), 1, 6, 1),
        stitch_expand: clamp_round(raw(ctx, KEY.8).as_ref(), 16, 200, 96),
        stitch_feather: clamp_round(raw(ctx, KEY.9).as_ref(), 8, 200, 48),
        stitch_edge: clamp_round(raw(ctx, KEY.10).as_ref(), 512, 2048, 1024),
    };
    // 联动约束：羽化 ≤ 0.6×外扩，否则过渡带越过模型重绘区，缝反而更露。
    // 在服务端生效，设置页与前端拿到的都是实际生效值
    s.stitch_feather = s.stitch_feather.min((s.stitch_expand as f64 * 0.6).round() as i64);
    s
}

/// 给前端的样子：key 只回「存没存 + 末 4 位」
pub fn public_settings(ctx: &Ctx) -> Value {
    let s = settings(ctx);
    let mut m = serde_json::to_value(&s).unwrap_or_else(|_| Value::Object(Map::new()));
    let o = m.as_object_mut().expect("CloudSettings 序列化后是对象");
    o.remove("key");
    o.insert("key_saved".into(), Value::Bool(!s.key.is_empty()));
    let tail: String = s.key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    o.insert("key_tail".into(), Value::String(tail));
    m
}

/// 白名单落库；key 传空表示保持原值不动。返回改动过的键，供接口直接回给前端
pub fn save(ctx: &Ctx, patch: &Value) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    if patch.get("kind").is_some() {
        let k = if patch["kind"].as_str() == Some("cloud") { "cloud" } else { "comfyui" };
        put_setting(ctx, KEY.0, k).map_err(|e| e.to_string())?;
        out.insert("kind".into(), Value::String(k.into()));
    }
    if patch.get("base").is_some() {
        let b = norm_base(patch["base"].as_str().unwrap_or(""))?;
        put_setting(ctx, KEY.1, &b).map_err(|e| e.to_string())?;
        out.insert("base".into(), Value::String(b));
    }
    if patch.get("model").is_some() {
        let m = clip(patch["model"].as_str().unwrap_or("").trim(), 80);
        put_setting(ctx, KEY.2, &m).map_err(|e| e.to_string())?;
        out.insert("model".into(), Value::String(m));
    }
    for (field, idx, max) in [("size", KEY.3, 20usize), ("quality", KEY.4, 20)] {
        if patch.get(field).is_some() {
            let v = clip(patch[field].as_str().unwrap_or("").trim(), max);
            put_setting(ctx, idx, &v).map_err(|e| e.to_string())?;
            out.insert(field.into(), Value::String(v));
        }
    }
    if patch.get("timeout").is_some() {
        let v = clamp_round(patch.get("timeout"), TIMEOUT_MIN, 600_000, 180_000);
        put_setting(ctx, KEY.6, &v.to_string()).map_err(|e| e.to_string())?;
        out.insert("timeout_ms".into(), Value::from(v));
    }
    if patch.get("concurrency").is_some() {
        let v = clamp_round(patch.get("concurrency"), 1, 6, 1);
        put_setting(ctx, KEY.7, &v.to_string()).map_err(|e| e.to_string())?;
        out.insert("concurrency".into(), Value::from(v));
    }
    let mut touched_stitch = false;
    for (field, key, lo, hi, fb) in [
        ("stitch_expand", KEY.8, 16i64, 200i64, 96i64),
        ("stitch_feather", KEY.9, 8, 200, 48),
        ("stitch_edge", KEY.10, 512, 2048, 1024),
    ] {
        if patch.get(field).is_some() {
            touched_stitch = true;
            let v = clamp_round(patch.get(field), lo, hi, fb);
            put_setting(ctx, key, &v.to_string()).map_err(|e| e.to_string())?;
            out.insert(field.into(), Value::from(v));
        }
    }
    // 保存了任一缝合参数就把三个都按生效值回给前端（联动约束在 settings() 里做）
    if touched_stitch {
        let s = settings(ctx);
        out.insert("stitch_expand".into(), Value::from(s.stitch_expand));
        out.insert("stitch_feather".into(), Value::from(s.stitch_feather));
        out.insert("stitch_edge".into(), Value::from(s.stitch_edge));
    }
    if patch.get("key").is_some() {
        let k = patch["key"].as_str().unwrap_or("").trim().to_string();
        if !k.is_empty() {
            put_setting(ctx, KEY.5, &k).map_err(|e| e.to_string())?;
            out.insert("key_changed".into(), Value::Bool(true));
        }
    }
    if patch.get("clear_key").and_then(Value::as_bool).unwrap_or(false) {
        del_setting(ctx, KEY.5).map_err(|e| e.to_string())?;
        out.insert("key_changed".into(), Value::Bool(true));
    }
    Ok(out)
}

/// 有些转发服务把 OpenAI 风格的错误塞在 JSON 里，原文比状态码有用得多
async fn read_error(r: reqwest::Response) -> String {
    let status = r.status().as_u16();
    let text = r.text().await.unwrap_or_default();
    let msg: Option<String> = serde_json::from_str::<Value>(&text).ok().and_then(|j| {
        j.get("error")
            .and_then(|e| e.get("message").or_else(|| e.get("code")))
            .and_then(|v| v.as_str())
            .or_else(|| j.get("message").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
    });
    let body = match msg {
        Some(m) => m,
        None => text.split_whitespace().collect::<Vec<_>>().join(" "),
    };
    format!("HTTP {status}：{}", body.chars().take(240).collect::<String>())
}

/// 探活只 GET /models，绝不为了试连通烧一次生成
pub async fn probe(ctx: &Ctx) -> Value {
    let s = settings(ctx);
    let mut out = Map::new();
    if s.base.is_empty() {
        out.insert("ok".into(), Value::Bool(false));
        out.insert("error".into(), Value::String("还没填 base_url".into()));
        return Value::Object(out);
    }
    let t0 = std::time::Instant::now();
    let req = ctx.http.get(join(&s.base, "/models"));
    let req = if s.key.is_empty() { req } else { req.header("Authorization", format!("Bearer {}", s.key)) };
    let r = req.timeout(Duration::from_millis(s.timeout_ms.min(12_000) as u64)).send().await;
    match r {
        Err(e) => {
            out.insert("ok".into(), Value::Bool(false));
            out.insert("error".into(), Value::String(e.to_string().chars().take(200).collect()));
            out.insert("ms".into(), Value::from(t0.elapsed().as_millis() as i64));
        }
        Ok(resp) => {
            let status = resp.status().as_u16();
            if !(100..=299).contains(&status) {
                out.insert("ok".into(), Value::Bool(false));
                out.insert("status".into(), Value::from(status));
                out.insert("error".into(), Value::String(read_error(resp).await));
                out.insert("ms".into(), Value::from(t0.elapsed().as_millis() as i64));
            } else {
                let text = resp.text().await.unwrap_or_default();
                let n = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|j| j.get("data").and_then(|d| d.as_array()).map(|a| a.len()))
                    .map(Value::from)
                    .unwrap_or(Value::Null);
                out.insert("ok".into(), Value::Bool(true));
                out.insert("status".into(), Value::from(status));
                out.insert("ms".into(), Value::from(t0.elapsed().as_millis() as i64));
                out.insert("models".into(), n);
            }
        }
    }
    Value::Object(out)
}

/// 一次裁切区重绘：图 + 同尺寸「透明=重绘」遮罩，回一张 PNG 字节
pub async fn edit(ctx: &Ctx, image_buf: Vec<u8>, mask_buf: Vec<u8>, prompt: &str, size: Option<&str>) -> Result<Vec<u8>, String> {
    let s = settings(ctx);
    if s.base.is_empty() {
        return Err("云端还没配置 base_url（设置 → 云端）".into());
    }
    if s.model.is_empty() {
        return Err("云端还没填模型名".into());
    }
    if s.key.is_empty() {
        return Err("云端还没填 API key".into());
    }
    let png = |name: &str, b: Vec<u8>| {
        reqwest::multipart::Part::bytes(b).file_name(name.to_string()).mime_str("image/png").expect("mime 常量")
    };
    let mut form = reqwest::multipart::Form::new()
        .part("image", png("image.png", image_buf))
        .part("mask", png("mask.png", mask_buf))
        .text("prompt", prompt.to_string())
        .text("model", s.model.clone())
        // 显式要 b64：回 URL 的话地址可能带有效期，还得多一跳去下载
        .text("response_format", "b64_json");
    // 尺寸由调用方按原图比例算好后带过来（发出去的就是这个尺寸，不会拉伸）；没带才用设置里填的
    let sz = size.unwrap_or(&s.size).trim().to_string();
    if !sz.is_empty() {
        form = form.text("size", sz);
    }
    if !s.quality.is_empty() {
        form = form.text("quality", s.quality.clone());
    }
    let r = ctx
        .http
        .post(join(&s.base, "/images/edits"))
        .header("Authorization", format!("Bearer {}", s.key))
        .multipart(form)
        .timeout(Duration::from_millis(s.timeout_ms as u64))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(read_error(r).await);
    }
    let text = r.text().await.map_err(|e| e.to_string())?;
    let j: Value = serde_json::from_str(&text)
        .map_err(|_| format!("云端应答不是 JSON：{}", text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect::<String>()))?;
    let one = j.get("data").and_then(|d| d.get(0)).cloned().unwrap_or(j.clone());
    if let Some(b64) = one.get("b64_json").and_then(|v| v.as_str()) {
        let bytes = crate::util::decode_b64(b64);
        if bytes.is_empty() {
            return Err("云端应答里的 b64_json 是空的".into());
        }
        return Ok(bytes);
    }
    if let Some(url) = one.get("url").and_then(|v| v.as_str()) {
        let img = ctx
            .http
            .get(url)
            .timeout(Duration::from_millis(s.timeout_ms as u64))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !img.status().is_success() {
            return Err(format!("云端给了地址但取不回图 HTTP {}", img.status().as_u16()));
        }
        return img.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string());
    }
    Err(format!("云端应答里没有图：{}", text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect::<String>()))
}

/// 给 settle/回收用的超时阈值：请求超时之外，再给缝合与回传留两分钟
pub const CLOUD_GRACE_MS: u128 = 120_000;

pub fn grace_ms(ctx: &Ctx) -> u128 {
    settings(ctx).timeout_ms as u128 + CLOUD_GRACE_MS
}
