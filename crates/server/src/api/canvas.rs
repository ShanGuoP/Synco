//! 画布（图生图）：空白画面上画几笔，整张交给云端按提示词生成。
//!
//! 画稿是 `images` 表里 `kind='sketch'` 的一行，`orig_path` 存的就是那张 PNG。
//! 它与照片的唯一区别是**没有"蒙版外要保持"这回事**：不带遮罩、不裁切、不缝合，
//! 所以出图走 `service::queue` 里的画布分支，而照片那两条入口（`/api/run`、`/api/cloud/queue`）
//! 会按 `kind` 显式拒绝画稿。

use super::common::{bad, bad_args, body_of, err, ok, path_id};
use crate::error::{AppError, Result};
use crate::models::{dto, entity::Image};
use crate::repo::{images as rimg, projects as rproj, results as rres};
use crate::service::{cloud, imagesvc, queue, refs};
use crate::state::Shared;
use crate::util;
use axum::body::Bytes;
use axum::extract::{Path as APath, State};
use axum::response::Response;
use serde_json::{json, Map, Value};
use stitch_core::Rgba;

/// 画布默认档：1024×1024 正好落在云端硬约束中间，和本机那路的 1024 手感一致
const DEFAULT_EDGE: i64 = 1024;

fn new_name(body: &Value) -> String {
    let raw = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
    util::clip(if raw.is_empty() { "画布" /* i18n-keep 建图时落库的默认名，翻译它等于改已存数据 */ } else { raw }, 60)
}

/// 建一张空白画布。可以给现有项目加，也可以顺手开一个只装画布的新项目。
pub async fn canvas_create(State(ctx): State<Shared>, raw: Bytes) -> Result<Response> {
    let body = body_of(raw).await?;
    let w = util::clamp_round(body.get("w"), 1, 8192, DEFAULT_EDGE) as usize;
    let h = util::clamp_round(body.get("h"), 1, 8192, DEFAULT_EDGE) as usize;
    queue::sketch_fit(w, h)?;
    let name = new_name(&body);
    let pid = match body.get("project_id").and_then(|v| v.as_i64()) {
        Some(id) => {
            if rproj::by_id(&ctx, id)?.is_none() {
                return Ok(err(404, "srv.common.noProject"));
            }
            id
        }
        None => rproj::create(&ctx, &format!("画布 · {name}") /* i18n-keep 建项目时落库的默认名 */)?,
    };
    let safe = util::safe_name(&name);
    // 空白画布 = 全透明的 RGBA；发出去时才拍到白底上（prepare_sketch 那一步在队列里）
    let rel = util::rel_path(&[
        "projects".into(),
        pid.to_string(),
        format!("{}_{r4}_{safe}_sketch.png", util::now_ms(), r4 = util::r4()),
    ]);
    // 建目录、光栅化与 PNG 编码都是同步活（8192 一档的光栅就是几百 MB），整段过阻塞池
    {
        let data = ctx.data.clone();
        let rel2 = rel.clone();
        util::blocking(move || -> Result<()> {
            std::fs::create_dir_all(data.join("projects").join(pid.to_string()))
                .map_err(|e| AppError::fail_args("srv.canvas.mkdirFail", json!({ "msg": e.to_string() })))?;
            let blank = Rgba::new(w, h);
            // rel 本身已经带 projects/{pid}/ 前缀，只能接在 data 下面：接在项目目录上会写深一层，
            // 而 write_bytes 自己会补父目录，于是库里那行指的地址上根本没有文件
            imagesvc::write_bytes(&data.join(&rel2), &crate::img::codec::encode_png(&blank))?;
            Ok(())
        })
        .await?;
    }
    let id = rimg::insert_kind(&ctx, pid, &safe, &rel, w as i64, h as i64, "sketch")?;
    rproj::touch(&ctx, pid)?;
    Ok(ok(serde_json::json!({ "project_id": pid, "image_id": id, "name": safe, "w": w, "h": h })))
}

/// 画布详情：画稿地址 + 历史成图。不走 `/api/images/{id}`，那条会替照片判云端僵尸、补派生档。
pub async fn canvas_get(State(ctx): State<Shared>, APath(id): APath<String>) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "srv.canvas.noImage")) };
    if !img.is_sketch() {
        return Ok(err(404, "srv.canvas.notSketch"));
    }
    let rows: Vec<Value> = rres::list_for_image(&ctx, iid, 60)?
        .iter()
        .map(|r| dto::result_json(&ctx, r))
        .collect();
    let mut o = Map::new();
    o.insert("image".into(), dto::image_json(&ctx, &img, true));
    o.insert("sketch_url".into(), Value::String(format!("/file/{}", img.orig_path)));
    o.insert("results".into(), Value::Array(rows));
    // 当前槽位：刷新页面还要看得见"我挂了哪几张参考图"，所以存在服务端而不是浏览器内存里
    o.insert("refs".into(), Value::Array(refs::json_of(&ctx, &refs::slots(&ctx, &img))));
    o.insert("refs_max".into(), Value::from(refs::MAX as i64));
    Ok(ok(Value::Object(o)))
}

/// 存画稿：原地覆写（.part→rename），先解一次确认是能解码的图。
pub async fn sketch_post(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "srv.canvas.noImage")) };
    if !img.is_sketch() {
        return Ok(bad("srv.canvas.notSketch"));
    }
    let body = body_of(raw).await?;
    let b64 = body.get("b64").and_then(|v| v.as_str()).unwrap_or("");
    let bytes = util::decode_b64(b64);
    sketch_relate(&img)?;
    // 解码与覆写都是同步重活（涂抹层能到 3072 一档），整段过阻塞池。
    // 解不出来就不覆盖：画稿是用户唯一的手感来源，写坏一次等于抹掉他的草稿
    let dest = ctx.data.join(&img.orig_path);
    let (dw, dh) = util::blocking(move || -> Result<(usize, usize)> {
        let decoded = crate::img::codec::decode(&bytes)
            .map_err(|_| AppError::bad("srv.canvas.pngBadNoOverwrite"))?;
        if decoded.w == 0 || decoded.h == 0 {
            return Err(AppError::bad("srv.canvas.emptyNoOverwrite"));
        }
        imagesvc::write_bytes(&dest, &bytes)?;
        Ok((decoded.w, decoded.h))
    })
    .await?;
    if dw as i64 != img.w || dh as i64 != img.h {
        rimg::set_dims(&ctx, iid, dw as i64, dh as i64)?;
    }
    Ok(ok(serde_json::json!({ "ok": true, "w": dw, "h": dh })))
}

/// 提交一次生成：进同一条云端队列，进度就是这条 results 行。
pub async fn canvas_generate(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "srv.canvas.noImage")) };
    if !img.is_sketch() {
        return Ok(bad("srv.canvas.notSketch"));
    }
    let body = body_of(raw).await?;
    let settings = body.get("settings").cloned().unwrap_or(Value::Object(Map::new()));
    let prompt = settings.get("prompt").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if prompt.is_empty() {
        return Ok(bad("srv.canvas.needPrompt"));
    }
    if !util::file_alive(&ctx.data, &Value::String(img.orig_path.clone())) {
        return Ok(bad("srv.canvas.sketchGone"));
    }
    queue::sketch_fit(img.w.max(1) as usize, img.h.max(1) as usize)?;
    let s = cloud::settings(&ctx);
    if s.base.is_empty() || s.model.is_empty() || s.key.is_empty() {
        return Ok(bad("srv.canvas.needCloud"));
    }
    // 参考图缺文件时**建行之前**就拒：一旦插了行再失败，用户看到的是一条挂着空气的"生成中"
    let slots = refs::slots(&ctx, &img);
    if let Some(missing) = slots.iter().find(|r| !util::file_alive(&ctx.data, &Value::String(r.to_string()))) {
        return Ok(bad_args("srv.canvas.refsGone", json!({ "missing": missing })));
    }
    let rerun_of = body.get("rerun_of").and_then(|v| v.as_i64());
    let payload = serde_json::json!({ "prompt": prompt, "negative": "", "steps": 0, "cfg": 0, "loras": [], "canvas": true });
    let rid = rres::insert_cloud(&ctx, img.id, img.project_id, &prompt, &payload.to_string(), &s.model, rerun_of)?;
    // 先把这一版的线稿原样复制一份再排队：画稿是原地覆写的，用户接着画两笔，
    // "出这张图时我画的是什么"就只剩这一份能证明。复制而不重编码——转一档会把笔迹变糊
    if let Err(e) = snapshot_sketch(&ctx, &img, rid).await {
        eprintln!("  画稿快照没存下（#{rid}）：{e}"); /* 日志：控制台给人读 */
    }
    // 参考图同理复制成这一行自己的快照：排着队的时候换槽位，不该改"这一版参考了哪几张"
    if !slots.is_empty() {
        // 行已经建起来了，这两步再往上传 Err 就是留一条永远"生成中"又没人跑它的僵尸：
        // 判死这一行、把原因还给用户。也不能退化成"少发几张照跑"——界面上写着带 N 张参考图
        let snap_try = {
            let (ctx2, im, sl) = (ctx.clone(), img.clone(), slots.clone());
            util::blocking(move || refs::snapshot(&ctx2, &im, rid, &sl))
                .await
        };
        let snapped = match snap_try {
            Ok(v) => v,
            Err(e) => {
                // 内层已经有自己的钥匙（画稿不在盘上 / 复制失败），原样传出去，不再包一句中文
                ctx.mark_error_of(rid, &e);
                return Err(e);
            }
        };
        if let Err(e) = rres::set_refs(&ctx, rid, &snapped) {
            refs::drop(&ctx, img.project_id, &snapped);
            ctx.mark_error_of(rid, &e);
            return Err(e);
        }
    }
    rres::set_queued(&ctx, rid)?;
    rproj::touch(&ctx, img.project_id)?;
    queue::pump(&ctx).await;
    Ok(ok(serde_json::json!({ "result_id": rid, "image_id": img.id, "queued": true })))
}

/// 加参考图。三种来源各走一条，成功后回整组槽位：
/// `files:[{b64}]` 本地上传、`image_ids:[id]` 从库里挑、`from_result:rid` 照搬某一版当时那几张。
/// 三条都落成**这个画布自己的**新文件（复制，不引用）：原图后来被删或被重新导入，
/// "这一版参考了哪张"要还能说清。
pub async fn canvas_refs_add(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "srv.canvas.noImage")) };
    if !img.is_sketch() {
        return Ok(bad("srv.canvas.notSketch"));
    }
    let body = body_of(raw).await?;
    // 三条来源的校验留在 handler 里（拒绝的理由要说准），解码/折档/重编码/落盘整段过阻塞池：
    // 一张参考图是全分辨率的 PNG，四张就是四次"解码 + 缩放 + 重编码"
    let row_opt = match body.get("from_result").and_then(|v| v.as_i64()) {
        Some(rid) => {
            let Some(row) = rres::by_id(&ctx, rid)? else { return Ok(bad("srv.common.noResult")) };
            if row.image_id != iid {
                return Ok(bad("srv.canvas.wrongOwner"));
            }
            Some(row)
        }
        None => None,
    };
    let files: Vec<Vec<u8>> = body
        .get("files")
        .and_then(|v| v.as_array())
        .map(|list| list.iter().map(|f| util::decode_b64(f.get("b64").and_then(|v| v.as_str()).unwrap_or(""))).collect())
        .unwrap_or_default();
    let mut src_ids: Vec<i64> = Vec::new();
    if let Some(ids) = body.get("image_ids").and_then(|v| v.as_array()) {
        for v in ids {
            let Some(src) = v.as_i64() else { return Ok(bad("srv.canvas.idsNumber")) };
            src_ids.push(src);
        }
    }
    let (added, from_empty) = {
        let (ctx2, im) = (ctx.clone(), img.clone());
        util::blocking(move || -> Result<(Vec<String>, bool)> {
            let mut added: Vec<String> = Vec::new();
            let mut from_empty = false;
            if let Some(row) = &row_opt {
                added = refs::add_from_result(&ctx2, &im, row)?;
                from_empty = added.is_empty();
            }
            for bytes in &files {
                added.push(refs::add_bytes(&ctx2, &im, bytes)?);
            }
            for src in &src_ids {
                added.push(refs::add_from_image(&ctx2, &im, *src)?);
            }
            Ok((added, from_empty))
        })
        .await?
    };
    if added.is_empty() {
        // 那一版根本没带参考图 ≠ 请求写坏了：两种理由分开说，不然用户不知道点错了哪一条
        return Ok(bad(if from_empty { "srv.canvas.noRefsInVersion" } else { "srv.canvas.noRefsGiven" }));
    }
    let mut all = refs::slots(&ctx, &img);
    let over = all.len() + added.len() > refs::MAX;
    all.extend(added);
    refs::set(&ctx, &img, &all)?;
    if over {
        // 新加的已经落盘，超出上限的那几张当场撤下并删文件，不然槽位越清越多
        let cut = all.split_off(refs::MAX);
        refs::drop(&ctx, img.project_id, &cut);
        refs::set(&ctx, &img, &all)?;
        return Ok(bad_args("srv.canvas.capDropped", json!({ "cap": refs::MAX })));
    }
    Ok(ok(serde_json::json!({ "refs": refs::json_of(&ctx, &all) })))
}

/// 整组替换：撤一张、清空、调整顺序都走这条。被撤下的文件连着删，集合里只留还认的。
pub async fn canvas_refs_set(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "srv.canvas.noImage")) };
    if !img.is_sketch() {
        return Ok(bad("srv.canvas.notSketch"));
    }
    let body = body_of(raw).await?;
    let want: Vec<String> = body
        .get("paths")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if want.len() > refs::MAX {
        return Ok(bad_args("srv.canvas.capGiven", json!({ "cap": refs::MAX, "got": want.len() })));
    }
    let before = refs::slots(&ctx, &img);
    // 只认"之前 GET 到的集合里的、而且这次只算一张"：dedup() 只去相邻重复，
    // body 里 ["a","b","a"] 会原样留着，同一张参考图就被发两次
    let mut kept: Vec<String> = Vec::new();
    for r in want {
        if before.contains(&r) && !kept.contains(&r) {
            kept.push(r);
        }
    }
    // 只删"这次不再认"的那些：前端传来的 rel 一定来自之前 GET 到的集合，
    // 不在集合里的字符串不认账，免得跟着一个坏 body 删到别人的文件
    let removed: Vec<String> = before.iter().filter(|r| !kept.contains(r)).cloned().collect();
    refs::drop(&ctx, img.project_id, &removed);
    refs::set(&ctx, &img, &kept)?;
    Ok(ok(serde_json::json!({ "refs": refs::json_of(&ctx, &kept) })))
}

/// 这一行的画稿落在哪：**写**之前先词法判一次（写目标可能还没有文件，`canonicalize` 判不了不存在的
/// 路径），读则走 `util::data_file`（要真在盘上）。库里若躺着绝对路径、盘符或 `..`，不能跟着它写到 data/ 外。
fn sketch_relate(img: &Image) -> std::result::Result<(), AppError> {
    if util::rel_ok(&img.orig_path) {
        Ok(())
    } else {
        Err(AppError::bad("srv.canvas.badPathNoOverwrite"))
    }
}

/// 先把这一版的线稿原样复制一份再排队：复制而不重编码——转一档会把笔迹变糊。
/// 一张画稿是几十 MB 的同步拷贝，所以整段走阻塞池。
async fn snapshot_sketch(ctx: &Shared, img: &Image, rid: i64) -> crate::error::Result<()> {
    let rel = util::rel_path(&["projects".into(), img.project_id.to_string(), format!("k{}_{rid}_sketch.png", img.id)]);
    let src = util::data_file(&ctx.data, &img.orig_path).ok_or_else(|| AppError::fail("srv.canvas.sketchGoneRaw"))?;
    let ctx2 = ctx.clone();
    let dst = ctx.data.join(&rel);
    util::blocking(move || -> Result<()> {
        std::fs::copy(&src, &dst).map_err(|e| AppError::fail_args("srv.common.copyFail", json!({ "msg": e.to_string() })))?;
        rres::set_sketch(&ctx2, rid, &rel)?;
        Ok(())
    })
    .await
}

/// 取回某一版当时的画稿：把那份快照写回画稿本体。
/// 「这条不满意 → 参数搬回来 → 接着改」这条路要求屏幕上还是**当时那版笔迹**，
/// 而不是他后来接着画上去的那些——所以这一步必须能覆盖当前画稿（前端先冲一次自动保存）。
pub async fn canvas_use_sketch(State(ctx): State<Shared>, APath(id): APath<String>, raw: Bytes) -> Result<Response> {
    let iid = path_id(&id)?;
    let Some(img) = rimg::by_id(&ctx, iid)? else { return Ok(err(404, "srv.canvas.noImage")) };
    if !img.is_sketch() {
        return Ok(bad("srv.canvas.notSketch"));
    }
    let body = body_of(raw).await?;
    let Some(rid) = body.get("result_id").and_then(|v| v.as_i64()) else { return Ok(bad("srv.canvas.needResultId")) };
    let Some(row) = rres::by_id(&ctx, rid)? else { return Ok(err(404, "srv.common.noResult")) };
    if row.image_id != iid {
        return Ok(bad("srv.canvas.wrongOwner"));
    }
    let Some(snap) = row.sketch_path.clone() else {
        return Ok(bad("srv.canvas.noSnapshot"));
    };
    sketch_relate(&img)?;
    // 与存画稿同一条纪律：先确认解得开，再 .part→rename，别把用户的草稿写成半张图。
    // 读快照、解码、覆写都是同步重活，整段过阻塞池
    let snap_abs = util::data_file(&ctx.data, &snap).ok_or_else(|| AppError::bad("srv.canvas.snapGone"))?;
    let dest = ctx.data.join(&img.orig_path);
    let (dw, dh) = util::blocking(move || -> Result<(usize, usize)> {
        let bytes = std::fs::read(&snap_abs).map_err(|_| AppError::bad("srv.canvas.snapGone"))?;
        let decoded = crate::img::codec::decode(&bytes).map_err(|_| AppError::bad("srv.canvas.snapBadPng"))?;
        if decoded.w == 0 || decoded.h == 0 {
            return Err(AppError::bad("srv.canvas.snapEmpty"));
        }
        imagesvc::write_bytes(&dest, &bytes)?;
        Ok((decoded.w, decoded.h))
    })
    .await?;
    if dw as i64 != img.w || dh as i64 != img.h {
        rimg::set_dims(&ctx, iid, dw as i64, dh as i64)?;
    }
    rproj::touch(&ctx, img.project_id)?;
    Ok(ok(serde_json::json!({ "ok": true, "from_result": rid, "w": dw, "h": dh })))
}
