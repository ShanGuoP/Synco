//! 回环守卫。
//! 只绑 127.0.0.1 挡不住浏览器里的跨站请求：同源策略管读不管发，所以带 Origin 的一律要同源，
//! Host 也必须是本机地址。GET 常常不带 Origin（同源的 fetch 就不带），那条看 Sec-Fetch-Site。

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
    if let Some(origin) = headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok()) {
        match origin_host(origin) {
            None => return Err("bad origin"),
            Some(oh) if oh != host => return Err("forbidden origin"),
            _ => {}
        }
    }
    if method == Method::GET || method == Method::HEAD {
        /* 跨站页面驱动 GET 时不带 Origin，但它一定带 Sec-Fetch-Site（Chromium 89+/Firefox 90+ 起，
           `<img>`、`fetch`、导航都算）。写操作已经从 GET 上摘干净了，剩下的"补派生档、切瓦片"
           这类磁盘活还是要有人挡。
           非浏览器客户端（curl、tools 目录下那些自检 harness）不送这个头 → 原样放行：
           这道守卫从来拦不住能自己发请求的本地程序，拦的是网页。 */
        let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()).unwrap_or("");
        if site.eq_ignore_ascii_case("cross-site") || site.eq_ignore_ascii_case("cross-origin") {
            return Err("forbidden site");
        }
        return Ok(());
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

    fn hdr(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(axum::http::header::HeaderName::from_static(k), v.parse().unwrap());
        }
        h
    }

    /// GET 这一侧只拦网页：跨站的 `<img>`/`fetch` 一定带 Sec-Fetch-Site，
    /// 而 curl 与 tools/*.js 那些 harness 什么都不带——放行它们是不打断自检的前提
    #[test]
    fn get_按_sec_fetch_site_判() {
        let host: &'static str = "host";
        let site: &'static str = "sec-fetch-site";
        assert_eq!(check(&hdr(&[(host, "127.0.0.1:7861"), (site, "cross-site")]), &Method::GET), Err("forbidden site"));
        assert_eq!(check(&hdr(&[(host, "127.0.0.1:7861"), (site, "cross-origin")]), &Method::GET), Err("forbidden site"));
        for ok_site in ["same-origin", "same-site", "none", ""] {
            assert_eq!(check(&hdr(&[(host, "127.0.0.1:7861"), (site, ok_site)]), &Method::GET), Ok(()), "{ok_site}");
        }
        assert_eq!(check(&hdr(&[(host, "127.0.0.1:7861")]), &Method::GET), Ok(()));
        // 带了 Origin 就必须同源，GET 也一样
        assert_eq!(
            check(&hdr(&[(host, "127.0.0.1:7861"), ("origin", "http://evil.example")]), &Method::GET),
            Err("forbidden origin")
        );
    }

    /// 写操作的规矩没变：本地进程不带 Origin 也能写（这是拍过的板，不是漏判）
    #[test]
    fn 写操作照旧只验_origin_与_content_type() {
        let host: &'static str = "host";
        assert_eq!(check(&hdr(&[(host, "127.0.0.1:7861")]), &Method::POST), Err("json only"));
        assert_eq!(
            check(&hdr(&[(host, "127.0.0.1:7861"), ("content-type", "application/json")]), &Method::POST),
            Ok(())
        );
        assert_eq!(
            check(&hdr(&[(host, "127.0.0.1:7861"), ("sec-fetch-site", "cross-site")]), &Method::DELETE),
            Ok(())
        );
    }
}
