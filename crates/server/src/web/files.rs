//! 静态资源与取图：`/public/*` 走 no-store/immutable，`/file/*` 只放行图片扩展名，
//! 目录归属一律用相对路径判（前缀比较会放行 public_backup 这种同层兄弟目录）。
//! 缓存分级按文件名的命名约定判（imagesvc::cache_policy），ETag/304 与流式读取都在这层。

use crate::state::Shared;
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use std::path::Path;

const MIME: &[(&str, &str)] = &[
    (".html", "text/html; charset=utf-8"),
    (".js", "application/javascript; charset=utf-8"),
    (".css", "text/css; charset=utf-8"),
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".webp", "image/webp"),
    (".svg", "image/svg+xml"),
    (".ttf", "font/ttf"),
    (".otf", "font/otf"),
    (".woff2", "font/woff2"),
    (".avif", "image/avif"),
    (".bmp", "image/bmp"),
    (".json", "application/json; charset=utf-8"),
];

/// `/file/` 只发图片：不加白名单的话 /file/app.db 会把整个库端出去
const FILE_EXT: &[&str] = &[".png", ".jpg", ".jpeg", ".webp", ".avif", ".bmp"];

fn mime_of(ext: &str) -> &'static str {
    MIME.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m).unwrap_or("application/octet-stream")
}

fn json_err(code: u16, msg: &str) -> Response {
    (
        StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        format!("{{\"error\":{}}}", serde_json::Value::String(msg.to_string())),
    )
        .into_response()
}

/// decodeURIComponent 的等价物：%XX 按 UTF-8 字节收集，其余原样
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn ext_of(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e.to_ascii_lowercase())).unwrap_or_default()
}

/// 内容先读到手再写响应头：路径是目录（/public/）或被占用时 read 会抛，头发出去就收不回来了
fn bytes_response(content_type: &str, cache: Option<&str>, etag: Option<&str>, body: Vec<u8>) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, body.len().to_string());
    if let Some(c) = cache {
        builder = builder.header(header::CACHE_CONTROL, c);
    }
    if let Some(e) = etag {
        builder = builder.header(header::ETAG, e);
    }
    builder.body(Body::from(body)).unwrap_or_else(|_| json_err(500, "响应构造失败"))
}

/// 一次阻塞线程里把"在不在、多大、要不要读"判完：304 命中时连文件内容都不碰。
/// 返回 None = 读不到（目录、不存在、权限），由调用方出 404。
fn stat_and_read(data: &std::path::Path, rel: &str, inm: Option<&str>) -> Option<(String, Option<Vec<u8>>)> {
    // 词法判过还得再看一次真实路径：`fs::read` 跟随符号链接与 junction，只判字面 `..` 挡不住
    // 有人在 data/ 或 public/ 里放一个指向外面的链接。这里顺手拿到 canonical 路径，
    // 之后读的就是被判过的那一个，不存在"判 A 开 B"的中间窗口。
    let joined = crate::util::data_file(data, rel)?;
    let md = std::fs::metadata(&joined).ok()?;
    if !md.is_file() {
        return None;
    }
    // ETag = mtimeMs-size：和 Node 那边 fs.statSync 的两个数一一对应，改过一定变号，没改过就是同一个串
    let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    let etag = format!("\"{mtime:x}-{:x}\"", md.len());
    if inm == Some(etag.as_str()) {
        return Some((etag, None));
    }
    match std::fs::read(&joined) {
        Ok(b) => Some((etag, Some(b))),
        Err(e) => {
            eprintln!("  读 {} 失败：{e}", joined.display());
            None
        }
    }
}

async fn read_async(data: std::path::PathBuf, rel: String, inm: Option<String>) -> Option<(String, Option<Vec<u8>>)> {
    tokio::task::spawn_blocking(move || stat_and_read(&data, &rel, inm.as_deref())).await.ok().flatten()
}

pub async fn fallback(State(ctx): State<Shared>, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    let inm = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).map(str::to_string);
    if path == "/" || path == "/index.html" {
        return serve_static(&ctx, "index.html", inm).await;
    }
    if let Some(rel) = path.strip_prefix("/public/") {
        return serve_static(&ctx, &percent_decode(rel), inm).await;
    }
    if let Some(rel) = path.strip_prefix("/file/") {
        return serve_file(&ctx, &percent_decode(rel), inm).await;
    }
    json_err(404, "not found")
}

async fn serve_static(ctx: &Shared, rel: &str, inm: Option<String>) -> Response {
    if !crate::util::rel_ok(rel) || !crate::util::inside(&ctx.public, &ctx.public.join(rel)) {
        return json_err(403, "forbidden");
    }
    let ext = ext_of(Path::new(rel));
    // 中文字重文件 25MB，不缓存的话每次刷新都要重读一遍；样式和脚本不缓存是为了改完立刻见效
    let cache = match ext.as_str() {
        ".ttf" | ".otf" | ".woff2" => Some("public, max-age=31536000, immutable"),
        ".css" | ".js" | ".html" => Some("no-store"),
        _ => None,
    };
    // 盘上没有就用 exe 里内嵌的那一份——发布形态下界面是打进单文件的
    let got = read_async(ctx.public.clone(), rel.to_string(), inm.clone())
        .await
        .or_else(|| crate::web::assets::read(rel, inm.as_deref()));
    match got {
        // 静态资源里没有会原地覆写的，判新一律靠 no-store 那批的整页刷新
        Some((_, Some(bytes))) => bytes_response(mime_of(&ext), cache, None, bytes),
        Some((_, None)) => not_modified(cache, None),
        None => json_err(404, "not found"),
    }
}

async fn serve_file(ctx: &Shared, rel: &str, inm: Option<String>) -> Response {
    let joined = ctx.data.join(rel);
    let ext = ext_of(&joined);
    if !FILE_EXT.contains(&ext.as_str()) {
        return json_err(403, "only images");
    }
    if !crate::util::rel_ok(rel) || !crate::util::inside(&ctx.data, &joined) {
        return json_err(403, "forbidden");
    }
    let (cache, use_etag) = crate::service::imagesvc::cache_policy(rel);
    let (etag, body) = match read_async(ctx.data.clone(), rel.to_string(), if use_etag { inm } else { None }).await {
        Some(r) => r,
        None => return json_err(404, "not found"),
    };
    match body {
        Some(bytes) => bytes_response(mime_of(&ext), Some(cache), if use_etag { Some(&etag) } else { None }, bytes),
        None => not_modified(Some(cache), if use_etag { Some(&etag) } else { None }),
    }
}

fn not_modified(cache: Option<&str>, etag: Option<&str>) -> Response {
    let mut builder = Response::builder().status(StatusCode::NOT_MODIFIED);
    if let Some(c) = cache {
        builder = builder.header(header::CACHE_CONTROL, c);
    }
    // 有条件请求判新的那几条路（/file/ 的遮罩与派生档）必须把 ETag 原样带回 304，
    // 否则浏览器下一次不知道该发什么；/public/ 一律 no-store，传进来的是 None
    if let Some(e) = etag {
        builder = builder.header(header::ETAG, e);
    }
    builder.body(Body::empty()).unwrap_or_else(|_| json_err(500, "响应构造失败"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 百分号解码与扩展名归一() {
        assert_eq!(percent_decode("a%20b.png"), "a b.png");
        assert_eq!(percent_decode("%E4%B8%AD.png"), "中.png");
        assert_eq!(percent_decode("bad%-x"), "bad%-x");
        assert_eq!(ext_of(Path::new("x/y.JPG")), ".jpg");
        assert_eq!(ext_of(Path::new("noext")), "");
    }

    #[test]
    fn 白名单只放图片() {
        assert!(FILE_EXT.contains(&".png"));
        assert!(!FILE_EXT.contains(&".db"));
        assert_eq!(mime_of(".jpg"), "image/jpeg");
        assert_eq!(mime_of(".zzz"), "application/octet-stream");
    }
}
