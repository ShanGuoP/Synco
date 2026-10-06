//! 回环守卫。
//! 只绑 127.0.0.1 挡不住浏览器里的跨站请求：同源策略管读不管发，所以写操作要验 Origin，
//! Host 也必须是本机地址。

use axum::{
    extract::Request,
    http::{HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

fn is_loopback_host(host: &str) -> bool {
    let bare = match host.rsplit_once(':') {
        // "[::1]:7861" 这种要按最后一个冒号切，且切完还得带括号
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    };
    matches!(bare, "127.0.0.1" | "localhost" | "[::1]")
}

/// `new URL(origin).host` 的等价物：没有 scheme 就当作解析失败（JS 那边是抛异常 → bad origin）
fn origin_host(origin: &str) -> Option<&str> {
    let (_, after) = origin.split_once("://")?;
    let host = after.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn check(headers: &HeaderMap, method: &Method) -> Result<(), &'static str> {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !is_loopback_host(host) {
        return Err("forbidden host");
    }
    if method == Method::GET || method == Method::HEAD {
        return Ok(());
    }
    if let Some(origin) = headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok()) {
        match origin_host(origin) {
            None => return Err("bad origin"),
            Some(oh) if oh != host => return Err("forbidden origin"),
            _ => {}
        }
    }
    if method == Method::POST {
        let ct = headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !ct.to_ascii_lowercase().contains("application/json") {
            return Err("json only");
        }
    }
    Ok(())
}

pub async fn loopback_only(req: Request, next: Next) -> Response {
    if let Err(err) = check(req.headers(), req.method()) {
        return (
            StatusCode::FORBIDDEN,
            [(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")],
            format!("{{\"error\":\"{err}\"}}"),
        )
            .into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 回环判定() {
        assert!(is_loopback_host("127.0.0.1:7861"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("[::1]:80"));
        assert!(!is_loopback_host("evil.example"));
        assert!(!is_loopback_host("127.0.0.1.evil.example"));
        assert!(!is_loopback_host(""));
    }

    #[test]
    fn origin_host_取法与_url_一致() {
        assert_eq!(origin_host("http://127.0.0.1:7861/"), Some("127.0.0.1:7861"));
        assert_eq!(origin_host("http://127.0.0.1:7861/api/run"), Some("127.0.0.1:7861"));
        assert_eq!(origin_host("http://127.0.0.1/a#b"), Some("127.0.0.1"));
        assert_eq!(origin_host("garbage"), None);
    }
}
