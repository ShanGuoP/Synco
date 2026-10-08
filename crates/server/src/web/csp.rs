//! 内容安全策略：界面所有字节都由本机服务发出，页面里没有一条跨源脚本，所以能收到很紧的一档。
//!
//! 这层主要是给桌面壳垫的纵深。壳开着 `withGlobalTauri`（`core/desktop.js` 读 `window.__TAURI__`），
//! 并给 `http://127.0.0.1:*` 这个远程域授了 copy-data-to / set-data-dir / restart-app / open-url：
//! 一旦某个 `html:` 插值漏了、把用户文本原样发进 DOM，没有 CSP 那就是整目录（含明文 key 与原片）
//! 被人复制走；有了 CSP，脚本只能在同源下跑，注入进来的那段什么都干不成。
//!
//! 逐字符核过的：前端没有内联 `<script>`（首帧那次上色挪到了 `js/core/theme-boot.js`），
//! 没有 `eval`/`new Function`，没有内联 `style=` 属性（只有 CSSOM 的 `element.style.x=`，那不受 CSP 管），
//! 没有跨源图片/字体/fetch（ComfyUI 与中转的地址都只在本机服务端用，浏览器不直接碰），
//! Worker 是同源的 `/public/js/core/maskEncode.worker.js`（由 `script-src 'self'` 放行）。

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};

/// 一次写死在这一处：接口与页面同源，不需要为任何一档开例外
pub const POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; \
object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

pub async fn apply(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut().insert(HeaderName::from_static("content-security-policy"), header());
    res
}

fn header() -> HeaderValue {
    // from_str 而不是 from_static：后者对非法字节直接 panic，这条常量的合法性交给下面的单测判
    HeaderValue::from_str(POLICY).unwrap_or_else(|_| HeaderValue::from_static("default-src 'none'"))
}

#[cfg(test)]
mod tests {
    use super::POLICY;

    #[test]
    fn 策略是一条合法的头部值() {
        assert!(axum::http::HeaderValue::from_str(POLICY).is_ok());
    }

    /// 收紧的这几档一条都不能掉：掉了就等于给壳里那四条命令开门
    #[test]
    fn 关键档位都在() {
        for need in [
            "default-src 'self'",
            "script-src 'self'",
            "style-src 'self'",
            "img-src 'self' data: blob:",
            "connect-src 'self'",
            "object-src 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(POLICY.contains(need), "缺 {need}");
        }
        assert!(!POLICY.contains("unsafe-eval") && !POLICY.contains("unsafe-inline"));
    }
}
