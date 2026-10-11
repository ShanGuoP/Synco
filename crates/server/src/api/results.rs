//! 路由薄层：只做参数解析、状态码与响应形状；业务在 `crate::service`，读写在 `crate::repo`。

use super::common::{bad, body_of, err, ok, path_id, SEED_MAX};
use crate::service::workflow as cfg;
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
        return Ok(serde_json::json!({ "image_id": img_id, "skipped": true, "reason": "srv.submit.imageGone" }));
    };
    let skipped = |reason: &str| serde_json::json!({ "image_id": img_id, "skipped": true, "reason": reason });
    // 带参数的理由：界面按钥匙取句，参数当数组传（顿号属于哪种语言由字典定）
    let skipped_with = |reason: &str, args: Value|
        serde_json::json!({ "image_id": img_id, "skipped": true, "reason": reason, "reason_args": args });
    // 已经是钥匙的错误原样给钥匙+参数；没交钥匙的（上游原文）就把那句当理由
    let skipped_of = |e: &AppError| {
        let (code, args) = e.reason();
        let mut o = skipped(&code);
        if let Some(a) = args { o["reason_args"] = Value::Object(a); }
        o
    };
    // 画稿不能进局部重绘：它没有"蒙版外要保持"这回事，裁切与缝合对它没有意义
    if i.is_sketch() {
        return Ok(skipped("srv.submit.isSketch"));
    }
    // 整图重绘只走云端：本机那条要整图重绘得旁路掉裁切缝合，那是另一张图、另一套校验，
    // 悄悄改用内置图去跑就等于换了一条语义还不说
    if settings.get("full").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(skipped("srv.submit.fullNeedsCloud"));
    }
    let Some(mask_path) = i.mask_path.clone() else { return Ok(skipped("srv.submit.noMask")) };
    // 库里有过这行不等于盘上还有这个文件：先读会在 readFileSync 抛 ENOENT，报错只对开发者可读
    if !util::file_alive(&ctx.data, &Value::String(i.orig_path.clone())) {
        return Ok(skipped("srv.submit.origGone"));
    }
    if !util::file_alive(&ctx.data, &Value::String(mask_path.clone())) {
        return Ok(skipped("srv.submit.maskGone"));
    }
    // 先判参数再上传：0 步 0 CFG（云端那一行的形状）进了 KSampler 不会报错，只会出一张废图
    if let Err(e) = comfy::sample_args(&settings) {
        return Ok(skipped_of(&e));
    }
    // 提交用哪张计算图：你的工作流（角色校验过了）或程序内置图。
    // 是你的图却认不出角色时**明确拒绝**，不回退内置图（D14）：回退就是"静默谎报"搬到了结构层。
    // 判在上传之前，免得为一张注定不提交的图先传两张 24MP 的 PNG。
    let plan = cfg::plan(ctx);
    if let cfg::Plan::Refused { errors } = &plan {
        return Ok(skipped_with("srv.submit.wfCannotTakeover", json!({ "errors": errors })));
    }
    // 提交给本机工作流的是"调整后"那张（拍板 4）：遮罩与照片必须同一套参数算出来。
    // 没动过滑杆时两条都原样回文件字节与原路径，链路与 0.2.1 逐字节一致。
    // 涂抹层存的是 proxy 分辨率，工作流里 DrawMaskOnImage 又要和照片同幅：上采样与几何段都在服务侧做完
    // （见 adjust::submit_mask——照片过了几何段而遮罩没过，圈到的就是另一块地方）。
    // 读原图与这两步都是全分辨率的活，整段过阻塞池：压在 worker 上时整站轮询会一起卡住
    let (photo_bytes, mask_bytes, sent_rel) = {
        let ctx2 = ctx.clone();
        let img = i.clone();
        util::blocking(move || -> Result<(Vec<u8>, Vec<u8>, String)> {
            let (p, rel) = crate::service::adjust::submit_artifact(&ctx2, &img)?;
            let m = crate::service::adjust::submit_mask(&ctx2, &img)?;
            Ok((p, m, rel))
        })
        .await?
    };
    let stamp = util::now_ms();
    let photo = comfy::upload(ctx, photo_bytes, &format!("p{}_{}.png", i.id, stamp)).await?;
    let mask = comfy::upload(ctx, mask_bytes, &format!("p{}_m_{}.png", i.id, stamp)).await?;
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
            let built = comfy::inject_workflow(&g, &roles, &photo, &mask, &settings, seed)?;
            (built, comfy::Outputs::from_roles(&roles))
        }
        _ => (
            comfy::build_graph(&photo, &mask, settings, seed, &cfg::get_cfg(ctx)),
            comfy::Outputs::builtin(),
        ),
    };
    let r = comfy::post_json(ctx, "/prompt", serde_json::json!({ "prompt": graph, "client_id": "synco" }))
        .await
        ?;
    let Some(prompt_id) = r.get("prompt_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string) else {
        let txt: String = r.to_string().chars().take(300).collect();
        return Ok(skipped_with("srv.submit.comfyRejected", json!({ "msg": txt })));
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
        &sent_rel,
        &Value::Object(snap).to_string(),
        rerun_of,
    )?;
    rproj::touch(ctx, i.project_id)?;
    Ok(serde_json::json!({ "result_id": id, "image_id": i.id }))
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
    match r.prompt_id {
        // 云端行没有 prompt_id：那次请求在对面跑着，撤不回来；标记一下让 worker 别采用结果
        None => {
            ctx.job_cancel(rid);
            ctx.mark_error(rid, "srv.result.cancelled", serde_json::Value::Null);
        }
        Some(prompt_id) => {
            let errs = comfy::interrupt(&ctx, &prompt_id).await;
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
    let got = util::blocking(move || queue::lossless_bytes(&ctx2, &row)).await?;
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
    let got = util::blocking(move || queue::lossless_bytes(&ctx2, &row)).await?;
    let lossless = got.lossless;
    let why = got.why;
    // 文件名里不能出现 # —— 它会直接把 /file/ 的 URL 截断
    let label = format!("{stem} 派生{rid}.{}", got.ext); /* i18n-keep 落盘文件名，跟着库走，翻它等于改已有数据 */
    let rel = util::rel_path(&["projects".into(), r.project_id.to_string(), format!("{}_{r4}_{label}", util::now_ms(), r4 = util::r4())]);
    // 重算与写盘都是同步重活（24MP 解码 + 缝合 + PNG 编码），一次都别按在 worker 上
    let dst = ctx.data.join(&rel);
    let bytes = got.bytes;
    util::blocking(move || -> Result<()> {
        std::fs::write(&dst, &bytes).map_err(|e| AppError::fail_args("srv.common.writeFail", json!({ "msg": e.to_string() })))?;
        Ok(())
    })
    .await?;
    // 谱系落库：名字里那个 `派生{rid}` 是给人看的，父子关系靠这两列（改名也不断）
    let new_id = rimg::insert_derived(&ctx, r.project_id, &label, &rel, w, h, r.image_id, rid)?;
    rproj::touch(&ctx, r.project_id)?;
    Ok(ok(json!({ "image_id": new_id, "name": label, "derived_from": r.image_id, "lossless": lossless, "why": why })))
}
