//! 统一错误 → JSON。响应体只有 `error` 一个键，前端 `api.js` 就是按这个读法写的，别改成 message。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    /// 参数不合法：400，文案会直接进前端的 toast
    #[error("{0}")]
    Bad(String),
    /// 上游（ComfyUI / 云端）回来的错误，状态码由被调用方决定
    #[error("{code} {msg}")]
    Upstream { code: u16, msg: String },
    /// handler 里冒到顶层的读写失败：与 Node 一样回 500，并把原文带出去
    #[error("{0}")]
    Fail(String),
    /// 直接指定状态码的错误（413 这类）
    #[error("{msg}")]
    Status { code: u16, msg: String },
}

impl From<String> for AppError {
    fn from(v: String) -> Self {
        AppError::Fail(v)
    }
}

impl AppError {
    pub fn bad(msg: impl Into<String>) -> Self {
        AppError::Bad(msg.into())
    }

    fn status(&self) -> StatusCode {
        match self {
            AppError::Io(_) | AppError::Sql(_) | AppError::Fail(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Bad(_) => StatusCode::BAD_REQUEST,
            AppError::Status { code, .. } => StatusCode::from_u16(*code).unwrap_or(StatusCode::BAD_GATEWAY),
            AppError::Upstream { code, .. } => StatusCode::from_u16(*code).unwrap_or(StatusCode::BAD_GATEWAY),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let mut body = serde_json::json!({ "error": self.to_string() });
        if let AppError::Upstream { msg, .. } = &self {
            body["detail"] = serde_json::Value::String(msg.clone());
        }
        (
            self.status(),
            [(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")],
            body.to_string(),
        )
            .into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(AppError::bad("遮罩不能为空").status(), StatusCode::BAD_REQUEST);
        assert_eq!(AppError::Status { code: 404, msg: "没有这张图".into() }.status(), StatusCode::NOT_FOUND);
        assert_eq!(AppError::Fail("磁盘满了".into()).status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(AppError::Io(std::io::Error::other("x")).status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            AppError::Upstream { code: 413, msg: "body 太大".into() }.status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(AppError::bad("遮罩不能为空").to_string(), "遮罩不能为空");
        // 100..=999 之外的码构造不出 StatusCode，退到 502；范围内的自定义码原样透传
        assert_eq!(
            AppError::Upstream { code: 7, msg: "怪".into() }.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(AppError::Upstream { code: 429, msg: "慢一点".into() }.status().as_u16(), 429);
    }
}
