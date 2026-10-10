//! 统一错误 → JSON。
//!
//! 界面读的是 **`code`**（句子在语言包的 `srv.*` 节里，跟着语言切换走），
//! `error` 是同一把钥匙查中文那份字典拼出来的兜底，给不加载语言包的调用方看
//! （synco-tools 控制台、日志、harness）。两条路共用 `body()`，形状不会长岔。
//!
//! `error` 这个键名别改成 `message`：前端 api.js 与四套 harness 都按它读。

use crate::text;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value};
use std::borrow::Cow;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    /// 状态码 + 钥匙：400/404/409/413/500 都走这一格，句子在字典的 `srv.*` 节里。
    /// `code` 也允许是一句运行时原文（上游带回来的、库里存的），那种查不到钥匙就原样出去
    #[error("{code}")]
    Msg { status: u16, code: Cow<'static, str>, args: Map<String, Value> },
    /// 上游（ComfyUI / 云端）回来的错误，状态码由被调用方决定，原文照抄
    #[error("{code} {msg}")]
    Upstream { code: u16, msg: String },
    /// 还没交钥匙的老文案。R7 的账本只钉"已交完的文件"，所以这一格会随迁移清零。
    #[error("{0}")]
    Fail(String),
    /// 直接指定状态码、文案由上游或常量给（413 这类）
    #[error("{msg}")]
    Status { code: u16, msg: String },
}

impl From<String> for AppError {
    fn from(v: String) -> Self {
        AppError::Fail(v)
    }
}

/// `json!({...})` 传进来的参数取出来；不是对象就当没参数
fn args_of(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

impl AppError {
    /// 400：`code` 是字典里的键，例如 `srv.image.noMask`
    pub fn bad(code: &'static str) -> Self {
        AppError::Msg { status: 400, code: Cow::Borrowed(code), args: Map::new() }
    }

    /// 400 带参数：`AppError::bad_args("srv.adjust.clamped", json!({ "n": 2 }))`
    pub fn bad_args(code: &'static str, args: Value) -> Self {
        AppError::Msg { status: 400, code: Cow::Borrowed(code), args: args_of(args) }
    }

    /// 400 但句子是运行时拼出来的（上游原文、库里的旧文案）：没有钥匙，界面拿到什么显示什么
    pub fn bad_msg(msg: impl Into<String>) -> Self {
        AppError::coded_msg(400, msg.into())
    }

    /// 任意状态码 + 运行时原文
    pub fn coded_msg(status: u16, msg: String) -> Self {
        AppError::Msg { status, code: Cow::Owned(msg), args: Map::new() }
    }

    /// 任意状态码 + 钥匙
    pub fn coded(status: u16, code: &'static str) -> Self {
        AppError::Msg { status, code: Cow::Borrowed(code), args: Map::new() }
    }

    pub fn coded_args(status: u16, code: &'static str, args: Value) -> Self {
        AppError::Msg { status, code: Cow::Borrowed(code), args: args_of(args) }
    }

    /// 句子在字典里、细节是运行时原文（io / 解码器 / 上游那句）：细节只当 `{msg}` 参数塞进去。
    /// 把原文拼进中文句子会做出半译的文案——英文态读起来是"Write failed：建目录失败"。
    fn detailed(status: u16, code: &'static str, msg: impl std::fmt::Display) -> Self {
        AppError::Msg {
            status,
            code: Cow::Borrowed(code),
            args: args_of(serde_json::json!({ "msg": crate::util::clip(&msg.to_string(), 180) })),
        }
    }

    pub fn detail(code: &'static str, msg: impl std::fmt::Display) -> Self {
        Self::detailed(400, code, msg)
    }

    pub fn fail_detail(code: &'static str, msg: impl std::fmt::Display) -> Self {
        Self::detailed(500, code, msg)
    }

    /// 已经是"钥匙 + 参数"那一对（探活结论、批量拒绝理由那种写在 JSON 里的）直接升成错误，不再拼句子
    pub fn verdict(status: u16, code: String, args: Value) -> Self {
        AppError::Msg { status, code: Cow::Owned(code), args: args_of(args) }
    }

    /// `{msg}` 之外还要带别的参数（路径、尺寸）时用这条：句子照旧查字典，细节照旧只是参数
    pub fn detailed_args(status: u16, code: &'static str, extra: Value, msg: impl std::fmt::Display) -> Self {
        let mut args = args_of(extra);
        args.insert("msg".into(), Value::String(crate::util::clip(&msg.to_string(), 180)));
        AppError::Msg { status, code: Cow::Borrowed(code), args }
    }

    /// 服务端自己没兜住（500），但句子仍然要能翻
    pub fn fail(code: &'static str) -> Self {
        AppError::Msg { status: 500, code: Cow::Borrowed(code), args: Map::new() }
    }

    pub fn fail_args(code: &'static str, args: Value) -> Self {
        AppError::Msg { status: 500, code: Cow::Borrowed(code), args: args_of(args) }
    }

    /// 这把错误的钥匙与参数（只有交过钥匙的才有）。落库与嵌套传播都靠它——
    /// 把内层错误包成一句中文、再把钥匙塞进参数里，英文态就只剩半句人话。
    pub fn keyed(&self) -> Option<(&str, Map<String, Value>)> {
        match self {
            AppError::Msg { code, args, .. } => Some((code.as_ref(), args.clone())),
            _ => None,
        }
    }

    /// 给人看的那一句：有钥匙就查中文语言包，没钥匙就是原文。
    /// `Display` 在这种错误上是钥匙名（`{code}`），日志与控制台拿它当文案会打出一串 `srv.xxx`。
    pub fn text(&self) -> String {
        match self {
            AppError::Msg { code, args, .. } => text::text(code, Some(args)),
            other => other.to_string(),
        }
    }

    /// 摊成 `{reason: 钥匙, reason_args: {…}}` 那一格：批量提交的"这张为什么没排上"、
    /// 探活那种把结论写进 JSON 的接口都走这里。没交钥匙的（上游原文、库里的老文案）
    /// 就把原文当 reason——前端查不到字典会原样显示，不会空一格。
    pub fn reason(&self) -> (String, Option<Map<String, Value>>) {
        match self.keyed() {
            Some((code, args)) if args.is_empty() => (code.to_string(), None),
            Some((code, args)) => (code.to_string(), Some(args)),
            None => (self.text(), None),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            AppError::Io(_) | AppError::Sql(_) | AppError::Fail(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Status { code, .. } | AppError::Upstream { code, .. } => {
                StatusCode::from_u16(*code).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            AppError::Msg { status, .. } => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST),
        }
    }
}

/// 错误响应体的唯一形状：`{ error, code?, args? }`。
/// 只有交过钥匙的错误带 `code`——把中文原文塞进 code 会让"code 是给机器的钥匙"这件事失去意义。
/// `args` 为空就不写出去，免得每个 400 都拖一个空对象。
pub fn body(code: &str, args: Option<&Map<String, Value>>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("error".into(), Value::String(text::text(code, args)));
    m.insert("code".into(), Value::String(code.to_string()));
    if let Some(a) = args.filter(|a| !a.is_empty()) {
        m.insert("args".into(), Value::Object(a.clone()));
    }
    m
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let m = match &self {
            AppError::Msg { code, args, .. } => body(code, Some(args)),
            AppError::Upstream { msg, .. } => {
                let mut m = Map::new();
                m.insert("error".into(), Value::String(self.to_string()));
                // 上游原文另放一格：它不是我们的文案，不进字典也不参与翻译
                m.insert("detail".into(), Value::String(msg.clone()));
                m
            }
            _ => {
                let mut m = Map::new();
                m.insert("error".into(), Value::String(self.to_string()));
                m
            }
        };
        (
            self.status(),
            [(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")],
            Value::Object(m).to_string(),
        )
            .into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 两类外部错误都能进_apperror() {
        let sql: AppError = rusqlite::Error::QueryReturnedNoRows.into();
        assert!(matches!(sql, AppError::Sql(_)));
        let io: AppError = std::io::Error::other("磁盘满了").into();
        assert!(matches!(io, AppError::Io(_)));
        assert_eq!(io.to_string(), "磁盘满了");
    }

    #[test]
    fn 状态码与文案映射() {
        assert_eq!(AppError::bad("srv.测试").status(), StatusCode::BAD_REQUEST);
        assert_eq!(AppError::coded(404, "srv.测试").status(), StatusCode::NOT_FOUND);
        assert_eq!(AppError::fail("srv.测试").status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(AppError::Status { code: 404, msg: "没有这张图".into() }.status(), StatusCode::NOT_FOUND);
        assert_eq!(AppError::Fail("磁盘满了".into()).status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(AppError::Io(std::io::Error::other("x")).status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            AppError::Upstream { code: 413, msg: "body 太大".into() }.status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        // 100..=999 之外的码构造不出 StatusCode，退到 502；范围内的自定义码原样透传
        assert_eq!(AppError::Upstream { code: 7, msg: "怪".into() }.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(AppError::Upstream { code: 429, msg: "慢一点".into() }.status().as_u16(), 429);
    }

    #[test]
    fn 有钥匙的错误查不到字典就露钥匙本身() {
        // 字典里没有 srv.这条不存在 时，error 必须等于 code：前端因此能退回原样显示而不是空白
        let m = body("srv.这条不存在", None);
        assert_eq!(m["error"], json!("srv.这条不存在"));
        assert_eq!(m["code"], json!("srv.这条不存在"));
        assert!(m.get("args").is_none(), "没参数就不该多出空 args");
    }

    #[test]
    fn 参数跟着键一起出去() {
        let m = body("app.title", Some(&Map::from_iter([("n".to_string(), json!(3))])));
        assert_eq!(m["args"]["n"], json!(3), "字典查不到时也要把参数交给前端");
    }
}
