//! ComfyUI 目录体检与一键配置脚本生成。
//! 只生成脚本、不代下载：23 GB 的权重写进别人磁盘这种事，交回给用户双击执行。

use crate::backend::join;
use crate::service::workflow as cfg;
use crate::repo::settings::{get as get_setting, put as put_setting};
use crate::state::Ctx;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PORTABLE_URL: &str =
    "https://github.com/comfyanonymous/ComfyUI_windows_portable/releases/latest/download/ComfyUI_windows_portable_nvidia.7z";

/// 这条管线真正用到的第三方节点包：buildGraph 自己拼图，工作流 JSON 里的 rgthree/Note 都不需要
pub const PACKS: [(&str, &str, &str, &[&str]); 2] = [
    (
        "ComfyUI-Inpaint-CropAndStitch",
        "https://github.com/lllcho/ComfyUI-Inpaint-CropAndStitch",
        "setup.pack.cropStitch",
        &["InpaintCropImproved", "InpaintStitchImproved"],
    ),
    ("ComfyUI-KJNodes", "https://github.com/kijai/ComfyUI-KJNodes", "setup.pack.kjNodes", &["DrawMaskOnImage"]),
];

/// 权重槽位：目录用 ComfyUI 新版约定，本机实测一致
const SLOTS: [(&str, &str, &str); 3] = [
    ("unet", "diffusion_models", "setup.slot.unet"),
    ("clip", "text_encoders", "setup.slot.clip"),
    ("vae", "vae", "VAE"),
];

fn is_file(p: &Path) -> bool {
    p.is_file()
}
fn is_dir(p: &Path) -> bool {
    p.is_dir()
}
fn size_of(p: &Path) -> u64 {
    p.metadata().map(|m| m.len()).unwrap_or(0)
}

/// 默认从工作流路径反推便携包根目录（.../<root>/ComfyUI/user/... 里的那段 <root>）
pub fn root_setting(ctx: &Ctx) -> String {
    if let Some(v) = get_setting(ctx, "comfy_root") {
        return v;
    }
    let wf = cfg::workflow_path(ctx).replace('\\', "/");
    match wf.find("/ComfyUI/") {
        Some(i) if i > 0 => wf[..i].to_string(),
        _ => String::new(),
    }
}

/// 根目录清洗：去首尾空白、成对引号、结尾分隔符。
/// 粘贴路径时带引号或尾斜杠很常见，而 `Test-Path 'D:\x ' / 'D:\x"'` 一律为假——
/// 症状正是脚本说"没找到 ComfyUI\main.py"，而文件明明在（打印出来的路径看不出差别）。
pub fn neat_root(p: &str) -> String {
    let mut s = p.trim().trim_matches(['"', '\'']).trim();
    while s.ends_with('/') || s.ends_with('\\') {
        s = s[..s.len() - 1].trim_end();
    }
    s.to_string()
}

pub fn set_root(ctx: &Ctx, p: &str) -> String {
    let _ = put_setting(ctx, "comfy_root", &neat_root(p));
    root_setting(ctx)
}

pub struct Model {
    pub slot: String,
    pub label: String,
    pub dir: String,
    pub optional: bool,
    pub rel: String,
    pub sha256: String,
    pub want: u64,
}

/// 权重清单：路径与指纹基准都来自 defaults.json（synco-tools 的 lock-weights 写回），换工作流自动跟着变
pub fn model_list(ctx: &Ctx) -> Vec<Model> {
    let cfg_v = cfg::get_cfg(ctx);
    let src = cfg::sources();
    let at = |k: &str, f: &str| -> String {
        src.get(k)
            .and_then(|v| v.get(f))
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .unwrap_or_default()
    };
    let at_num = |k: &str, f: &str| -> u64 {
        let s = at(k, f);
        s.parse().unwrap_or(0)
    };
    let mut out: Vec<Model> = Vec::new();
    for (key, dir, label) in SLOTS {
        let rel = cfg_v.get(key).and_then(|v| v.as_str()).unwrap_or("").replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        out.push(Model {
            slot: key.into(),
            label: label.into(),
            dir: dir.into(),
            optional: false,
            rel,
            sha256: at(key, "sha256"),
            want: at_num(key, "size"),
        });
    }
    if let Some(loras) = cfg_v.get("loras").and_then(|v| v.as_array()) {
        for (i, l) in loras.iter().enumerate() {
            let name = l.get("name").and_then(|v| v.as_str()).unwrap_or("").replace('\\', "/");
            if name.is_empty() {
                continue;
            }
            let key = format!("lora:{i}");
            let base = name.rsplit('/').next().unwrap_or(&name).to_string();
            out.push(Model {
                slot: key.clone(),
                label: format!("LoRA {base}"),
                dir: "loras".into(),
                optional: l.get("enabled").and_then(Value::as_bool) != Some(true),
                rel: name,
                sha256: at(&key, "sha256"),
                want: at_num(&key, "size"),
            });
        }
    }
    out.retain(|m| !m.rel.is_empty());
    out
}

/// 流式算本机指纹：22 GB 的权重不能整块读进内存
fn sha256_of(p: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 24];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 深度校验：逐个算本机指纹比基准，13GB 约几秒，所以只在用户主动点「深度校验」时跑
pub fn verify(ctx: &Ctx, root: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for m in model_list(ctx) {
        let p = Path::new(root).join("ComfyUI").join("models").join(&m.dir).join(&m.rel);
        let status = if !is_file(&p) {
            "missing"
        } else if m.sha256.is_empty() {
            "no-baseline"
        } else {
            match sha256_of(&p) {
                Ok(hex) if hex == m.sha256 => "ok",
                Ok(_) => "mismatch",
                Err(_) => "unknown",
            }
        };
        let mut o = Map::new();
        o.insert("label".into(), Value::String(m.label));
        o.insert("rel".into(), Value::String(m.rel));
        o.insert("status".into(), Value::String(status.into()));
        out.push(Value::Object(o));
    }
    out
}

/// 运行时有没有这个节点类，比看目录可靠（装了但依赖缺失是常事）
pub async fn probe_nodes(ctx: &Ctx, base: &str, classes: &[String]) -> Value {
    let mut out = Map::new();
    for c in classes {
        let url = format!("{}/object_info/{}", join(base, ""), path_escape(c));
        let v = ctx.http.get(&url).timeout(Duration::from_millis(2500)).send().await;
        match v {
            Ok(r) if r.status().is_success() => {
                let j = r.json::<Value>().await.unwrap_or(Value::Null);
                out.insert(c.clone(), Value::Bool(j.get(c).is_some()));
            }
            Ok(_) => {
                out.insert(c.clone(), Value::Bool(false));
            }
            Err(_) => {
                // null = 后端没起来，判不了
                out.insert(c.clone(), Value::Null);
            }
        }
    }
    Value::Object(out)
}

pub fn detect(ctx: &Ctx, root: &str) -> Value {
    let ui = Path::new(root).join("ComfyUI");
    let mut models = Vec::new();
    let mut total = 0u64;
    let mut missing = 0u64;
    for m in model_list(ctx) {
        let p = ui.join("models").join(&m.dir).join(&m.rel);
        let found = is_file(&p);
        let bytes = size_of(&p);
        if !m.optional {
            total += bytes;
            if !found {
                missing += bytes;
            }
        }
        let mut o = Map::new();
        o.insert("slot".into(), Value::String(m.slot));
        o.insert("label".into(), Value::String(m.label));
        o.insert("dir".into(), Value::String(m.dir));
        o.insert("big".into(), Value::Bool(true));
        o.insert("optional".into(), Value::Bool(m.optional));
        o.insert("rel".into(), Value::String(m.rel.clone()));
        o.insert("sha256".into(), Value::String(m.sha256));
        o.insert("want".into(), Value::from(m.want));
        // Node 那边是 path.join 的结果，Windows 上全无反斜杠；这里显式归一，免得混排两种分隔符
        o.insert("path".into(), Value::String(p.to_string_lossy().replace('/', "\\")));
        o.insert("found".into(), Value::Bool(found));
        o.insert("bytes".into(), Value::from(bytes));
        models.push(Value::Object(o));
    }
    let mut runtime = Map::new();
    runtime.insert("comfyui".into(), Value::Bool(is_dir(&ui)));
    runtime.insert("main_py".into(), Value::Bool(is_file(&ui.join("main.py"))));
    runtime.insert("python".into(), Value::Bool(is_file(&Path::new(root).join("python_embeded").join("python.exe"))));
    runtime.insert("qwen_nodes".into(), Value::Bool(is_file(&ui.join("comfy_extras").join("nodes_qwen.py"))));

    let packs: Vec<Value> = PACKS
        .iter()
        .map(|(dir, git, label, nodes)| {
            let mut o = Map::new();
            o.insert("dir".into(), Value::String((*dir).into()));
            o.insert("git".into(), Value::String((*git).into()));
            o.insert("label".into(), Value::String((*label).into()));
            o.insert("nodes".into(), Value::Array(nodes.iter().map(|n| Value::String((*n).into())).collect()));
            o.insert("installed".into(), Value::Bool(is_dir(&ui.join("custom_nodes").join(dir))));
            Value::Object(o)
        })
        .collect();

    let mut out = Map::new();
    out.insert("root".into(), Value::String(root.into()));
    out.insert("runtime".into(), Value::Object(runtime));
    out.insert("packs".into(), Value::Array(packs));
    out.insert("models".into(), Value::Array(models));
    out.insert("total_bytes".into(), Value::from(total));
    out.insert("missing_bytes".into(), Value::from(missing));
    Value::Object(out)
}

pub fn setup_dir(ctx: &Ctx) -> PathBuf {
    ctx.data.join("setup")
}

/// 生成 ASCII 引导 bat + UTF-8 BOM 的 ps1 + manifest：中文只进数据文件，不进 bat
pub fn write_script(ctx: &Ctx, root: &str, proxy: &str) -> Result<Value, String> {
    let d = detect(ctx, root);
    let models: Vec<Value> = d["models"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let to = Path::new(root)
                .join("ComfyUI")
                .join("models")
                .join(m["dir"].as_str().unwrap_or(""))
                .join(m["rel"].as_str().unwrap_or(""))
                .to_string_lossy()
                .replace('/', "\\");
            let min_bytes = m["want"].as_u64().filter(|x| *x > 0).or_else(|| m["bytes"].as_u64()).unwrap_or(0);
            let mut o = Map::new();
            o.insert("to".into(), Value::String(to));
            o.insert("label".into(), m["label"].clone());
            o.insert("optional".into(), m["optional"].clone());
            o.insert("sha256".into(), m["sha256"].clone());
            o.insert("min_bytes".into(), Value::from(min_bytes));
            Value::Object(o)
        })
        .collect();
    let mut manifest = Map::new();
    manifest.insert("comfy_root".into(), Value::String(root.into()));
    manifest.insert("portable_url".into(), Value::String(PORTABLE_URL.into()));
    manifest.insert("proxy".into(), Value::String(proxy.into()));
    manifest.insert(
        "packs".into(),
        Value::Array(PACKS.iter().map(|(dir, git, _, _)| serde_json::json!({"dir": dir, "git": git})).collect()),
    );
    manifest.insert("models".into(), Value::Array(models));

    let dir = setup_dir(ctx);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&Value::Object(manifest.clone())).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(dir.join("install_comfyui.ps1"), format!("\u{feff}{PS1}")).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("install_comfyui.bat"),
        "@echo off\r\npowershell -NoProfile -ExecutionPolicy Bypass -File \"%~dp0install_comfyui.ps1\"\r\nif errorlevel 1 pause\r\n",
    )
    .map_err(|e| e.to_string())?;

    let mut out = Map::new();
    out.insert("dir".into(), Value::String(dir.to_string_lossy().into_owned()));
    out.insert("bat".into(), Value::String(dir.join("install_comfyui.bat").to_string_lossy().into_owned()));
    out.insert("ps1".into(), Value::String(dir.join("install_comfyui.ps1").to_string_lossy().into_owned()));
    out.insert("manifest".into(), Value::Object(manifest));
    out.insert("detect".into(), d);
    Ok(Value::Object(out))
}

/// 脚本自己往 progress.json 写状态，工坊读它显示进度
pub fn progress(ctx: &Ctx) -> Value {
    match std::fs::read_to_string(setup_dir(ctx).join("progress.json")) {
        Ok(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

/// 检查脚本内容。只核对不下载：缺什么、放哪儿、指纹对不对，报给用户自己处理
/// 装机脚本的正文。它在磁盘上是一个 .ps1 文件，不是界面文案：
/// 翻它等于改用户要跑的脚本，所以整段放在旁边那个文件里，编进 exe 时逐字节照搬。
const PS1: &str = include_str!("install_comfyui.ps1");

/// /object_info/<class> 里类名可能带非常规字符，这里做 URL 路径段编码
pub fn path_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::neat_root;

    #[test]
    fn 根目录清洗认得出粘贴带来的东西() {
        assert_eq!(neat_root("D:/AI/qwen/ComfyUI_windows_portable"), "D:/AI/qwen/ComfyUI_windows_portable");
        assert_eq!(neat_root("  D:\\AI\\x\\  "), "D:\\AI\\x");
        assert_eq!(neat_root("\"D:\\AI\\x\""), "D:\\AI\\x");
        assert_eq!(neat_root("'D:/AI/x/'"), "D:/AI/x");
        assert_eq!(neat_root("D:/AI/x/"), "D:/AI/x");
        assert_eq!(neat_root("D:/AI/x\\\\"), "D:/AI/x");
        assert_eq!(neat_root(""), "");
    }
}
