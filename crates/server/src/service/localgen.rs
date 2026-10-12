//! 本机生成用例：冻结任务地址和工作流，准备输入、提交并登记结果。
use crate::error::{AppError, Result};
use crate::repo::{images as rimg, projects as rproj, results as rres};
use crate::service::{comfy, workflow as cfg};
use crate::{state::Shared, util};
use serde_json::{json, Value};
use comfy::SEED_MAX;

pub async fn run_one(ctx: &Shared, img_id: i64, settings: &Value, rerun_of: Option<i64>, comfy_base: &str) -> Result<Value> {
    let _library = ctx.work().enter().await;
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
    let plan_ctx = ctx.clone();
    let (plan, graph_cfg) = util::library_work(ctx.clone(), move || Ok((cfg::plan(&plan_ctx), cfg::get_cfg(&plan_ctx)))).await?;
    if let cfg::Plan::Refused { errors } = &plan {
        return Ok(skipped_with("srv.submit.wfCannotTakeover", json!({ "errors": errors })));
    }
    // 提交给本机工作流的是"调整后"那张（拍板 4）：遮罩与照片必须同一套参数算出来。
    // 没动过滑杆时两条都原样回文件字节与原路径，链路与 0.2.1 逐字节一致。
    // 涂抹层存的是 proxy 分辨率，工作流里 DrawMaskOnImage 又要和照片同幅：上采样与几何段都在服务侧做完
    // （见 adjust::submit_mask——照片过了几何段而遮罩没过，圈到的就是另一块地方）。
    // 读原图与这两步都是全分辨率的活，整段过阻塞池：压在 worker 上时桌面壳的窗口会一起卡住
    let use_alpha_mask = matches!(&plan, cfg::Plan::Mapped { profile, .. } if profile.bindings.iter().any(|b| b.role == "source_image_mask"));
    let (photo_bytes, mask_bytes, sent_rel) = {
        let ctx2 = ctx.clone();
        let img = i.clone();
        util::image_work(ctx.clone(), util::image_pixels(&i), move || -> Result<(Vec<u8>, Vec<u8>, String)> {
            let (mut p, rel) = crate::service::adjust::submit_artifact(&ctx2, &img)?;
            let m = crate::service::adjust::submit_mask(&ctx2, &img)?;
            if use_alpha_mask { p = crate::service::workflow_graph::alpha_image(&p, &m)?; }
            Ok((p, m, rel))
        })
        .await?
    };
    let stamp = util::now_ms();
    let photo = comfy::upload(ctx, comfy_base, photo_bytes, &format!("p{}_{}.png", i.id, stamp)).await?;
    let mask = comfy::upload(ctx, comfy_base, mask_bytes, &format!("p{}_m_{}.png", i.id, stamp)).await?;
    let random_seed = settings.get("randomSeed").and_then(Value::as_bool).unwrap_or(false);
    let base = if random_seed {
        (rand::random::<f64>() * SEED_MAX as f64).floor() as i64
    } else {
        util::num_or(settings.get("seed"), 0.0) as i64
    };
    let seed = comfy::seed_for(base, i.id);
    let takeover = matches!(plan, cfg::Plan::Workflow { .. } | cfg::Plan::Mapped { .. });
    let (graph, outs) = match plan {
        cfg::Plan::Mapped { graph: g, profile } => {
            let built = crate::service::workflow_graph::compile(&g, &profile, &photo, &mask, &settings, seed)?;
            let roles = serde_json::from_value(json!({"out_final":profile.output_node})).unwrap();
            (built, comfy::Outputs::from_roles(&roles))
        }
        cfg::Plan::Workflow { graph: g, roles } => {
            let built = comfy::inject_workflow(&g, &roles, &photo, &mask, &settings, seed)?;
            (built, comfy::Outputs::from_roles(&roles))
        }
        _ => (
            comfy::build_graph(&photo, &mask, settings, seed, &graph_cfg),
            comfy::Outputs::builtin(),
        ),
    };
    let r = comfy::post_json(ctx, comfy_base, "/prompt", serde_json::json!({ "prompt": graph, "client_id": "synco" }))
        .await
        ?;
    let Some(prompt_id) = r.get("prompt_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string) else {
        let txt: String = r.to_string().chars().take(300).collect();
        return Ok(skipped_with("srv.submit.comfyRejected", json!({ "msg": txt })));
    };
    // 输出节点号跟着这一行走：接管提交时它和内置图的 17/19/20 可以完全不同，
    // 轮询侧要按这一行记的那三个号去 history 里取图
    let mut snap = settings.as_object().cloned().unwrap_or_default();
    snap.insert("comfy_url".into(), Value::String(comfy_base.to_string()));
    snap.insert("workflow_out".into(), outs.to_json());
    snap.insert("graph_source".into(), Value::String(if takeover { "workflow".into() } else { "builtin".into() }));
    let inserted = rres::insert_local(
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
    );
    let id = match inserted {
        Ok(id) => id,
        Err(e) => { let _ = comfy::interrupt(ctx, comfy_base, &prompt_id).await; return Err(e); }
    };
    // 任务已受理并有记录，项目排序更新失败不能丢掉返回给调用方的任务身份。
    let _ = rproj::touch(ctx, i.project_id);
    Ok(serde_json::json!({ "result_id": id, "image_id": i.id }))
}
