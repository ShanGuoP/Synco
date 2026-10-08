//! 本地调整的路由薄层：解包、状态码、响应形状。链路的编排在 `service::adjust`。
//!
//! 所有解码/算链/编码/落盘都过 `util::blocking`——一次 proxy 档预览是几十毫秒到几百毫秒的
//! 纯 CPU，压在当前 worker 上整个桌面窗口会跟着僵（这是 0.2.1 那轮 P0 定下的契约）。

use super::common::{body_of, err, ok, path_id};
use crate::error::{AppError, Result};
use crate::models::entity::Image;
use crate::repo::{adjust as radj, images as rimg};
use crate::service::adjust;
use crate::state::Shared;
use crate::util;
use axum::body::Bytes;
use axum::extract::{Path as APath, State};
use axum::response::Response;
use photoedit_core::EditOps;
use serde_json::{json, Value};

/// body 里可以整份就是参数，也可以包一层 `{"ops":{...}}`——前端两种写法都出现过，
/// 契约里没钉死，这里就都认，但只认这两种。
fn ops_of_body(body: &Value) -> Value {
    match body.get("ops") {
        Some(v) if v.is_object() => v.clone(),
        _ => body.clone(),
    }
}

async fn photo_of(ctx: &Shared, id: i64) -> Result<Option<Image>> {
    let Some(img) = rimg::by_id(ctx, id)? else { return Ok(None) };
    if img.is_sketch() {
        return Err(AppError::bad("画稿不走本地调整：那是画布自己的链路"));
    }
    // 行在但文件不在：与其在解码处抛一句 ENOENT，不如在入口说清楚
    if !util::file_alive(&ctx.data, &Value::String(img.orig_path.clone())) {
        return Err(AppError::bad("原图文件已不在磁盘上（data/projects 被清过？）"));
    }
    Ok(Some(img))
}

/// 读参数（含预设表与盘上 LUT 名单）。无记录回全默认——前端滑杆要有个起点。
pub async fn adjust_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = photo_of(&ctx, iid).await? else { return Ok(err(404, "no image")) };
    let data = adjust::panel(&ctx, img.id)?;
    // 关键点没检出过时明确说 false，面板据此把一键塑形那排滑杆灰掉，
    // 而不是让用户拖一根"看着动了其实没用"的杆
    let has_face = util::blocking({
        let ctx = ctx.clone();
        move || Ok(json!({ "has_landmarks": crate::service::face::has_landmarks(&ctx, &img) }))
    })
    .await?;
    let mut out = data.as_object().cloned().unwrap_or_default();
    if let Some(v) = has_face.get("has_landmarks") {
        out.insert("has_landmarks".into(), v.clone());
    }
    Ok(ok(Value::Object(out)))
}

/// 存参数：夹逼 + 轨迹抽稀之后整条替换。回 `clamped` 让前端把越界的那几根杆说清楚。
pub async fn adjust_post(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let iid = path_id(&id)?;
    let Some(img) = photo_of(&ctx, iid).await? else { return Ok(err(404, "no image")) };
    let payload = ops_of_body(&body).to_string();
    let long_edge = img.w.max(img.h).max(1) as usize;
    let ctx2 = ctx.clone();
    let saved = util::blocking(move || -> Result<Value> {
        let (mut ops, mut rep) = EditOps::parse(&payload).map_err(AppError::bad)?;
        // LUT 名字要拿去拼路径：这里先给个准信，服务侧读文件时还有一道白名单
        if let Some(l) = ops.lut.as_ref() {
            if l.name.contains('/') || l.name.contains('\\') || l.name.contains("..") {
                return Err(AppError::bad("LUT 只能选 data/luts 里的那一层文件"));
            }
        }
        rep.extend(adjust::normalize(&mut ops, long_edge));
        radj::put(&ctx2, iid, &ops.to_json())?;
        Ok(json!({ "ok": true, "clamped": rep, "ops": ops }))
    })
    .await?;
    Ok(ok(saved))
}

/// 预览：proxy 档同步渲染，回 URL。同参数已有档直接复用（文件名里带参数指纹）。
pub async fn preview_post(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let iid = path_id(&id)?;
    let Some(img) = photo_of(&ctx, iid).await? else { return Ok(err(404, "no image")) };
    // 带 ops 就按这一次的参数渲（滑杆还没落库也能先看），不带就读库里那份
    let inline = body.get("ops").filter(|v| v.is_object()).map(|v| v.to_string());
    let ctx2 = ctx.clone();
    let out = util::blocking(move || -> Result<Value> {
        let ops = match inline.as_deref().map(EditOps::parse) {
            Some(Ok((mut o, _))) => {
                o.clamp();
                o
            }
            Some(Err(e)) => return Err(AppError::bad(e)),
            None => adjust::load_ops(&ctx2, img.id),
        };
        if ops.is_identity() {
            // 没参数就别白算一遍：把当前档位原样交回去
            let rel = img.proxy_path.clone().unwrap_or(img.orig_path.clone());
            return Ok(json!({ "preview_url": format!("/file/{rel}"), "w": img.w, "h": img.h, "ms": 0, "reused": true, "identity": true }));
        }
        let b = adjust::build_preview(&ctx2, &img, &ops).map_err(AppError::Fail)?;
        let mut v = adjust::preview_json(&b).as_object().cloned().unwrap_or_default();
        v.insert("identity".into(), json!(false));
        Ok(Value::Object(v))
    })
    .await?;
    Ok(ok(out))
}

/// 成图：原分辨率落 `_adjusted<指纹>.jpg`（immutable）。同步返回，实测超 3 秒再考虑挪进任务队列。
pub async fn render_post(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = photo_of(&ctx, iid).await? else { return Ok(err(404, "no image")) };
    let ctx2 = ctx.clone();
    let out = util::blocking(move || -> Result<Value> {
        let ops = adjust::load_ops(&ctx2, img.id);
        if ops.is_identity() {
            return Err(AppError::bad("还没动过任何滑杆，先调一下再落盘"));
        }
        let (b, thumb) = adjust::build_render(&ctx2, &img, &ops).map_err(AppError::Fail)?;
        Ok(json!({
            // 宽高报的是**渲出来那张**：几何段转过 90° 之后长宽会换边，
            // 报源图那对就会让前端把新图摆歪
            "url": format!("/file/{}", b.rel),
            "thumb_url": thumb.map(|t| format!("/file/{t}")),
            "ms": b.ms,
            "reused": b.reused,
            "w": b.w,
            "h": b.h,
        }))
    })
    .await?;
    Ok(ok(out))
}

/// 另存为新图：先落成图，再复制一份注册成新的图片行（起始参数为空），并补它自己的派生档。
pub async fn fork_post(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = photo_of(&ctx, iid).await? else { return Ok(err(404, "no image")) };
    let ctx2 = ctx.clone();
    let out = util::blocking(move || -> Result<Value> {
        let ops = adjust::load_ops(&ctx2, img.id);
        if ops.is_identity() {
            return Err(AppError::bad("这张图还没调整，另存为新图没有意义"));
        }
        // 成图的真实像素尺寸才是新行的 w/h：几何段转过 90° 之后长宽是会换边的
        let (b, _) = adjust::build_render(&ctx2, &img, &ops).map_err(AppError::Fail)?;
        let (nw, nh) = (b.w as i64, b.h as i64);
        let new_id = adjust::fork_from(&ctx2, &img, &b.rel, nw, nh)?;
        Ok(json!({ "image_id": new_id, "derived_from": img.id, "w": nw, "h": nh }))
    })
    .await?;
    // 新行的缩略/代理/瓦片按导入同一套后台补齐，接口不等它
    if let Some(nid) = out.get("image_id").and_then(|v| v.as_i64()) {
        if let Some(nimg) = rimg::by_id(&ctx, nid)? {
            crate::service::imagesvc::spawn_derive(&ctx, nimg.clone());
            tokio::spawn({
                let ctx = ctx.clone();
                async move {
                    let _ = crate::service::imagesvc::tiles_async(&ctx, nimg).await;
                }
            });
        }
    }
    Ok(ok(out))
}
