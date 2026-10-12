//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{bad, body_of, err, ok, path_id};
use crate::error::{AppError, Result};
use crate::models::dto;
use crate::repo::{images as rimg, projects as rproj, results as rres};
use crate::service::{comfy, queue, reclaim};
use crate::state::Shared;
use crate::util;
use axum::body::Bytes;
use axum::extract::{Path as APath, Query, State};
use axum::response::Response;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// 结果集合：项目页「派生查看」的数据源。`?project_id=` 列整项目，`&image_id=` 收窄到一张。
/// 与 `/api/images/{id}` 的差别是这条不补派生档，也不把范围锁在 8 条。
pub async fn results_list(State(ctx): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Result<Response> {
    let num = |k: &str| q.get(k).and_then(|s| s.parse::<i64>().ok());
    let limit = num("limit").unwrap_or(400).clamp(1, 2000);
    let rows = match (num("project_id"), num("image_id")) {
        (_, Some(iid)) => rres::list_for_image(&ctx, iid, limit + 1)?,
        (Some(pid), None) => rres::list_for_project(&ctx, pid, limit + 1)?,
        _ => return Ok(bad("srv.submit.needIds")),
    };
    let truncated = rows.len() as i64 > limit;
    let out: Vec<Value> = rows
        .into_iter()
        .take(if truncated { limit as usize } else { usize::MAX })
        .map(|r| dto::result_json(&ctx, &r))
        .collect();
    Ok(ok(serde_json::json!({ "results": out, "truncated": truncated })))
}

pub async fn run_post(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let settings = body.get("settings").cloned().unwrap_or(Value::Object(Map::new()));
    let rerun_of = body.get("rerun_of").and_then(|v| v.as_i64());
    let comfy_base = match body.get("comfy_url").and_then(Value::as_str) {
        Some(base) => crate::backend::norm_url(base)?,
        None => crate::backend::active_url(&ctx),
    };
    let mut out: Vec<Value> = Vec::new();
    let ids: Vec<Value> = body.get("image_ids").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    // 逐张兜异常：整批一起抛的话，前面已经交给 ComfyUI 的任务会因为没返回 result_id 而没人轮询，
    // 库里只留下一排永远 running 的僵尸记录。
    for img in ids {
        let img_id = img.as_i64().unwrap_or(-1);
        match crate::service::localgen::run_one(&ctx, img_id, &settings, rerun_of, &comfy_base).await {
            Ok(v) => out.push(v),
            Err(e) => out.push(serde_json::json!({
                "image_id": img,
                "skipped": true,
                "reason": e.to_string().chars().take(200).collect::<String>()
            })),
        }
    }
    Ok(ok(serde_json::json!({ "results": out })))
}

/// 轮询读的是**当前状态**，推进由服务端那条推进器做（`service::reclaim::spawn_advancer`）。
/// 以前这条 GET 会顺带问 ComfyUI、下载成图、写库：网页上一个 `<img src="…/api/results/1">`
/// 就能驱动本机干这些活，所以现在读就是读。
pub async fn result_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    Ok(match rres::value_by_id(&ctx, path_id(&id)?)? {
        Some(v) => ok(dto::result_json(&ctx, &v)),
        None => err(404, "srv.common.noResult"),
    })
}

pub async fn interrupt_post(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let rid = path_id(&id)?;
    let Some(r) = rres::by_id(&ctx, rid)? else { return Ok(err(404, "srv.common.noResult")) };
    if !r.running() {
        return Ok(bad("srv.submit.settled"));
    }
    let local_base = if r.prompt_id.is_some() { Some(comfy::task_base(&r.settings())?) } else { None };
    match r.prompt_id {
        // 云端行没有 prompt_id：那次请求在对面跑着，撤不回来；标记一下让 worker 别采用结果
        None => {
            ctx.job_cancel(rid);
            ctx.mark_error(rid, "srv.result.cancelled", serde_json::Value::Null);
        }
        Some(prompt_id) => {
            let errs = comfy::interrupt(&ctx, local_base.as_deref().unwrap(), &prompt_id).await;
            let (code, args) = if errs.is_empty() {
                ("srv.submit.byHand", serde_json::Value::Null)
            } else {
                ("srv.submit.cancelNoNotify", json!({ "errs": errs }))
            };
            ctx.mark_error(rid, code, args);
        }
    }
    Ok(ok(rres::value_by_id(&ctx, rid)?.map(|v| dto::result_json(&ctx, &v)).unwrap_or(Value::Null)))
}

pub async fn result_delete(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let rid = path_id(&id)?;
    let Some(r) = rres::by_id(&ctx, rid)? else { return Ok(err(404, "srv.common.noResult")) };
    // 云端行先按超时判一次：判死了就允许删，别让"生成中"永久挡着垃圾桶
    if r.running() && !reclaim::judge_cloud(&ctx, rid) {
        return Ok(bad("srv.submit.stillRunning"));
    }
    // 先删行、后删文件：反过来一旦中间失败，库里就挂着指向空气的记录（卡片在、点开是空的）。
    // 清单出自 repo 那一份列名表——加了新列（窗口原图、遮罩快照）也不会在这条路上漏文件
    let rels = rres::paths_for_row(&ctx, rid)?;
    rres::delete(&ctx, rid)?;
    for rel in rels {
        if let Some(p) = util::data_file(&ctx.data, &rel) {
            let _ = std::fs::remove_file(p);
        }
    }
    Ok(ok(serde_json::json!({ "ok": true })))
}

/// 无损那一张的下载口。成图那一档现在存的是 q95（只当预览与对比），这一条按这一行的快照
/// 把当年那次缝合**重算**成 PNG；重算不成时给的是 q95 原件，并把文件名后缀换成 `.jpg`——
/// 下载气泡里那一处变化是用户一定会看到的提示，比在角落闪一句 toast 诚实。
pub async fn lossless_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let rid = path_id(&id)?;
    let Some(r) = rres::by_id(&ctx, rid)? else { return Ok(err(404, "srv.result.noResult")) };
    let ctx2 = ctx.clone();
    let row = r.clone();
    // 24MP 解码 + 缝合 + PNG 编码是秒级同步重活，一次都该待在阻塞池里
    let got = util::image_work(ctx.clone(), 0, move || queue::lossless_bytes(&ctx2, &row)).await?;
    let (name, _, _) = rimg::name_and_size(&ctx, r.image_id)?.unwrap_or(("photo".into(), 0, 0));
    // safe_name 去过 # 与控制字符：它们会把 URL 或 Content-Disposition 直接截断
    let stem = util::safe_name(&util::stem_of(&name));
    let ct: &'static str = if got.ext == "png" { "image/png" } else { "image/jpeg" };
    let mut resp = axum::response::Response::new(axum::body::Body::from(got.bytes));
    let h = resp.headers_mut();
    h.insert(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static(ct));
    let dis = format!("attachment; filename=\"{stem}_#{rid}.{ext}\"", ext = if got.ext == "png" { "png" } else { "jpg" });
    h.insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(&dis).unwrap_or_else(|_| axum::http::HeaderValue::from_static("attachment")),
    );
    // 给程序看的档位：前端 fetch 完可以据此说明拿到的是哪一档，直链下载时也看得到
    h.insert("X-Synco-Quality", axum::http::HeaderValue::from_static(if got.lossless { "lossless" } else { "jpeg-q95" }));
    if let Some(why) = got.why {
        h.insert("X-Synco-Quality-Reason", axum::http::HeaderValue::from_str(why).unwrap_or_else(|_| axum::http::HeaderValue::from_static("unknown")));
    }
    Ok(resp)
}

pub async fn fork_post(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let rid = path_id(&id)?;
    let Some(r) = rres::by_id(&ctx, rid)? else { return Ok(err(404, "srv.result.noResult")) };
    if r.final_path.as_deref().unwrap_or("").is_empty() {
        return Ok(bad("srv.submit.noCopyable"));
    }
    let (name, w, h) = rimg::name_and_size(&ctx, r.image_id)?.unwrap_or(("photo".into(), 0, 0));
    let stem = util::stem_of(&name);
    /* 派生出来的那张还要接着涂、接着提交，拿 q95 当输入就是二次压缩。所以走无损那道口：
       按快照重算得出来就是 PNG，重算不成才退回成图那一档，并把原因一起带回给界面说清 */
    let ctx2 = ctx.clone();
    let row = r.clone();
    let got = util::image_work(ctx.clone(), 0, move || queue::lossless_bytes(&ctx2, &row)).await?;
    let lossless = got.lossless;
    let why = got.why;
    // 文件名里不能出现 # —— 它会直接把 /file/ 的 URL 截断
    let label = format!("{stem} 派生{rid}.{}", got.ext); /* i18n-keep 落盘文件名，跟着库走，翻它等于改已有数据 */
    let rel = util::rel_path(&["projects".into(), r.project_id.to_string(), format!("{}_{r4}_{label}", util::now_ms(), r4 = util::r4())]);
    // 重算与写盘都是同步重活（24MP 解码 + 缝合 + PNG 编码），一次都别按在 worker 上
    let dst = ctx.data.join(&rel);
    let bytes = got.bytes;
    util::library_work(ctx.clone(), move || -> Result<()> {
        std::fs::write(&dst, &bytes).map_err(|e| AppError::fail_args("srv.common.writeFail", json!({ "msg": e.to_string() })))?;
        Ok(())
    })
    .await?;
    // 谱系落库：名字里那个 `派生{rid}` 是给人看的，父子关系靠这两列（改名也不断）
    let new_id = rimg::insert_derived(&ctx, r.project_id, &label, &rel, w, h, r.image_id, rid)?;
    rproj::touch(&ctx, r.project_id)?;
    Ok(ok(json!({ "image_id": new_id, "name": label, "derived_from": r.image_id, "lossless": lossless, "why": why })))
}
