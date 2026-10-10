//! api 层共用：响应形状、请求体读取、上限常量、导入落盘。

use crate::error::{AppError, Result};
use crate::repo::images as rimg;
use crate::state::Shared;
use crate::util;
use axum::body::Bytes;
use axum::http::{header::CONTENT_TYPE, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};

/// 一批 base64 的体积按 80MB 收：前端按 8 张一批上传，正常永远碰不到
pub const BODY_LIMIT: usize = 80 * 1024 * 1024;
/// 种子取 int32 正区间：面板滑杆与直输框夹在同一上限，回填历史参数才不会被截断成别的种子
pub const SEED_MAX: i64 = 2_147_483_647;

/// 提交时实际下发的种子：面板（或回填）的种子 + 这张图的序号，两端都先夹进 int32 正区间。
///
/// 夹在最前面而不是加完再取模：库里躺着的旧种子可能是 2^48 那种当年放开的随机值，
/// 不夹就直接加会在 debug 构建溢出 panic、release 静默绕回，最后得到一个"合法但复现不了当时那张"的种子。
pub(crate) fn seed_for(raw: i64, img_id: i64) -> i64 {
    (raw.rem_euclid(SEED_MAX) + img_id.rem_euclid(SEED_MAX)).rem_euclid(SEED_MAX)
}

#[cfg(test)]
mod seed_tests {
    use super::seed_for;
    use super::SEED_MAX;

    #[test]
    fn 野种子先夹进区间再叠图片序号() {
        assert_eq!(seed_for(0, 7), 7);
        assert_eq!(seed_for(5, 0), 5);
        assert_eq!(seed_for(-1, 0), SEED_MAX - 1, "负数要走欧几里得取模，不能往负里偏");
        // 顶格再叠序号：绕回区间头部，而不是溢出
        assert_eq!(seed_for(SEED_MAX - 1, 5), 4);
        // 当年放开到 2^48 的老种子：夹进来仍然是个合法值（旧写法在这里 debug 构建直接 panic）
        let wild = seed_for(i64::MAX, i64::MAX);
        assert!((0..SEED_MAX).contains(&wild), "{wild} 跑出 int32 正区间了");
        assert_eq!(seed_for(i64::MAX, 0), seed_for(i64::MAX, 0));
    }
}

pub(crate) fn j(code: u16, body: Value) -> Response {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, [(CONTENT_TYPE, "application/json; charset=utf-8")], body.to_string()).into_response()
}
pub(crate) fn ok(body: Value) -> Response {
    j(200, body)
}

/// 直接返回 Response 的错误（handler 里 `return bad(...)` 那种）。
/// 一律经 `AppError` 出去，所以和冒到顶层的形状不会长岔。
pub(crate) fn coded(status: u16, code: &'static str, args: Value) -> Response {
    AppError::coded_args(status, code, args).into_response()
}
/// 状态码 + 字典钥匙（`srv.*`）。句子在语言包里，换语言不用重编服务端
pub(crate) fn err(status: u16, code: &'static str) -> Response {
    coded(status, code, Value::Null)
}
pub(crate) fn err_args(status: u16, code: &'static str, args: Value) -> Response {
    coded(status, code, args)
}
pub(crate) fn bad(code: &'static str) -> Response {
    err(400, code)
}
pub(crate) fn bad_args(code: &'static str, args: Value) -> Response {
    err_args(400, code, args)
}
/// 上游或运行时的原文：不是我们的文案，因此不进字典，界面拿到什么就显示什么
pub(crate) fn raw(status: u16, msg: impl Into<String>) -> Response {
    AppError::coded_msg(status, msg.into()).into_response()
}
pub(crate) fn bad_raw(msg: impl Into<String>) -> Response {
    raw(400, msg)
}

/// 读 body：空 body 当 `{}`（与 Node 的 `JSON.parse(x || '{}')` 一致）。
/// 非法 JSON 这里回 400，Node 那边是冒到顶层回 500 —— 前端永远不会发这种东西，
/// 但 500 会把"你发的 body 有问题"报成"服务坏了"，所以这是刻意的偏离。
pub async fn body_of(raw: Bytes) -> Result<Value> {
    if raw.len() > BODY_LIMIT {
        return Err(AppError::coded_args(
            413,
            "srv.common.bodyTooLarge",
            json!({ "got": raw.len() / 1048576, "cap": BODY_LIMIT >> 20 }),
        ));
    }
    let text = String::from_utf8_lossy(&raw);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str::<Value>(trimmed)
        .map_err(|e| AppError::bad_args("srv.common.badJson", json!({ "why": e.to_string() })))
}

pub(crate) fn path_id(s: &str) -> Result<i64> {
    s.parse::<i64>().map_err(|_| AppError::bad("srv.common.idNotNumber"))
}

/// 一批导入的张数上限。80MB 的 body 限的是**字节**不是张数：几千张 1×1 的小图照样塞得下，
/// 而每一张都是"落盘 + 建行 + 排一次派生"，一个请求就能把队列和磁盘 IO 排满。
/// 前端按 8 张一批发，正常永远碰不到。
pub const BATCH_MAX: usize = 200;

/// 张数超限的参数；没超限返 None。单独抽出来是因为建项目那条要**在建项目之前**判一次，
/// 否则会留下一个空项目挂在库里
pub(crate) fn batch_limit(files: &[Value]) -> Option<Value> {
    (files.len() > BATCH_MAX).then(|| json!({ "cap": BATCH_MAX, "got": files.len() }))
}

/// 一批文件逐个落盘建行。磁盘那段走阻塞池：一批 8 张 24MP 就是几十 MB 的顺序写，
/// 压在 handler 所在的 worker 上时整个窗口都会跟着僵住。
/// 逐张 `take` 而不是把整批 clone 一份——base64 载荷在这里已经是几十 MB，复制一次等于翻倍。
pub(crate) async fn save_batch(ctx: &Shared, pid: i64, files: &mut [Value]) -> Result<Vec<i64>> {
    if let Some(args) = batch_limit(files) {
        return Err(AppError::bad_args("srv.common.batchMax", args));
    }
    let mut ids = Vec::with_capacity(files.len());
    for f in files.iter_mut() {
        let one = std::mem::take(f);
        let ctx2 = ctx.clone();
        ids.push(util::blocking(move || save_image_file(&ctx2, pid, &one)).await?);
    }
    Ok(ids)
}

/// 导入一张图：落盘 + 建行，返回新 id
pub(crate) fn save_image_file(ctx: &Shared, pid: i64, f: &Value) -> Result<i64> {
    let dir = ctx.data.join("projects").join(pid.to_string());
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::fail_args("srv.common.mkdirFail", json!({ "msg": e.to_string() })))?;
    let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("photo.jpg");
    // # 会截断 /file/ 的 URL，% 会打断 decodeURIComponent，落盘前先去掉
    let safe = util::safe_name(name);
    if safe.is_empty() {
        return Err(AppError::bad("srv.common.nameEmpty"));
    }
    let rel = util::rel_path(&[
        "projects".into(),
        pid.to_string(),
        format!("{}_{r4}_{safe}", util::now_ms(), r4 = util::r4()),
    ]);
    let b64 = f.get("b64").and_then(|v| v.as_str()).unwrap_or("");
    let bytes = util::decode_b64(b64);
    if bytes.is_empty() {
        return Err(AppError::bad("srv.common.dataEmpty"));
    }
    std::fs::write(ctx.data.join(&rel), &bytes)
        .map_err(|e| AppError::fail_args("srv.common.writeFail", json!({ "msg": e.to_string() })))?;
    // 宽高是客户端报的，不能照单全收：0 会让后面任何一次编码直接 panic（release 是 abort，
    // 整个应用连带在飞的任务一起没），负数转 usize 还会变成巨值。真实尺寸由后台派生档那一步
    // 校正回库里（imagesvc::derive），这里只保证进来的值至少能算。
    let w = util::num_or(f.get("w"), 0.0);
    let h = util::num_or(f.get("h"), 0.0);
    if !(1.0..=30000.0).contains(&w) || !(1.0..=30000.0).contains(&h) {
        let _ = std::fs::remove_file(ctx.data.join(&rel));
        return Err(AppError::bad_args("srv.common.badDims", json!({ "w": w.to_string(), "h": h.to_string() })));
    }
    let id = rimg::insert(ctx, pid, &safe, &rel, w as i64, h as i64)?;
    // 导入这一刻原图就在盘上，顺手把 320/3072 两档切出来：列表与编辑器之后都只读小档
    if let Some(img) = rimg::by_id(ctx, id)? {
        crate::service::imagesvc::spawn_derive(ctx, img);
    }
    Ok(id)
}
