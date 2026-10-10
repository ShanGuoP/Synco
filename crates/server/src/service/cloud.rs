//! 云端局部重绘：没装 ComfyUI 的机器走这条。base_url / 模型 / 尺寸 / 画质全部由用户自己填。
//! 明文 key 只存在库里，任何 GET 都不回显（只回 key_saved + 末四位）。

use crate::backend::{join, norm_base};
use crate::error::AppError;
use crate::repo::settings::{del as del_setting, get_raw, put as put_setting};
use crate::state::Ctx;
use crate::util::{clamp_round, clip};
use serde::Serialize;
use serde_json::{json, Map, Value};
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
pub fn save(ctx: &Ctx, patch: &Value) -> crate::error::Result<Map<String, Value>> {
    let mut out = Map::new();
    if patch.get("kind").is_some() {
        let k = if patch["kind"].as_str() == Some("cloud") { "cloud" } else { "comfyui" };
        put_setting(ctx, KEY.0, k)?;
        out.insert("kind".into(), Value::String(k.into()));
    }
    if patch.get("base").is_some() {
        let b = norm_base(patch["base"].as_str().unwrap_or(""))?;
        put_setting(ctx, KEY.1, &b)?;
        out.insert("base".into(), Value::String(b));
    }
    if patch.get("model").is_some() {
        let m = clip(patch["model"].as_str().unwrap_or("").trim(), 80);
        put_setting(ctx, KEY.2, &m)?;
        out.insert("model".into(), Value::String(m));
    }
    for (field, idx, max) in [("size", KEY.3, 20usize), ("quality", KEY.4, 20)] {
        if patch.get(field).is_some() {
            let v = clip(patch[field].as_str().unwrap_or("").trim(), max);
            put_setting(ctx, idx, &v)?;
            out.insert(field.into(), Value::String(v));
        }
    }
    if patch.get("timeout").is_some() {
        let v = clamp_round(patch.get("timeout"), TIMEOUT_MIN, 600_000, 180_000);
        put_setting(ctx, KEY.6, &v.to_string())?;
        out.insert("timeout_ms".into(), Value::from(v));
    }
    if patch.get("concurrency").is_some() {
        let v = clamp_round(patch.get("concurrency"), 1, 6, 1);
        put_setting(ctx, KEY.7, &v.to_string())?;
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
            put_setting(ctx, key, &v.to_string())?;
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
            put_setting(ctx, KEY.5, &k)?;
            out.insert("key_changed".into(), Value::Bool(true));
        }
    }
    if patch.get("clear_key").and_then(Value::as_bool).unwrap_or(false) {
        del_setting(ctx, KEY.5)?;
        out.insert("key_changed".into(), Value::Bool(true));
    }
    Ok(out)
}

/// 有些转发服务把 OpenAI 风格的错误塞在 JSON 里，原文比状态码有用得多
async fn read_error(r: reqwest::Response) -> AppError {
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
    // 对面那句话不是我们的文案：不进字典，只当 detail 参数原样带出去
    AppError::coded_args(502, "srv.cloud.upstream", json!({ "code": status, "detail": body.chars().take(240).collect::<String>() }))
}

/// 探活只 GET /models，绝不为了试连通烧一次生成
pub async fn probe(ctx: &Ctx) -> Value {
    let s = settings(ctx);
    let mut out = Map::new();
    if s.base.is_empty() {
        out.insert("ok".into(), Value::Bool(false));
        out.insert("error".into(), Value::String("srv.cloud.noUrl".into()));
        return Value::Object(out);
    }
    let t0 = std::time::Instant::now();
    let req = ctx.http.get(join(&s.base, "/models"));
    let req = if s.key.is_empty() { req } else { req.header("Authorization", format!("Bearer {}", s.key)) };
    let r = req.timeout(Duration::from_millis(s.timeout_ms.min(12_000) as u64)).send().await;
    match r {
        Err(e) => {
            out.insert("ok".into(), Value::Bool(false));
            let (code, args) = AppError::fail_detail("srv.cloud.probeFail", e).reason();
            out.insert("error".into(), Value::String(code));
            if let Some(a) = args { out.insert("error_args".into(), Value::Object(a)); }
            out.insert("ms".into(), Value::from(t0.elapsed().as_millis() as i64));
        }
        Ok(resp) => {
            let status = resp.status().as_u16();
            if !(100..=299).contains(&status) {
                out.insert("ok".into(), Value::Bool(false));
                out.insert("status".into(), Value::from(status));
                let (code, args) = read_error(resp).await.reason();
                out.insert("error".into(), Value::String(code));
                if let Some(a) = args { out.insert("error_args".into(), Value::Object(a)); }
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

/// 应答原文压成一行给界面看：换行与连续空白都收成单空格，只留前两二百个字符
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect()
}

/// 传输层失败：种类在这里判、句子在字典里，调用方原样往外传。
/// 细节取错误链上的下一层；链空了才退回 Display（它带 URL，而 URL 本来就在 `{url}` 那格里）。
fn send_err(url: &str, e: reqwest::Error) -> AppError {
    let code = if e.is_timeout() {
        "srv.cloud.timeout"
    } else if e.is_connect() {
        "srv.cloud.connect"
    } else {
        "srv.cloud.send"
    };
    AppError::detailed_args(502, code, json!({ "url": url.to_string() }), crate::util::reqwest_detail(&e))
}

/// 一次出站图像的**单文件**上限。中转那头的口径各家不一样（20MB / 50MB），取最紧的那个：
/// 超了不是"慢一点"，而是对面直接拒收，用户只看到提交失败、不知所以。
pub const SEND_MAX_BYTES: usize = 20 * 1024 * 1024;
/// 一次请求里所有图像 part 的**合计**上限。这个数不是接口契约——多参考图之后单文件判据不够用了
/// （四张各 19MB 单看都合规，合起来 76MB），先按自家口径收在"三张单文件上限"，撞到货柜再改。
pub const SEND_MAX_TOTAL: usize = 60 * 1024 * 1024;
/// 参考图的字段名。`edits` 实测过的形状只有单 `image` + 可选 `mask`；
/// 多图那一族（`image[]` / 同名重复 `image` / `image_1`…）还没验过，由 `tools/sketch-probe.js --refs` 去撞。
/// 换形状只改这一行，队列那边不用动。
pub const REF_FIELD: &str = "image[]";

/// 一次 `images/edits` 的载荷：主输入（画稿折白底 / 裁切区 / 整张原图）+ 可选遮罩 + 任意张参考图。
/// 三个字节都由调用方保证是 PNG——声明与实际编码不符会被对面整批拒收。
/// 画稿一笔没涂且挂了参考图时 `image` 传 `None`，这一枪就是"只按参考图与提示词生成"。
pub struct Edit<'a> {
    pub image: Option<Vec<u8>>,
    pub mask: Option<Vec<u8>>,
    pub refs: Vec<Vec<u8>>,
    pub prompt: &'a str,
    pub size: Option<&'a str>,
}

/// 一次裁切区重绘：图 + 同尺寸「透明=重绘」遮罩，回一张 PNG 字节。
/// `mask` 传 `None` 时**不带这个 part**——反向涂抹一笔没涂就是"整幅重绘"，
/// 与其造一张全透明的巨图（各家的遮罩体积上限还不一样），不如按接口本来的样子省略。
pub async fn edit(ctx: &Ctx, e: Edit<'_>) -> crate::error::Result<Vec<u8>> {
    let s = settings(ctx);
    if s.base.is_empty() {
        return Err(AppError::bad("srv.cloud.noBase"));
    }
    if s.model.is_empty() {
        return Err(AppError::bad("srv.cloud.noModel"));
    }
    if s.key.is_empty() {
        return Err(AppError::bad("srv.cloud.noKey"));
    }
    // 先按长度判两道（单文件 / 合计），再拼装 multipart：判体积不该把几十 MB 复制一遍
    let mut sizes: Vec<usize> = Vec::new();
    for buf in e.image.iter().chain(e.refs.iter()).chain(e.mask.iter()) {
        sizes.push(buf.len());
    }
    if let Some(big) = sizes.iter().max() {
        if *big > SEND_MAX_BYTES {
            return Err(AppError::bad_args(
                "srv.cloud.fileOver",
                json!({ "mb": big / 1048576, "cap": SEND_MAX_BYTES / 1048576 }),
            ));
        }
    }
    let total: usize = sizes.iter().sum();
    if total > SEND_MAX_TOTAL {
        return Err(AppError::bad_args(
            "srv.cloud.totalOver",
            json!({ "mb": total / 1048576, "cap": SEND_MAX_TOTAL / 1048576 }),
        ));
    }
    let png = |name: &str, b: Vec<u8>| {
        reqwest::multipart::Part::bytes(b).file_name(name.to_string()).mime_str("image/png").expect("mime 常量")
    };
    let mut form = reqwest::multipart::Form::new()
        .text("prompt", e.prompt.to_string())
        .text("model", s.model.clone())
        // 显式要 b64：回 URL 的话地址可能带有效期，还得多一跳去下载
        .text("response_format", "b64_json");
    if let Some(buf) = e.image {
        form = form.part("image", png("image.png", buf));
    }
    for (i, buf) in e.refs.into_iter().enumerate() {
        form = form.part(REF_FIELD, png(&format!("ref{}.png", i + 1), buf));
    }
    if let Some(m) = e.mask {
        form = form.part("mask", png("mask.png", m));
    }
    // 尺寸由调用方按原图比例算好后带过来（发出去的就是这个尺寸，不会拉伸）；没带才用设置里填的
    let sz = e.size.unwrap_or(&s.size).trim().to_string();
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
        .map_err(|e| send_err(&s.base, e))?;
    if !r.status().is_success() {
        return Err(read_error(r).await);
    }
    let text = r.text().await.map_err(|e| send_err(&s.base, e))?;
    let j: Value = serde_json::from_str(&text)
        .map_err(|_| AppError::bad_args("srv.cloud.notJson", json!({ "detail": flat(&text) })))?;
    let one = j.get("data").and_then(|d| d.get(0)).cloned().unwrap_or(j.clone());
    if let Some(b64) = one.get("b64_json").and_then(|v| v.as_str()) {
        let bytes = crate::util::decode_b64(b64);
        if bytes.is_empty() {
            return Err(AppError::fail("srv.cloud.b64Empty"));
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
            .map_err(|e| send_err(&s.base, e))?;
        if !img.status().is_success() {
            return Err(AppError::fail_args("srv.cloud.urlFail", json!({ "code": img.status().as_u16() })));
        }
        return img.bytes().await.map(|b| b.to_vec()).map_err(|e| send_err(&s.base, e));
    }
    Err(AppError::bad_args("srv.cloud.noImage", json!({ "detail": flat(&text) })))
}

/// 给 settle/回收用的超时阈值：请求超时之外，再给缝合与回传留两分钟
pub const CLOUD_GRACE_MS: u128 = 120_000;

pub fn grace_ms(ctx: &Ctx) -> u128 {
    settings(ctx).timeout_ms as u128 + CLOUD_GRACE_MS
}
