//! 服务端要往外说的句子，一条都不写在这里。
//!
//! 文案在 `public/locales/{zh,en}.json` 的 `srv.*` 节里，界面按当前语言自己查；
//! 这里只负责把 `code` 拼回一句中文，给**非浏览器**的调用方看（synco-tools 的控制台、
//! 日志、harness）——它们不加载语言包，看不懂 code。
//! 所以错误响应里 `error` 是中文兜底、`code` 才是给界面的钥匙，两份文案不打架。
//!
//! `Public` 从 `web/assets.rs` 挪到这里：那个 derive 会把整个 `public/` 嵌进二进制，
//! 一处只能有一份，界面取文件与文案取句子共用它。

use rust_embed::RustEmbed;
use serde_json::{Map, Value};
use std::sync::OnceLock;

#[derive(RustEmbed)]
#[folder = "../../public"]
pub struct Public;

/// 中文那份字典，只解一次。读不到就返回 None——错误路径不该因为查字典再失败一次。
fn dict() -> Option<&'static Map<String, Value>> {
    static CACHE: OnceLock<Option<Map<String, Value>>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            Public::get("locales/zh.json")
                .and_then(|f| serde_json::from_slice::<Value>(&f.data).ok())
                .and_then(|v| v.as_object().cloned())
        })
        .as_ref()
}

/// 点分键走到那条字符串。查不到返回 None，调用方决定是露 code 还是露原句
fn leaf(code: &str) -> Option<&'static str> {
    let mut segs = code.split('.');
    let mut cur: &Value = dict()?.get(segs.next()?)?;
    for seg in segs {
        cur = cur.as_object()?.get(seg)?;
    }
    cur.as_str()
}

/// `srv.queue.noMask` 这种键取一句，`{n}` 之类的占位按 args 填。
/// 查不到就把 code 原样交出去：宁可看见一个陌生的键名，也不要一句假装成功的空话。
pub fn text(code: &str, args: Option<&Map<String, Value>>) -> String {
    match leaf(code) {
        None => code.to_string(),
        Some(raw) => fill(raw, args),
    }
}

/// 字典里有这条吗：给自检与测试用，免得 code 漂了没人知道
pub fn known(code: &str) -> bool {
    leaf(code).is_some()
}

/// `{code, args}` 那种结构化理由取一句给人看（控制台、日志、harness 都不加载语言包）。
/// 不是这个形状就回 None，调用方自己决定兜什么。
pub fn render(v: &Value) -> Option<String> {
    let code = v.get("code")?.as_str()?;
    Some(text(code, v.get("args").and_then(|a| a.as_object())))
}

fn fill(raw: &str, args: Option<&Map<String, Value>>) -> String {
    let Some(args) = args.filter(|a| !a.is_empty()) else { return raw.to_string() };
    let mut out = String::with_capacity(raw.len() + 16);
    let mut rest = raw;
    while let Some(i) = rest.find('{') {
        let Some(j) = rest[i..].find('}') else { break };
        let key = &rest[i + 1..i + j];
        out.push_str(&rest[..i]);
        match args.get(key) {
            // 参数没给就把占位留着：宁可显示 {ms}，也不要"成图（ ）"那种装完成的样子
            Some(Value::String(s)) => out.push_str(s),
            Some(v @ (Value::Object(_) | Value::Array(_))) => out.push_str(&loose(v)),
            Some(v) => out.push_str(&v.to_string()),
            None => out.push_str(&rest[i..i + j + 1]),
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out
}

/// 参数是"一簇理由"（数组）或"一条结构化理由"（`{code, args}`）时的写法。
/// 与前端 `argText` 是同一条约定：核心层（photoedit-core 那种不知道语言包在哪的）只交钥匙，
/// 句子在这一处才出现——不然英文界面上会冒出半句中文。
fn loose(v: &Value) -> String {
    match v {
        Value::Array(a) => a.iter().map(loose).collect::<Vec<_>>().join(&text("settings.sepList", None)),
        Value::Object(m) => match m.get("code").and_then(|c| c.as_str()) {
            Some(code) => text(code, m.get("args").and_then(|a| a.as_object())),
            None => v.to_string(),
        },
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 字典里的句子取得到() {
        // 这条同时是"locales 目录进了资源"的凭据：取不到就说明打包或路径出了问题
        assert!(known("app.title"), "字典里该有 app.title");
        assert_eq!(text("不存在的键名", None), "不存在的键名", "查不到就原样回 code，别编一句");
        assert!(!known("app.没有这个键"));
    }

    #[test]
    fn 占位按参数填_没给的留着() {
        let mut a = Map::new();
        a.insert("n".into(), json!(3));
        assert_eq!(fill("已夹逼 {n} 项", Some(&a)), "已夹逼 3 项");
        assert_eq!(fill("成图 {ms} ms", Some(&a)), "成图 {ms} ms", "缺的参数不该被吞成空白");
        assert_eq!(fill("原样", None), "原样");
        assert_eq!(fill("尾巴 {n}", Some(&a)), "尾巴 3");
    }

    /// 核心层（photoedit-core 那种不知道语言包在哪的）只交钥匙，句子在这一层才出现
    #[test]
    fn 参数里的结构化理由跟着一起翻() {
        let a = Map::from_iter([("msg".to_string(), json!({ "code": "lut.noSize" }))]);
        assert_eq!(
            text("srv.adjust.lutParse", Some(&a)),
            "这个 LUT 读不懂：没有 LUT_1D_SIZE / LUT_3D_SIZE 声明"
        );
        // 一簇理由（工作流校验那种一条一个的清单）按本语言的顿号接起来
        let b = Map::from_iter([(
            "errors".to_string(),
            json!([{ "code": "srv.wf.cycle" }, { "code": "srv.wf.noPrompt" }]),
        )]);
        let s = text("srv.submit.wfCannotTakeover", Some(&b));
        assert!(s.contains('、'), "{s} 该用本语言的顿号");
        assert!(s.contains("这张图里有环") && s.contains("采样器没接提示词编码"), "{s}");
        // 带参数的子句也要能嵌：行号从 args 里来
        let c = Map::from_iter([(
            "msg".to_string(),
            json!({ "code": "lut.sizeRange", "args": { "line": 7, "value": 512 } }),
        )]);
        assert_eq!(text("srv.adjust.lutParse", Some(&c)), "这个 LUT 读不懂：第 7 行的尺寸 512 超出 2..256");
    }
}
