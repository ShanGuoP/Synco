//! 瓦片金字塔布局、生成及带资源准入的异步入口。
use crate::error::{AppError, Result};
use crate::models::entity::Image;
use crate::state::{Ctx, Shared};
use crate::{img::codec, util};
use serde_json::json;
use stitch_core::{crop_scale_rgba, resize_rgba, Rgba};
use std::path::{Path, PathBuf};
use super::{tiles_dir, write_bytes, TILE, TILE_Q, TILE_MIN};
use super::derive::read_orig;
/// 瓦片在 `/file/` 下的 URL 根。与 `tiles_dir` 成对改——一个给盘、一个给浏览器，
/// 两边各写一份规则的话，换目录时瓦片会全 404 而测试还是一片绿
pub fn tiles_rel_root(img: &Image) -> String {
    util::rel_path(&["runtime".into(), "cache".into(), "t".into(), img.id.to_string()])
}

/// 金字塔各层尺寸：`v[0]` 是最粗的一层，最后一层就是 base 本身
pub(super) fn level_sizes(w: usize, h: usize) -> Vec<(usize, usize)> {
    let mut v = vec![(w, h)];
    while v.last().map(|&(a, b)| a.max(b) > TILE).unwrap_or(false) {
        let (a, b) = *v.last().unwrap();
        v.push(((a.div_ceil(2)).max(1), (b.div_ceil(2)).max(1)));
    }
    v.reverse();
    v
}

/// 瓦片金字塔：base 是原图（全分辨率），所以 1:1 与放大看的是真实像素。
/// meta.json 在目录里就是"切完了"的凭据；中途崩了下次重切，不留半套。
/// 返回的 JSON 直接给前端：`{w,h,tile,levels:[{z,w,h,cols,rows}],url}`，
/// `url` 是瓦片路径模板，前端把 `{z}_{x}_{y}` 换成实际编号。
pub fn tiles(ctx: &Ctx, img: &Image) -> Result<serde_json::Value> {
    pyramid(&tiles_dir(ctx, img), &tiles_rel_root(img), || read_orig(ctx, img), &format!("/file/{}", img.orig_path))
}

/// 通用的一套金字塔：`dir` 是瓦片目录，`root` 是它在库内的相对路径（进 URL 用），
/// `decode` 只在清单缺失时才会被调（切一套要一整幅解码，命中清单就不该付那份钱）。
/// 源图与「本地调整」的成图走这同一条，两者只差在目录多一层、源文件是渲染出来的那一张。
pub fn pyramid(
    dir: &Path,
    root: &str,
    decode: impl FnOnce() -> Result<Rgba>,
    whole_url: &str,
) -> Result<serde_json::Value> {
    let meta_path = dir.join("meta.json");
    let base = if meta_path.is_file() {
        std::fs::read(&meta_path).map_err(|e| AppError::fail_detail("srv.tile.metaRead", e))?
    } else {
        let full = decode()?;
        if full.w.max(full.h) < TILE_MIN {
            // 这种图整张塞进显存都不心疼，不值得为它维护一套瓦片目录
            let small = serde_json::json!({ "w": full.w, "h": full.h, "tile": 0, "levels": [], "url": whole_url });
            write_bytes(&meta_path, small.to_string().as_bytes())?;
            return Ok(small);
        }
        build_tiles_at(dir, root, &full)?;
        std::fs::read(&meta_path).map_err(|e| AppError::fail_detail("srv.tile.metaRead", e))?
    };
    serde_json::from_slice(&base).map_err(|e| AppError::fail_detail("srv.tile.metaBad", e))
}

/// 瓦片清单的 tokio 入口：与派生档共用同一对槽。
/// 首次访问那张图要切一套（一整幅解码 + 每层重采样），而编辑器进来可能一次打上好几个请求；
/// 不限流就是 N 份全分辨率解码同时铺开。拿到槽之后 `pyramid()` 自己会先看 meta.json，
/// 已经被别人切完的那次就直接读清单回来。
pub async fn tiles_async(ctx: &Shared, img: Image) -> Result<serde_json::Value> {
    let ctx2 = ctx.clone();
    pyramid_async(ctx, move || tiles(&ctx2, &img)).await
}

/// 切一套瓦片的公共入口：源图与「本地调整」的成图共用这一对限流槽，
/// 因为两者的开销是同一件事——一整幅解码 + 每层重采样 + 上百张编码。
/// `work` 由调用方决定切哪张图（调整那条要先读库里的参数）。
/// 槽位在 `Ctx::slots` 上而不是模块级 static：单测各造各的 Ctx，量出来的耗时才不被别人抢槽污染。
pub async fn pyramid_async(ctx: &Shared, work: impl FnOnce() -> Result<serde_json::Value> + Send + 'static) -> Result<serde_json::Value> {
    let Ok(_slot) = ctx.slots.clone().acquire_owned().await else { return Err(AppError::fail("srv.tile.slotClosed")) };
    // 崩了的那次拿不到返回值，只能报"线程没了"，细节是 JoinError 那句原文
    tokio::task::spawn_blocking(work).await.map_err(|e| AppError::fail_detail("srv.tile.threadPanic", e))?
}

fn build_tiles_at(dir: &Path, root: &str, full: &Rgba) -> Result<()> {
    let sizes = level_sizes(full.w, full.h);
    // 从最细一层开始，逐级减半往下走：每级只重采样上一级，比每级都从 24MP 原图重采便宜得多
    let mut cur = full.clone();
    let mut levels = Vec::with_capacity(sizes.len());
    for z in (0..sizes.len()).rev() {
        let (w, h) = sizes[z];
        if cur.w != w || cur.h != h {
            return Err(AppError::bad_args("srv.tile.levelMismatch", json!({ "w": w, "h": h, "aw": cur.w, "ah": cur.h })));
        }
        let cols = w.div_ceil(TILE);
        let rows = h.div_ceil(TILE);
        let jobs: Vec<(usize, usize, usize, usize, PathBuf)> = (0..rows)
            .flat_map(|ty| (0..cols).map(move |tx| (tx, ty)))
            .map(|(tx, ty)| (tx * TILE, ty * TILE, TILE.min(w - tx * TILE), TILE.min(h - ty * TILE), dir.join(z.to_string()).join(format!("{tx}_{ty}.jpg"))))
            .collect();
        // 单张瓦片编码不到 1ms，但一次要出上百张，分核跑完比串行快一个数量级
        let err: std::sync::Mutex<Option<AppError>> = std::sync::Mutex::new(None);
        let src = &cur;
        let nt = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(jobs.len().max(1));
        std::thread::scope(|s| {
            for grp in jobs.chunks(jobs.len().div_ceil(nt).max(1)) {
                let err = &err;
                s.spawn(move || {
                    for (x, y, sw, sh, out) in grp {
                        if err.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                            return;
                        }
                        let cut = crop_scale_rgba(src, *x, *y, *sw, *sh, *sw, *sh);
                        if let Some(e) = write_bytes(out, &codec::encode_jpeg(&cut, TILE_Q)).err() {
                            *err.lock().unwrap_or_else(|e2| e2.into_inner()) = Some(e);
                        }
                    }
                });
            }
        });
        if let Some(e) = err.into_inner().ok().flatten() {
            return Err(e);
        }
        levels.push(serde_json::json!({ "z": z, "w": w, "h": h, "cols": cols, "rows": rows }));
        if z > 0 {
            let (nw, nh) = sizes[z - 1];
            cur = resize_rgba(src, nw, nh);
        }
    }
    levels.reverse();
    let meta = serde_json::json!({
        "w": full.w, "h": full.h, "tile": TILE, "levels": levels,
        "url": format!("/file/{root}/{{z}}/{{x}}_{{y}}.jpg"),
    });
    write_bytes(&dir.join("meta.json"), meta.to_string().as_bytes())
}
