//! ComfyUI 客户端：上传 / 提交 / 轮询 / 下载 / 图谱构建。
//! 协议操作显式接收任务绑定地址，设置仅影响新任务。

use crate::backend::join;
use crate::error::AppError;
use crate::state::Ctx;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::time::Duration;

/// 种子取 int32 正区间：面板滑杆与直输框夹在同一上限，回填历史参数才不会被截断成别的种子
pub const SEED_MAX: i64 = 2_147_483_647;

/// 提交时实际下发的种子：面板（或回填）的种子 + 这张图的序号，两端都先夹进 int32 正区间。
///
/// 夹在最前面而不是加完再取模：库里躺着的旧种子可能是 2^48 那种当年放开的随机值，
/// 不夹就直接加会在 debug 构建溢出 panic、release 静默绕回，最后得到一个"合法但复现不了当时那张"的种子。
pub(crate) fn seed_for(raw: i64, img_id: i64) -> i64 {
    (raw.rem_euclid(SEED_MAX) + img_id.rem_euclid(SEED_MAX)).rem_euclid(SEED_MAX)
}

/// 本机这一路的采样参数：先判再进图。
///
/// 要挡的是"云端形状的 0 步 0 CFG"串回本机后被直接送进 KSampler——那不报错，
/// 只出一张没人看得出问题的图。跳过并给理由，比悄悄跑一张废图诚实。
pub fn sample_args(settings: &Value) -> crate::error::Result<(i64, f64)> {
    let steps = crate::util::number_of(settings.get("steps")).ok_or_else(|| AppError::bad("srv.comfy.noSteps"))?;
    let cfg = crate::util::number_of(settings.get("cfg")).ok_or_else(|| AppError::bad("srv.comfy.noCfg"))?;
    if !steps.is_finite() || !(1.0..=200.0).contains(&steps) {
        return Err(AppError::bad_args("srv.comfy.stepsBad", json!({ "n": steps.to_string() })));
    }
    if !cfg.is_finite() || !(0.0..=30.0).contains(&cfg) {
        return Err(AppError::bad_args("srv.comfy.cfgBad", json!({ "n": cfg.to_string() })));
    }
    Ok((steps.round() as i64, cfg))
}

/// 每个接口都可能被 ComfyUI 挂住（模型装载中、显存回收中），没有超时的请求会把这条 HTTP 永久吊着
/// 新任务必须有后端身份；历史未绑定任务不能猜测当前设置继续执行。
pub fn task_base(settings: &Value) -> crate::error::Result<String> {
    let base = settings.get("comfy_url").and_then(Value::as_str)
        .ok_or_else(|| AppError::fail("srv.queue.specMissing"))?;
    crate::backend::norm_url(base)
}

pub const T_UPLOAD: u64 = 60_000;
pub const T_PROMPT: u64 = 20_000;
pub const T_POLL: u64 = 10_000;
pub const T_FILE: u64 = 120_000;

/// ComfyUI 出错时给的是 HTML 页，直接当 JSON 解析抛 SyntaxError 会把真正的原因吞掉
async fn fetch_json(req: reqwest::RequestBuilder, url_for_msg: &str, ms: u64) -> crate::error::Result<Value> {
    let r = req.timeout(Duration::from_millis(ms)).send().await.map_err(|e| send_err(url_for_msg, e))?;
    let status = r.status().as_u16();
    let text = r.text().await.map_err(|e| send_err(url_for_msg, e))?;
    if !(100..=299).contains(&status) {
        let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let flat: String = flat.chars().take(180).collect();
        return Err(AppError::bad_args(
            "srv.comfy.http",
            json!({ "code": status, "detail": if flat.is_empty() { url_for_msg.to_string() } else { flat } }),
        ));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|_| AppError::bad_args("srv.comfy.notJson", json!({ "code": status, "detail": text.chars().take(180).collect::<String>() })))
}

/// 传输层失败：种类在这里判、句子在字典里，调用方原样往外传。
/// 别再包一句中文——那会做出"连不上 ComfyUI：Cannot reach ComfyUI"这种半译。
/// 细节优先取错误链上的下一层（连接被拒时露出 io 层的 os error）：reqwest 的 Display
/// 会把完整 URL 带进来，而链上没第二层时才退回它——URL 本来就在 `{url}` 这一格里。
fn send_err(url: &str, e: reqwest::Error) -> AppError {
    let code = if e.is_timeout() {
        "srv.comfy.timeout"
    } else if e.is_connect() {
        "srv.comfy.connect"
    } else {
        "srv.comfy.send"
    };
    AppError::detailed_args(502, code, json!({ "url": url.to_string() }), crate::util::reqwest_detail(&e))
}

/// 上传一张图，返回 ComfyUI 侧的引用名（可能带子目录）
pub async fn upload(ctx: &Ctx, base: &str, buf: Vec<u8>, filename: &str) -> crate::error::Result<String> {
    let url = join(base, "/upload/image");
    let form = reqwest::multipart::Form::new().part(
        "image",
        reqwest::multipart::Part::bytes(buf)
            .file_name(filename.to_string())
            .mime_str("image/png")
            .expect("mime 是常量"),
    ).text("overwrite", "true");
    let j = fetch_json(ctx.http.post(&url).multipart(form), &url, T_UPLOAD).await?;
    Ok(match (j.get("subfolder").and_then(|v| v.as_str()), j.get("name").and_then(|v| v.as_str())) {
        (Some(sub), Some(name)) if !sub.is_empty() => format!("{sub}/{name}"),
        (_, Some(name)) => name.to_string(),
        _ => String::new(),
    })
}

pub async fn post_json(ctx: &Ctx, base: &str, p: &str, obj: Value) -> crate::error::Result<Value> {
    let url = join(base, p);
    fetch_json(ctx.http.post(&url).json(&obj), &url, T_PROMPT).await
}

/// 把 ComfyUI 的产物写到 DATA 下的绝对路径
pub async fn download_to(ctx: &Ctx, url: &str, dest: &std::path::Path) -> crate::error::Result<()> {
    let r = ctx.http.get(url).timeout(Duration::from_millis(T_FILE)).send().await.map_err(|e| send_err(url, e))?;
    if !r.status().is_success() {
        return Err(AppError::bad_args("srv.comfy.download", json!({ "code": r.status().as_u16() })));
    }
    let bytes = r.bytes().await.map_err(|e| send_err(url, e))?;
    // 一张 20–33MB 的成图落盘是几百毫秒的同步写，按在 worker 上整个窗口会跟着一顿
    let dest = dest.to_path_buf();
    let lease = ctx.work().enter().await;
    crate::util::blocking(move || {
        let _lease = lease;
        std::fs::write(&dest, &bytes).map_err(|e| {
            AppError::detailed_args(500, "srv.image.slotWrite", json!({ "path": dest.display().to_string() }), e)
        })
    })
    .await
}

/// 队列里还活着的 prompt_id：服务重启时用它认哪些 running 记录已经是僵尸
pub async fn live_prompts(ctx: &Ctx, base: &str) -> crate::error::Result<HashSet<String>> {
    let url = join(base, "/queue");
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
    Unreachable(AppError),
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

pub async fn check_comfy(ctx: &Ctx, base: &str, pid: &str, outs: &Outputs) -> Check {
    let hist_url = join(&base, &format!("/history/{pid}"));
    let h = match fetch_json(ctx.http.get(&hist_url), &hist_url, T_POLL).await {
        Ok(v) => v,
        Err(e) => return Check::Unreachable(e),
    };
    if h.get(pid).is_none() {
        /* history 里没有这条有两种完全不同的解释：还在队列里没跑到，或者已经被 ComfyUI 忘掉
           （重启、点"清空历史"、历史条数把它挤出去）。只看 history 就回 running，工坊会替它转圈到天荒地老。 */
        let live = match live_prompts(ctx, base).await {
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
pub async fn interrupt(ctx: &Ctx, base: &str, prompt_id: &str) -> Vec<String> {
    let mut errs = Vec::new();
    let queue_url = join(base, "/queue");
    match fetch_json(ctx.http.get(&queue_url), &queue_url, T_POLL).await {
        Ok(queue) => {
            let running = queue.get("queue_running").and_then(Value::as_array)
                .map(|rows| rows.iter().any(|row| row.get(1).and_then(Value::as_str) == Some(prompt_id))).unwrap_or(false);
            // /interrupt 是后端全局动作，排队中的目标不能中断别人的当前任务。
            if running {
                if let Err(e) = post_json(ctx, base, "/interrupt", Value::Object(Map::new())).await { errs.push(e.text()); }
            }
        }
        Err(e) => errs.push(e.text()),
    }
    let pid = prompt_id.to_string();
    let url = join(base, "/queue");
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
            if let Err(e) = post_json(ctx, base, "/queue", serde_json::json!({ "queue_remove": [pid] })).await {
                errs.push(e.text());
            }
        }
    }
    errs
}


mod graph;
pub use graph::{build_graph, inject_workflow};
#[cfg(test)]
mod tests;
