//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{bad, body_of, err, ok, path_id, SEED_MAX};
use crate::service::workflow as cfg;
use crate::error::{AppError, Result};
use crate::models::dto;
use crate::repo::{images as rimg, projects as rproj, results as rres};
use crate::service::{comfy, reclaim};
use crate::state::Shared;
use crate::{service::imagesvc, util};
use axum::body::Bytes;
use axum::extract::{Path as APath, State};
use axum::response::Response;
use serde_json::{Map, Value};

pub async fn run_post(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let settings = body.get("settings").cloned().unwrap_or(Value::Object(Map::new()));
    let rerun_of = body.get("rerun_of").and_then(|v| v.as_i64());
    let mut out: Vec<Value> = Vec::new();
    let ids: Vec<Value> = body.get("image_ids").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    // 逐张兜异常：整批一起抛的话，前面已经交给 ComfyUI 的任务会因为没返回 result_id 而没人轮询，
    // 库里只留下一排永远 running 的僵尸记录。
    for img in ids {
        let img_id = img.as_i64().unwrap_or(-1);
        match run_one(&ctx, img_id, &settings, rerun_of).await {
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

pub async fn run_one(ctx: &Shared, img_id: i64, settings: &Value, rerun_of: Option<i64>) -> Result<Value> {
    let Some(i) = rimg::by_id(ctx, img_id)? else {
        return Ok(serde_json::json!({ "image_id": img_id, "skipped": true, "reason": "图片已不存在" }));
    };
    let skipped = |reason: &str| serde_json::json!({ "image_id": img_id, "skipped": true, "reason": reason });
    let Some(mask_path) = i.mask_path.clone() else { return Ok(skipped("未涂遮罩")) };
    // 库里有过这行不等于盘上还有这个文件：先读会在 readFileSync 抛 ENOENT，报错只对开发者可读
    if !util::file_alive(&ctx.data, &Value::String(i.orig_path.clone())) {
        return Ok(skipped("原图文件已不在磁盘上（data/projects 被清过？）"));
    }
    if !util::file_alive(&ctx.data, &Value::String(mask_path.clone())) {
        return Ok(skipped("遮罩文件已不在磁盘上，重涂一次再提交"));
    }
    let photo_bytes = std::fs::read(ctx.data.join(&i.orig_path)).map_err(|e| format!("读原图失败：{e}"))?;
    // 涂抹层现在存的是 proxy 分辨率，而工作流里 DrawMaskOnImage 要和原图同尺寸，先上采样
    let mask_bytes = imagesvc::mask_to_orig(ctx, &mask_path, i.w as usize, i.h as usize).map_err(|e| AppError::bad(e))?;
    let stamp = util::now_ms();
    let photo = comfy::upload(ctx, photo_bytes, &format!("p{}_{}.png", i.id, stamp)).await.map_err(msg)?;
    let mask = comfy::upload(ctx, mask_bytes, &format!("p{}_m_{}.png", i.id, stamp)).await.map_err(msg)?;
    let random_seed = settings.get("randomSeed").and_then(Value::as_bool).unwrap_or(false);
    let base = if random_seed {
        (rand::random::<f64>() * SEED_MAX as f64).floor() as i64
    } else {
        util::num_or(settings.get("seed"), 0.0) as i64
    };
    let seed = (base + i.id).rem_euclid(SEED_MAX);
    let graph = comfy::build_graph(&photo, &mask, settings, seed, &cfg::get_cfg(ctx));
    let r = comfy::post_json(ctx, "/prompt", serde_json::json!({ "prompt": graph, "client_id": "synco" }))
        .await
        .map_err(msg)?;
    let Some(prompt_id) = r.get("prompt_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string) else {
        let txt: String = r.to_string().chars().take(300).collect();
        return Ok(skipped(&format!("ComfyUI 拒收：{txt}")));
    };
    let id = rres::insert_local(
        ctx,
        i.id,
        i.project_id,
        &prompt_id,
        settings.get("prompt").and_then(|v| v.as_str()).unwrap_or(""),
        util::num_or(settings.get("steps"), 0.0),
        util::num_or(settings.get("cfg"), 0.0),
        seed,
        &i.orig_path,
        &settings.to_string(),
        rerun_of,
    )?;
    rproj::touch(ctx, i.project_id)?;
    Ok(serde_json::json!({ "result_id": id, "image_id": i.id }))
}

pub async fn result_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    Ok(match reclaim::settle(&ctx, path_id(&id)?).await? {
        Some(v) => ok(v),
        None => err(404, "no result"),
    })
}

pub async fn interrupt_post(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let rid = path_id(&id)?;
    let Some(r) = rres::by_id(&ctx, rid)? else { return Ok(err(404, "no result")) };
    if !r.running() {
        return Ok(bad("这条已经落定了"));
    }
    match r.prompt_id {
        // 云端行没有 prompt_id：那次请求在对面跑着，撤不回来；标记一下让 worker 别采用结果
        None => {
            ctx.job_cancel(rid);
            ctx.mark_error(rid, "已取消：云端那次请求撤不回来，结果不再采用");
        }
        Some(prompt_id) => {
            let errs = comfy::interrupt(&ctx, &prompt_id).await;
            let text = if errs.is_empty() {
                "已手动中断".to_string()
            } else {
                format!("已标记取消，但没通知到 ComfyUI（{}）", errs.join("；"))
            };
            ctx.mark_error(rid, &text);
        }
    }
    Ok(ok(rres::value_by_id(&ctx, rid)?.map(|v| dto::result_json(&ctx, &v)).unwrap_or(Value::Null)))
}

pub async fn result_delete(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let rid = path_id(&id)?;
    let Some(r) = rres::by_id(&ctx, rid)? else { return Ok(err(404, "no result")) };
    // 云端行先按超时判一次：判死了就允许删，别让"生成中"永久挡着垃圾桶
    if r.running() && !reclaim::judge_cloud(&ctx, rid) {
        return Ok(bad("这条还在生成中，等它落定再删"));
    }
    for rel in [&r.final_path, &r.crop_path, &r.maskoverlay_path, &r.thumb_path] {
        if let Some(rel) = rel {
            let _ = std::fs::remove_file(ctx.data.join(rel));
        }
    }
    rres::delete(&ctx, rid)?;
    Ok(ok(serde_json::json!({ "ok": true })))
}

pub async fn fork_post(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let rid = path_id(&id)?;
    let Some(r) = rres::by_id(&ctx, rid)? else { return Ok(err(404, "没有这条结果")) };
    let Some(final_path) = r.final_path.filter(|p| ctx.data.join(p).is_file()) else {
        return Ok(bad("这条记录还没有成图可复制"));
    };
    let (name, w, h) = rimg::name_and_size(&ctx, r.image_id)?.unwrap_or(("photo".into(), 0, 0));
    let stem = util::stem_of(&name);
    // 文件名里不能出现 # —— 它会直接把 /file/ 的 URL 截断
    let label = format!("{stem} 派生{rid}.png");
    let rel = util::rel_path(&["projects".into(), r.project_id.to_string(), format!("{}_{r4}_{label}", util::now_ms(), r4 = util::r4())]);
    std::fs::copy(ctx.data.join(&final_path), ctx.data.join(&rel)).map_err(|e| format!("复制失败：{e}"))?;
    let new_id = rimg::insert(&ctx, r.project_id, &label, &rel, w, h)?;
    rproj::touch(&ctx, r.project_id)?;
    Ok(ok(serde_json::json!({ "image_id": new_id, "name": label })))
}

pub(crate) fn msg(e: String) -> AppError {
    AppError::Upstream { code: 502, msg: e }
}
