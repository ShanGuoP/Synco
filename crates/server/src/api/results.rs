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
use axum::extract::{Path as APath, Query, State};
use axum::response::Response;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// 结果集合：项目页「派生查看」的数据源。`?project_id=` 列整项目，`&image_id=` 收窄到一张。
/// 与 `/api/images/{id}` 的差别是这条没有副作用（不判云端僵尸、不补派生档），也不锁在 8 条。
pub async fn results_list(State(ctx): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Result<Response> {
    let num = |k: &str| q.get(k).and_then(|s| s.parse::<i64>().ok());
    let limit = num("limit").unwrap_or(400).clamp(1, 2000);
    let rows = match (num("project_id"), num("image_id")) {
        (_, Some(iid)) => rres::list_for_image(&ctx, iid, limit + 1)?,
        (Some(pid), None) => rres::list_for_project(&ctx, pid, limit + 1)?,
        _ => return Ok(bad("要带 project_id 或 image_id")),
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
    // 画稿不能进局部重绘：它没有"蒙版外要保持"这回事，裁切与缝合对它没有意义
    if i.is_sketch() {
        return Ok(skipped("这是画稿，请在画布里点「生成」"));
    }
    // 整图重绘只走云端：本机那条要整图重绘得旁路掉裁切缝合，那是另一张图、另一套校验，
    // 悄悄改用内置图去跑就等于换了一条语义还不说
    if settings.get("full").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(skipped("整图重绘只走云端。本机 ComfyUI 那条要整图，请把没有裁切/缝合节点的工作流导成 API 再指过来"));
    }
    let Some(mask_path) = i.mask_path.clone() else { return Ok(skipped("未涂遮罩")) };
    // 库里有过这行不等于盘上还有这个文件：先读会在 readFileSync 抛 ENOENT，报错只对开发者可读
    if !util::file_alive(&ctx.data, &Value::String(i.orig_path.clone())) {
        return Ok(skipped("原图文件已不在磁盘上（data/projects 被清过？）"));
    }
    if !util::file_alive(&ctx.data, &Value::String(mask_path.clone())) {
        return Ok(skipped("遮罩文件已不在磁盘上，重涂一次再提交"));
    }
    // 先判参数再上传：0 步 0 CFG（云端那一行的形状）进了 KSampler 不会报错，只会出一张废图
    if let Err(e) = comfy::sample_args(&settings) {
        return Ok(skipped(&e));
    }
    // 提交用哪张计算图：你的工作流（角色校验过了）或程序内置图。
    // 是你的图却认不出角色时**明确拒绝**，不回退内置图（D14）：回退就是"静默谎报"搬到了结构层。
    // 判在上传之前，免得为一张注定不提交的图先传两张 24MP 的 PNG。
    let plan = cfg::plan(ctx);
    if let cfg::Plan::Refused { errors } = &plan {
        return Ok(skipped(&format!("你的工作流不能接管提交：{}", errors.join("；"))));
    }
    let photo_bytes = std::fs::read(util::data_file(&ctx.data, &i.orig_path).ok_or_else(|| AppError::bad("原图文件已不在磁盘上"))?)
        .map_err(|e| format!("读原图失败：{e}"))?;
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
    let seed = super::common::seed_for(base, i.id);
    let takeover = matches!(plan, cfg::Plan::Workflow { .. });
    let (graph, outs) = match plan {
        cfg::Plan::Workflow { graph: g, roles } => {
            let built = comfy::inject_workflow(&g, &roles, &photo, &mask, &settings, seed).map_err(|e| AppError::bad(e))?;
            (built, comfy::Outputs::from_roles(&roles))
        }
        _ => (
            comfy::build_graph(&photo, &mask, settings, seed, &cfg::get_cfg(ctx)),
            comfy::Outputs::builtin(),
        ),
    };
    let r = comfy::post_json(ctx, "/prompt", serde_json::json!({ "prompt": graph, "client_id": "synco" }))
        .await
        .map_err(msg)?;
    let Some(prompt_id) = r.get("prompt_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string) else {
        let txt: String = r.to_string().chars().take(300).collect();
        return Ok(skipped(&format!("ComfyUI 拒收：{txt}")));
    };
    // 输出节点号跟着这一行走：接管提交时它和内置图的 17/19/20 可以完全不同，
    // 轮询侧要按这一行记的那三个号去 history 里取图
    let mut snap = settings.as_object().cloned().unwrap_or_default();
    snap.insert("workflow_out".into(), outs.to_json());
    snap.insert("graph_source".into(), Value::String(if takeover { "workflow".into() } else { "builtin".into() }));
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
        &Value::Object(snap).to_string(),
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
    // 先删行、后删文件：反过来一旦中间失败，库里就挂着指向空气的记录（卡片在、点开是空的）
    let rels: Vec<&String> = [&r.final_path, &r.crop_path, &r.maskoverlay_path, &r.thumb_path, &r.sketch_path]
        .into_iter()
        .flatten()
        .filter(|rel| !rel.is_empty())
        .collect();
    rres::delete(&ctx, rid)?;
    for rel in rels {
        if let Some(p) = util::data_file(&ctx.data, rel) {
            let _ = std::fs::remove_file(p);
        }
    }
    Ok(ok(serde_json::json!({ "ok": true })))
}

pub async fn fork_post(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let rid = path_id(&id)?;
    let Some(r) = rres::by_id(&ctx, rid)? else { return Ok(err(404, "没有这条结果")) };
    let Some(src) = r.final_path.as_deref().and_then(|p| util::data_file(&ctx.data, p)) else {
        return Ok(bad("这条记录还没有成图可复制"));
    };
    let (name, w, h) = rimg::name_and_size(&ctx, r.image_id)?.unwrap_or(("photo".into(), 0, 0));
    let stem = util::stem_of(&name);
    // 文件名里不能出现 # —— 它会直接把 /file/ 的 URL 截断
    let label = format!("{stem} 派生{rid}.png");
    let rel = util::rel_path(&["projects".into(), r.project_id.to_string(), format!("{}_{r4}_{label}", util::now_ms(), r4 = util::r4())]);
    std::fs::copy(&src, ctx.data.join(&rel)).map_err(|e| format!("复制失败：{e}"))?;
    // 谱系落库：名字里那个 `派生{rid}` 是给人看的，父子关系靠这两列（改名也不断）
    let new_id = rimg::insert_derived(&ctx, r.project_id, &label, &rel, w, h, r.image_id, rid)?;
    rproj::touch(&ctx, r.project_id)?;
    Ok(ok(serde_json::json!({ "image_id": new_id, "name": label, "derived_from": r.image_id })))
}

pub(crate) fn msg(e: String) -> AppError {
    AppError::Upstream { code: 502, msg: e }
}
