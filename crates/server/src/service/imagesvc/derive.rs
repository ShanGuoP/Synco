//! 缩略图和代理图生成、后台补齐与结果缩略图。
use crate::error::{AppError, Result};
use crate::models::entity::Image;
use crate::state::{Ctx, Shared};
use crate::{img::codec, repo, util};
use stitch_core::Rgba;
use serde_json::json;
use super::{slot, proxy_edge, proxy_slot, thumb_slot, write_bytes, THUMB_EDGE, THUMB_Q, PROXY_Q};
/// 读原图并解码。库里有过这行不等于盘上还有这个文件，所以这里统一报可读的错。
/// 路径走 `util::data_file`：库里躺着的若是绝对路径或 `..`（被人改过的库），不该跟着它读到 data/ 外。
pub(super) fn read_orig(ctx: &Ctx, img: &Image) -> Result<Rgba> {
    let p = util::data_file(&ctx.data, &img.orig_path)
        .ok_or_else(|| AppError::coded_args(500, "srv.image.origGone", json!({ "path": img.orig_path.clone() })))?;
    let bytes = std::fs::read(&p).map_err(|e| AppError::fail_detail("srv.image.origRead", e))?;
    codec::decode(&bytes)
}

/// 生成 thumb（必出）与 proxy（只在原图超出档位时出）。已入库就整体跳过。
/// 返回 `false` 表示没动（已有或原图读不出来，调用方不用重复报错）。
pub fn derive(ctx: &Ctx, img: &Image) -> Result<bool> {
    // 画稿不切派生档：JPEG 没有 alpha，一张空白画布会被拍成一块死黑；
    // 画布视图按 1:1 自己画那张纸，项目卡片也按 kind 走专门的摆位
    if img.is_sketch() {
        return Ok(false);
    }
    if img.thumb_path.is_some() {
        return Ok(false);
    }
    let edge = proxy_edge(ctx);
    let base = read_orig(ctx, img)?;
    // 这一步反正已经把原图解开了：库里那对 w/h 是导入时客户端报的，与真实像素不符就顺手改回来，
    // 否则瓦片摆位与涂抹层的几何会一直歪着
    if (base.w as i64, base.h as i64) != (img.w, img.h) {
        repo::images::set_dims(ctx, img.id, base.w as i64, base.h as i64)?;
    }
    let long = base.w.max(base.h);
    let th = thumb_slot(ctx, img);
    write_bytes(&th.abs, &codec::encode_jpeg(&codec::scale_to_long_edge(&base, THUMB_EDGE), THUMB_Q))?;
    let px = if long > edge {
        let p = proxy_slot(ctx, img, edge);
        write_bytes(&p.abs, &codec::encode_jpeg(&codec::scale_to_long_edge(&base, edge), PROXY_Q))?;
        Some(p.rel)
    } else {
        None
    };
    repo::images::set_derived(ctx, img.id, Some(&th.rel), px.as_deref())?;
    Ok(true)
}

/// 存量补空当：一次最多补这么多张，免得打开一个 500 张的项目就把 CPU 占满
const BACKFILL_BATCH: i64 = 400;

/// 后台补一张图的派生档：导入和"首次读到这张图"都走这条，接口不等着它
pub fn spawn_derive(ctx: &Shared, img: Image) {
    if img.is_sketch() { return; }
    let Some(claim) = ctx.work().claim_derive(img.id) else { return };
    let ctx = ctx.clone();
    let rel = img.orig_path.clone();
    tokio::spawn(async move {
        // derive 里是一次全分辨率解码加两档重编码，秒级 CPU：留在当前 worker 上等于
        // 把一整条连接按住（桌面壳与服务同进程，窗口会跟着卡），所以要过一道阻塞池
        let ran = util::image_work(ctx.clone(), util::image_pixels(&img), move || { let _claim = claim; derive(&ctx, &img) }).await;
        // 补不出来只记一行：原图被手动删过是常态，前端回退 orig_url 就行
        if let Err(e) = ran {
            tracing_lite(&rel, &e.text());
        }
    });
}

fn tracing_lite(rel: &str, e: &str) {
    println!("  派生档没补上 {rel}：{e}");
}

/// 启动后在阻塞线程池里逐张补齐存量派生档
pub fn spawn_backfill(ctx: &Shared) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        if let Ok(rows) = repo::images::list_missing_thumb(&ctx, BACKFILL_BATCH) {
            for img in rows {
                let Some(claim) = ctx.work().claim_derive(img.id) else { continue };
                let c = ctx.clone();
                let _ = util::image_work(ctx.clone(), util::image_pixels(&img), move || { let _claim = claim; derive(&c, &img) }).await;
            }
        }
    });
}

/// 成图的 320 缩略图：历史列靠它，不再回落到 20–33MB 的成图
pub fn result_thumb(ctx: &Ctx, rid: i64, pid: i64, final_rel: &str) -> Option<String> {
    let rel = write_result_thumb(ctx, rid, pid, final_rel)?;
    if repo::results::set_thumb(ctx, rid, &rel).is_err() {
        let _ = std::fs::remove_file(ctx.data.join(&rel));
        return None;
    }
    Some(rel)
}

/// 云端完成操作自己登记缩略图，准备文件时不能提前改结果行。
pub fn write_result_thumb(ctx: &Ctx, rid: i64, pid: i64, final_rel: &str) -> Option<String> {
    let bytes = std::fs::read(util::data_file(&ctx.data, final_rel)?).ok()?;
    let decoded = codec::decode(&bytes).ok()?;
    let th = slot(ctx, pid, format!("r{rid}_{}_thumb.jpg", util::now_ms()));
    if write_bytes(&th.abs, &codec::encode_jpeg(&codec::scale_to_long_edge(&decoded, THUMB_EDGE), THUMB_Q)).is_err() {
        let _ = std::fs::remove_file(&th.abs);
        return None;
    }
    Some(th.rel)
}
