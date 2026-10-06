//! ComfyUI 客户端：上传 / 提交 / 轮询 / 下载 / 图谱构建。
//! 地址每次现取，切换后端不用重启进程。

use crate::backend::{active_url, join};
use crate::service::workflow::CROP_KEYS;
use crate::state::Ctx;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::time::Duration;

/// 本机这一路的采样参数：先判再进图。
///
/// 要挡的是"云端形状的 0 步 0 CFG"串回本机后被直接送进 KSampler——那不报错，
/// 只出一张没人看得出问题的图。跳过并给理由，比悄悄跑一张废图诚实。
pub fn sample_args(settings: &Value) -> std::result::Result<(i64, f64), String> {
    let steps = crate::util::number_of(settings.get("steps")).ok_or("请求里没带采样步数")?;
    let cfg = crate::util::number_of(settings.get("cfg")).ok_or("请求里没带 CFG")?;
    if !steps.is_finite() || !(1.0..=200.0).contains(&steps) {
        return Err(format!("采样步数不合法（{steps}）：这一路要 1–200 步，云端的结果没有步数，回填后请先补上"));
    }
    if !cfg.is_finite() || !(0.0..=30.0).contains(&cfg) {
        return Err(format!("CFG 不合法（{cfg}）：这一路要 0–30"));
    }
    Ok((steps.round() as i64, cfg))
}

/// 每个接口都可能被 ComfyUI 挂住（模型装载中、显存回收中），没有超时的请求会把这条 HTTP 永久吊着
pub const T_UPLOAD: u64 = 60_000;
pub const T_PROMPT: u64 = 20_000;
pub const T_POLL: u64 = 10_000;
pub const T_FILE: u64 = 120_000;

/// ComfyUI 出错时给的是 HTML 页，直接当 JSON 解析抛 SyntaxError 会把真正的原因吞掉
async fn fetch_json(req: reqwest::RequestBuilder, url_for_msg: &str, ms: u64) -> Result<Value, String> {
    let r = req.timeout(Duration::from_millis(ms)).send().await.map_err(|e| e.strip())?;
    let status = r.status().as_u16();
    let text = r.text().await.map_err(|e| e.strip())?;
    if !(100..=299).contains(&status) {
        let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let flat: String = flat.chars().take(180).collect();
        return Err(format!(
            "ComfyUI HTTP {status}：{}",
            if flat.is_empty() { url_for_msg.to_string() } else { flat }
        ));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|_| format!("ComfyUI 的应答不是 JSON（HTTP {status}）：{}", text.chars().take(180).collect::<String>()))
}

trait Strip {
    fn strip(self) -> String;
}
impl Strip for reqwest::Error {
    fn strip(self) -> String {
        let kind = if self.is_timeout() {
            "超时"
        } else if self.is_connect() {
            "连不上"
        } else {
            "请求失败"
        };
        // reqwest 的 Display 会把完整 URL 带进来（云端那条 URL 可能有凭据），
        // 所以只取错误链上的下一层：连接被拒时会露出 io 层的 os error
        let mut detail = String::new();
        let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&self);
        while let Some(e) = src {
            let t = e.to_string();
            if !t.is_empty() && !t.contains("http") {
                detail = t;
            }
            src = e.source();
        }
        let msg = if detail.is_empty() { kind.to_string() } else { format!("{kind}：{detail}") };
        msg.chars().take(200).collect()
    }
}

/// 上传一张图，返回 ComfyUI 侧的引用名（可能带子目录）
pub async fn upload(ctx: &Ctx, buf: Vec<u8>, filename: &str) -> Result<String, String> {
    let url = join(&active_url(ctx), "/upload/image");
    let form = reqwest::multipart::Form::new().part(
        "image",
        reqwest::multipart::Part::bytes(buf)
            .file_name(filename.to_string())
            .mime_str("image/png")
            .map_err(|e| e.to_string())?,
    ).text("overwrite", "true");
    let j = fetch_json(ctx.http.post(&url).multipart(form), &url, T_UPLOAD).await?;
    Ok(match (j.get("subfolder").and_then(|v| v.as_str()), j.get("name").and_then(|v| v.as_str())) {
        (Some(sub), Some(name)) if !sub.is_empty() => format!("{sub}/{name}"),
        (_, Some(name)) => name.to_string(),
        _ => String::new(),
    })
}

pub async fn post_json(ctx: &Ctx, p: &str, obj: Value) -> Result<Value, String> {
    let url = join(&active_url(ctx), p);
    fetch_json(ctx.http.post(&url).json(&obj), &url, T_PROMPT).await
}

/// 把 ComfyUI 的产物写到 DATA 下的绝对路径
pub async fn download_to(ctx: &Ctx, url: &str, dest: &std::path::Path) -> Result<(), String> {
    let r = ctx.http.get(url).timeout(Duration::from_millis(T_FILE)).send().await.map_err(|e| e.strip())?;
    if !r.status().is_success() {
        return Err(format!("回传成图失败 HTTP {}", r.status().as_u16()));
    }
    let bytes = r.bytes().await.map_err(|e| e.strip())?;
    std::fs::write(dest, bytes).map_err(|e| format!("写入 {} 失败：{e}", dest.display()))
}

/// 队列里还活着的 prompt_id：服务重启时用它认哪些 running 记录已经是僵尸
pub async fn live_prompts(ctx: &Ctx) -> Result<HashSet<String>, String> {
    let url = join(&active_url(ctx), "/queue");
    let j = fetch_json(ctx.http.get(&url), &url, T_POLL).await?;
    let mut ids = HashSet::new();
    for key in ["queue_running", "queue_pending"] {
        if let Some(arr) = j.get(key).and_then(|v| v.as_array()) {
            for item in arr {
                // 队列项是 [index, prompt_id, ...] 的元组数组
                if let Some(id) = item.get(1).and_then(|v| v.as_str()) {
                    ids.insert(id.to_string());
                } else if let Some(id) = item.get(1).and_then(|v| v.as_i64()) {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    Ok(ids)
}

pub enum Check {
    /// 后端连不上：与"没这条任务"是两种完全不同的事
    Unreachable(String),
    Queued,
    Lost,
    Error(String),
    Running,
    Done(Vec<Tagged>),
}

pub struct Tagged {
    pub tag: &'static str,
    pub url: String,
}

pub async fn check_comfy(ctx: &Ctx, pid: &str) -> Check {
    let base = active_url(ctx);
    let hist_url = join(&base, &format!("/history/{pid}"));
    let h = match fetch_json(ctx.http.get(&hist_url), &hist_url, T_POLL).await {
        Ok(v) => v,
        Err(e) => return Check::Unreachable(e),
    };
    if h.get(pid).is_none() {
        /* history 里没有这条有两种完全不同的解释：还在队列里没跑到，或者已经被 ComfyUI 忘掉
           （重启、点"清空历史"、历史条数把它挤出去）。只看 history 就回 running，工坊会替它转圈到天荒地老。 */
        let live = match live_prompts(ctx).await {
            Ok(v) => v,
            Err(e) => return Check::Unreachable(e),
        };
        return if live.contains(pid) { Check::Queued } else { Check::Lost };
    }
    let entry = h.get(pid).cloned().unwrap_or(Value::Null);
    let st = entry.get("status").cloned().unwrap_or(Value::Null);
    if st.get("status_str").and_then(|v| v.as_str()) == Some("error") {
        let msg = st
            .get("messages")
            .and_then(|m| m.as_array())
            .and_then(|a| a.iter().find(|m| m.get(0).and_then(|x| x.as_str()) == Some("execution_error")))
            .cloned();
        let detail = match msg {
            Some(m) => m.get(1).cloned().unwrap_or(Value::Null),
            None => Value::String("execution error".into()),
        };
        let txt = serde_json::to_string(&detail).unwrap_or_default();
        return Check::Error(txt.chars().take(500).collect());
    }
    if st.get("completed").and_then(Value::as_bool) != Some(true) {
        return Check::Running;
    }
    // 输出节点号固定 17/19/20，对应 final / crop / maskoverlay
    let mut images = Vec::new();
    let outs = entry.get("outputs").cloned().unwrap_or(Value::Null);
    for (nid, tag) in [("17", "final"), ("19", "crop"), ("20", "maskoverlay")] {
        let list = outs
            .get(nid)
            .and_then(|o| o.get("images"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for im in list {
            let filename = im.get("filename").and_then(|v| v.as_str()).unwrap_or("");
            let subfolder = im.get("subfolder").and_then(|v| v.as_str()).unwrap_or("");
            let itype = im.get("type").and_then(|v| v.as_str()).unwrap_or("output");
            let q = format!(
                "filename={}&subfolder={}&type={}",
                percent_encode(filename),
                percent_encode(subfolder),
                itype
            );
            images.push(Tagged { tag, url: format!("{}{}?{q}", base, "/view") });
        }
    }
    Check::Done(images)
}

/// encodeURIComponent 的等价物：保留 A-Za-z0-9-_.!~*'()，其余按 UTF-8 百分号编码
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 中断一条本机任务：/interrupt 掐掉当前执行，再把它从队列里移走。
/// 两个接口在不同 ComfyUI 版本上行为不一样，都尽力做，失败只回原因不抛。
pub async fn interrupt(ctx: &Ctx, prompt_id: &str) -> Vec<String> {
    let mut errs = Vec::new();
    if let Err(e) = post_json(ctx, "/interrupt", Value::Object(Map::new())).await {
        errs.push(format!("interrupt：{e}"));
    }
    let pid = prompt_id.to_string();
    let url = join(&active_url(ctx), "/queue");
    let body = serde_json::json!({ "prompt_id": pid });
    let del = ctx
        .http
        .delete(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .timeout(Duration::from_millis(T_POLL))
        .send()
        .await;
    match del {
        Ok(r) if (100..=299).contains(&r.status().as_u16()) => {}
        _ => {
            // 老版没有 DELETE /queue，退回 POST /queue 的 queue_remove
            if let Err(e) = post_json(ctx, "/queue", serde_json::json!({ "queue_remove": [pid] })).await {
                errs.push(format!("移出队列：{e}"));
            }
        }
    }
    errs
}

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

#[cfg(test)]
mod tests {
    use super::sample_args;
    use serde_json::json;

    #[test]
    fn 合法采样参数按面板区间放行() {
        assert_eq!(sample_args(&json!({ "steps": 20, "cfg": 3 })), Ok((20, 3.0)));
        assert_eq!(sample_args(&json!({ "steps": "4", "cfg": "0.5" })), Ok((4, 0.5)));
        assert_eq!(sample_args(&json!({ "steps": 20.6, "cfg": 3 })), Ok((21, 3.0)), "步数取整");
    }

    /// M4 的回归：云端行是 steps=0/cfg=0 的形状，串回本机时不能把 0 送进 KSampler
    #[test]
    fn 云端形状的零零参数被挡在提交之外() {
        for bad in [json!({ "steps": 0, "cfg": 0 }), json!({ "steps": -3, "cfg": 3 }), json!({ "cfg": 3 }), json!({ "steps": 20 })] {
            assert!(sample_args(&bad).is_err(), "{bad} 不该被接受");
        }
        assert!(sample_args(&json!({ "steps": 999, "cfg": 3 })).is_err());
        assert!(sample_args(&json!({ "steps": 20, "cfg": 99 })).is_err());
        let e = sample_args(&json!({ "steps": 0, "cfg": 0 })).unwrap_err();
        assert!(e.contains("步数"), "报错要指名是哪一项：{e}");
    }
}
