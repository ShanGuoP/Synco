//! api 层共用：响应形状、请求体读取、上限常量、导入落盘。

use crate::error::{AppError, Result};
use crate::repo::images as rimg;
use crate::state::Shared;
use crate::util;
use axum::body::Bytes;
use axum::http::{header::CONTENT_TYPE, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value};

/// 一批 base64 的体积按 80MB 收：前端按 8 张一批上传，正常永远碰不到
pub const BODY_LIMIT: usize = 80 * 1024 * 1024;
/// 种子取 int32 正区间：面板滑杆与直输框夹在同一上限，回填历史参数才不会被截断成别的种子
pub const SEED_MAX: i64 = 2_147_483_647;

pub(crate) fn j(code: u16, body: Value) -> Response {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, [(CONTENT_TYPE, "application/json; charset=utf-8")], body.to_string()).into_response()
}
pub(crate) fn ok(body: Value) -> Response {
    j(200, body)
}
pub(crate) fn err(code: u16, msg: &str) -> Response {
    let mut m = Map::new();
    m.insert("error".into(), Value::String(msg.to_string()));
    j(code, Value::Object(m))
}
pub(crate) fn bad(msg: impl Into<String>) -> Response {
    err(400, &msg.into())
}

/// 读 body：空 body 当 `{}`（与 Node 的 `JSON.parse(x || '{}')` 一致）。
/// 非法 JSON 这里回 400，Node 那边是冒到顶层回 500 —— 前端永远不会发这种东西，
/// 但 500 会把"你发的 body 有问题"报成"服务坏了"，所以这是刻意的偏离。
pub async fn body_of(raw: Bytes) -> Result<Value> {
    if raw.len() > BODY_LIMIT {
        return Err(AppError::Status {
            code: 413,
            msg: format!("一次传的图片太大（已收 {} MB，上限 {} MB）", raw.len() / 1048576, BODY_LIMIT >> 20),
        });
    }
    let text = String::from_utf8_lossy(&raw);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str::<Value>(trimmed).map_err(|e| AppError::bad(format!("请求体不是合法 JSON：{e}")))
}

pub(crate) fn path_id(s: &str) -> Result<i64> {
    s.parse::<i64>().map_err(|_| AppError::bad("id 不是数字"))
}

/// 导入一张图：落盘 + 建行，返回新 id
pub(crate) fn save_image_file(ctx: &Shared, pid: i64, f: &Value) -> Result<i64> {
    let dir = ctx.data.join("projects").join(pid.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败：{e}"))?;
    let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("photo.jpg");
    // # 会截断 /file/ 的 URL，% 会打断 decodeURIComponent，落盘前先去掉
    let safe = util::safe_name(name);
    if safe.is_empty() {
        return Err(AppError::bad("文件名是空的"));
    }
    let rel = util::rel_path(&[
        "projects".into(),
        pid.to_string(),
        format!("{}_{r4}_{safe}", util::now_ms(), r4 = util::r4()),
    ]);
    let b64 = f.get("b64").and_then(|v| v.as_str()).unwrap_or("");
    let bytes = util::decode_b64(b64);
    if bytes.is_empty() {
        return Err(AppError::bad("图片数据是空的"));
    }
    std::fs::write(ctx.data.join(&rel), &bytes).map_err(|e| format!("写入失败：{e}"))?;
    // 宽高是客户端报的，不能照单全收：0 会让后面任何一次编码直接 panic（release 是 abort，
    // 整个应用连带在飞的任务一起没），负数转 usize 还会变成巨值。真实尺寸由后台派生档那一步
    // 校正回库里（imagesvc::derive），这里只保证进来的值至少能算。
    let w = util::num_or(f.get("w"), 0.0);
    let h = util::num_or(f.get("h"), 0.0);
    if !(1.0..=30000.0).contains(&w) || !(1.0..=30000.0).contains(&h) {
        let _ = std::fs::remove_file(ctx.data.join(&rel));
        return Err(AppError::bad(format!("图片尺寸不对（报的是 {w}×{h}），换一张或重新导出再导")));
    }
    let id = rimg::insert(ctx, pid, &safe, &rel, w as i64, h as i64)?;
    // 导入这一刻原图就在盘上，顺手把 320/3072 两档切出来：列表与编辑器之后都只读小档
    if let Some(img) = rimg::by_id(ctx, id)? {
        crate::service::imagesvc::spawn_derive(ctx, img);
    }
    Ok(id)
}
