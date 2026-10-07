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

/// 读回侧要问哪几个节点拿图。内置图那三个是写死的 17/19/20；
/// 接管提交时它们来自角色映射，所以**必须逐行记**——两张图用的节点号可以完全不同。
#[derive(Clone, Debug, Default)]
pub struct Outputs {
    pub final_id: Option<String>,
    pub crop_id: Option<String>,
    pub overlay_id: Option<String>,
}

const TAG_FINAL: &str = "final";
const TAG_CROP: &str = "crop";
const TAG_OVERLAY: &str = "maskoverlay";

impl Outputs {
    pub fn builtin() -> Outputs {
        Outputs { final_id: Some("17".into()), crop_id: Some("19".into()), overlay_id: Some("20".into()) }
    }

    pub fn from_roles(roles: &Map<String, Value>) -> Outputs {
        let get = |k: &str| roles.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Outputs { final_id: get("out_final"), crop_id: get("out_crop"), overlay_id: get("out_overlay") }
    }

    /// `(节点号, tag)`：没映射的输出不参与读回
    pub fn pairs(&self) -> Vec<(String, &'static str)> {
        let mut v = Vec::new();
        for (id, tag) in [(&self.final_id, TAG_FINAL), (&self.crop_id, TAG_CROP), (&self.overlay_id, TAG_OVERLAY)] {
            if let Some(id) = id {
                v.push((id.clone(), tag));
            }
        }
        v
    }

    /// 要求成套回来的那几个 tag：只包括这一行映射了的。
    /// 用户图里没有遮罩预览节点时，不能因为读不到 maskoverlay 就把一张成功成图判死。
    pub fn required_tags(&self) -> Vec<&'static str> {
        let mut v = vec![TAG_FINAL];
        if self.crop_id.is_some() {
            v.push(TAG_CROP);
        }
        if self.overlay_id.is_some() {
            v.push(TAG_OVERLAY);
        }
        v
    }

    /// 随这一行的参数快照落库（老行没这一项，读出来时按内置图解释）
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        for (k, v) in [("final", &self.final_id), ("crop", &self.crop_id), ("maskoverlay", &self.overlay_id)] {
            if let Some(v) = v {
                o.insert(k.into(), Value::String(v.clone()));
            }
        }
        Value::Object(o)
    }

    pub fn from_settings(settings: &Value) -> Outputs {
        let Some(o) = settings.get("workflow_out").filter(|v| v.is_object()) else {
            return Outputs::builtin();
        };
        let get = |k: &str| o.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Outputs { final_id: get("final"), crop_id: get("crop"), overlay_id: get("maskoverlay") }
    }
}

pub async fn check_comfy(ctx: &Ctx, pid: &str, outs: &Outputs) -> Check {
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
    // 输出节点号随这一行的记录走（内置图是 17/19/20，接管提交时是角色映射那三个）
    let mut images = Vec::new();
    let outs_map = entry.get("outputs").cloned().unwrap_or(Value::Null);
    for (nid, tag) in outs.pairs() {
        let list = outs_map
            .get(&nid)
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

/// 往**你那张图**里填本次提交的变量。动到的只有这几处：照片、遮罩、提示词、
/// 种子/步数/CFG、LoRA 开关。裁切参数、模型名、连线关系一概按文件里那样跑——
/// 这正是接管的意义：你在 ComfyUI 里看到什么，提交出去就是什么。
pub fn inject_workflow(graph: &Map<String, Value>, roles: &Map<String, Value>, photo: &str, mask: &str, settings: &Value, seed: i64) -> Result<Value, String> {
    let mut g = graph.clone();
    let mut set = |role: &str, key: &str, val: Value| -> Result<(), String> {
        let id = roles.get(role).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| format!("角色「{role}」没配上节点"))?;
        let node = g.get_mut(id).ok_or_else(|| format!("角色「{role}」指的节点 {id} 不在这张图里"))?;
        let inputs = node.as_object_mut().and_then(|n| n.get_mut("inputs")).and_then(|i| i.as_object_mut()).ok_or_else(|| format!("节点 {id} 没有 inputs"))?;
        if !inputs.contains_key(key) {
            return Err(format!("节点 {id} 没有 {key} 这个输入"));
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

#[cfg(test)]
mod tests {
    use super::{inject_workflow, sample_args, Outputs};
    use serde_json::{json, Map, Value};

    fn table(s: &str) -> Map<String, Value> {
        serde_json::from_str::<Value>(s).unwrap().as_object().cloned().unwrap()
    }

    /// 用户导出的图：节点号与内置图那套完全不同，两条 LoRA 串在 unet 之后
    const USER: &str = r#"{
      "21":{"class_type":"LoadImage","inputs":{"image":"占位.png"}},
      "22":{"class_type":"LoadImageMask","inputs":{"image":"占位m.png","channel":"red"}},
      "30":{"class_type":"UNETLoader","inputs":{"unet_name":"u.safetensors"}},
      "31":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["30",0],"lora_name":"A.safetensors","strength_model":0.9}},
      "32":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["31",0],"lora_name":"B.safetensors","strength_model":0.5}},
      "40":{"class_type":"InpaintCropImproved","inputs":{"image":["21",0],"mask":["22",0],"mask_expand_pixels":64,"device_mode":"cpu (compatible)"}},
      "41":{"class_type":"TextEncodeQwenImage21","inputs":{"clip":["42",0],"prompt":"文件里的正向","negative_prompt":"文件里的负面"}},
      "43":{"class_type":"KSampler","inputs":{"model":["32",0],"positive":["41",0],"seed":7,"steps":18,"cfg":2.5,"sampler_name":"euler","scheduler":"simple"}},
      "50":{"class_type":"InpaintStitchImproved","inputs":{"stitcher":["40",0],"inpainted_image":["43",0]}},
      "51":{"class_type":"SaveImage","inputs":{"images":["50",0],"filename_prefix":"Mine"}}
    }"#;

    const ROLES: &str = r#"{
      "load_image":"21","load_mask":"22","text_encode":"41","ksampler":"43",
      "crop":"40","stitch":"50","out_final":"51","out_crop":"52","out_overlay":"53"
    }"#;

    fn settings() -> Value {
        json!({
            "prompt": "把妆容修干净", "negative": "不要的东西", "steps": 26, "cfg": 3.5, "seed": 4242,
            "loras": [
                { "name": "A.safetensors", "strength": 0.7, "enabled": false },
                { "name": "B.safetensors", "strength": 1.0, "enabled": true }
            ]
        })
    }

    /// 接管提交只填这几处：其余（裁切参数、模型名、采样器选型）一概按用户文件里那样跑
    #[test]
    fn 注入只动该动的输入() {
        let g = table(USER);
        let roles = table(ROLES);
        let out = inject_workflow(&g, &roles, "myphoto.png", "mymask.png", &settings(), 991).unwrap();
        assert_eq!(out["21"]["inputs"]["image"], json!("myphoto.png"));
        assert_eq!(out["22"]["inputs"]["image"], json!("mymask.png"));
        assert_eq!(out["41"]["inputs"]["prompt"], json!("把妆容修干净"));
        assert_eq!(out["41"]["inputs"]["negative_prompt"], json!("不要的东西"));
        assert_eq!(out["43"]["inputs"]["seed"], json!(991), "种子由调用方算好（带图片偏移）");
        assert_eq!(out["43"]["inputs"]["steps"], json!(26));
        assert_eq!(out["43"]["inputs"]["cfg"], json!(3.5));
        // 没让改的东西原样留着
        assert_eq!(out["43"]["inputs"]["sampler_name"], json!("euler"));
        assert_eq!(out["40"]["inputs"]["mask_expand_pixels"], json!(64));
        assert_eq!(out["30"]["inputs"]["unet_name"], json!("u.safetensors"));
        // 传进去的表不能被改坏（下一次提交还要用同一份）
        assert_eq!(g["21"]["inputs"]["image"], json!("占位.png"));
    }

    /// 面板上关了 A：A 节点从图里摘掉，B 接到 unet 上。
    /// 只加 `enabled:false` 是不够的——老版本 ComfyUI 不认那个键，会照跑。
    #[test]
    fn 关掉的_lora_被摘掉且下游接回上游() {
        let g = table(USER);
        let roles = table(ROLES);
        let out = inject_workflow(&g, &roles, "p", "m", &settings(), 1).unwrap();
        assert!(out.get("31").is_none(), "关掉的 LoRA 节点不该留在图里");
        assert_eq!(out["32"]["inputs"]["model"], json!(["30", 0]), "B 要接回 unet");
        assert_eq!(out["32"]["inputs"]["strength_model"], json!(1.0));
        assert_eq!(out["43"]["inputs"]["model"], json!(["32", 0]));
    }

    #[test]
    fn 两条都关时采样器直接接_unet() {
        let mut s = settings();
        s["loras"][0]["enabled"] = json!(true);
        s["loras"][1]["enabled"] = json!(false);
        let out = inject_workflow(&table(USER), &table(ROLES), "p", "m", &s, 1).unwrap();
        assert_eq!(out["31"]["inputs"]["strength_model"], json!(0.7), "开着的按面板强度写");
        assert!(out.get("32").is_none());
        assert_eq!(out["43"]["inputs"]["model"], json!(["31", 0]));

        let mut s2 = settings();
        s2["loras"][0]["enabled"] = json!(false);
        s2["loras"][1]["enabled"] = json!(false);
        let out2 = inject_workflow(&table(USER), &table(ROLES), "p", "m", &s2, 1).unwrap();
        assert!(out2.get("31").is_none() && out2.get("32").is_none(), "两条都关就都不在图里");
        assert_eq!(out2["43"]["inputs"]["model"], json!(["30", 0]));
    }

    /// 面板里的 LoRA 清单与工作流对不上时（挂在 CLIP 上、或换了文件）：不碰它，
    /// 免得把一条 Synco 不认识的链给摘断
    #[test]
    fn 面板没有的_lora_不动() {
        let mut s = settings();
        s["loras"] = json!([{ "name": "根本不存在的.safetensors", "strength": 1, "enabled": false }]);
        let out = inject_workflow(&table(USER), &table(ROLES), "p", "m", &s, 1).unwrap();
        assert!(out.get("31").is_some() && out.get("32").is_some());
    }

    #[test]
    fn 角色指的节点不在图里就报错而不是硬填() {
        let g = table(USER);
        let mut roles = table(ROLES);
        roles.remove("load_mask");
        let e = inject_workflow(&g, &roles, "p", "m", &settings(), 1).unwrap_err();
        assert!(e.contains("load_mask"), "{e}");
        let roles2 = table(r#"{"load_image":"999","load_mask":"22","text_encode":"41","ksampler":"43"}"#);
        let e2 = inject_workflow(&g, &roles2, "p", "m", &settings(), 1).unwrap_err();
        assert!(e2.contains("999"), "{e2}");
    }

    /// 负面词为空时不清空文件里那句：用户在工作流里写好的负面提示词不该被界面抹掉
    #[test]
    fn 空负面词不覆盖文件里的那句() {
        let mut s = settings();
        s["negative"] = json!("");
        let out = inject_workflow(&table(USER), &table(ROLES), "p", "m", &s, 1).unwrap();
        assert_eq!(out["41"]["inputs"]["negative_prompt"], json!("文件里的负面"));
    }

    #[test]
    fn 输出节点号按行取_老行按内置解释() {
        assert_eq!(Outputs::builtin().pairs(), vec![("17".into(), "final"), ("19".into(), "crop"), ("20".into(), "maskoverlay")]);
        // 改造前写的行没有 workflow_out：还按 17/19/20 读
        let old = Outputs::from_settings(&json!({ "steps": 20 }));
        assert_eq!(old.final_id.as_deref(), Some("17"));
        assert_eq!(old.required_tags(), vec!["final", "crop", "maskoverlay"]);

        let row = Outputs::from_settings(&json!({ "workflow_out": { "final": "51", "crop": "52" } }));
        assert_eq!(row.final_id.as_deref(), Some("51"));
        assert_eq!(row.overlay_id, None, "这一行没有遮罩预览那一路");
        assert_eq!(row.required_tags(), vec!["final", "crop"], "没映射的输出不能拿来判失败");
        // 落盘 → 读回要对得上
        let back = Outputs::from_settings(&json!({ "workflow_out": row.to_json() }));
        assert_eq!(back.final_id, row.final_id);
        assert_eq!(back.crop_id, row.crop_id);
    }

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
