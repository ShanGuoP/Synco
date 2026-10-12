//! 图像服务：把"解码 / 缩放 / 切瓦片"从浏览器搬到服务端。
//!
//! 落盘约定见方案 §4：派生档与原图同目录，档位写进文件名，所以重生成即换名，
//! 换名之后旧 URL 自然失效，新 URL 可以放心 immutable。
//! 这里的函数全是同步重活：异步调用方一律过 `util::image_work`，不要在 handler 里直接调（`*_async` 两个入口已经自带了）。

use crate::error::{AppError, Result};
use crate::models::entity::Image;
use crate::repo;
use crate::state::{Ctx};
use crate::util;
use crate::img::codec;
use serde_json::json;
use stitch_core::{crop_scale_rgba};
use std::path::{Path, PathBuf};

pub const THUMB_EDGE: usize = 320;
pub const TILE: usize = 512;
pub const DEFAULT_PROXY_EDGE: usize = 3072;
const THUMB_Q: u8 = 82;
const PROXY_Q: u8 = 88;
const TILE_Q: u8 = 88;
/// 成图存盘的档位。这一份只当预览与对比用——无损那一张不再整张留盘（24MP 一张 PNG ≈ 21 MB），
/// 导出、下载、另存都从"窗口原图 + 遮罩快照 + 参数快照"重算，见 `queue::lossless_bytes`。
/// 所以这里的 95 是"看着与成图无差别"的选择，不是"作品保真"的选择。
pub const FINAL_Q: u8 = 95;
/// 长边不到两个瓦片就不切：省一次全图重编码，查看器对这种图直接用整图
const TILE_MIN: usize = 1024;

/// proxy 档位：`proxy_edge` 是方案里那个"死旋钮"，钳在 1024–8192
pub fn proxy_edge(ctx: &Ctx) -> usize {
    repo::settings::get(ctx, "proxy_edge")
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|v| (1024..=8192).contains(v))
        .unwrap_or(DEFAULT_PROXY_EDGE)
}

/// 缓存分级只看命名规律，不看扩展名：`_mask.png` 与画稿 `_sketch.png` 都是**被原地覆写**的文件
/// （存遮罩、自动保存画稿、取回快照都改内容不改名），给它们 immutable 就是让浏览器拿旧内容
/// 说话——「取回这一版画稿」提示成功、屏幕上一切没变，正是这么坏掉的。
/// 快照文件名同样落在 `_sketch.png` 上，但它永不覆写：让它一并走 no-cache 只是多一次条件请求，
/// ETag 命中时连文件内容都不读（files::stat_and_read），比把覆写型文件留在一年缓存里便宜。
/// 返回 `(Cache-Control, 是否带 ETag)`。
pub fn cache_policy(name: &str) -> (&'static str, bool) {
    if name.ends_with("_mask.png") || name.ends_with("_sketch.png") {
        ("no-cache", true)
    } else {
        ("public, max-age=31536000, immutable", false)
    }
}

/// 派生档的一族路径：`rel` 是入库用的正斜杠相对路径，`abs` 是盘上路径
#[derive(Clone, Debug)]
pub struct Slot {
    pub rel: String,
    pub abs: PathBuf,
}

fn slot(ctx: &Ctx, pid: i64, name: String) -> Slot {
    let rel = util::rel_path(&["projects".into(), pid.to_string(), name]);
    Slot { abs: ctx.data.join(&rel), rel }
}

fn stem_of(orig_rel: &str) -> String {
    let s = util::stem_of(orig_rel);
    if s.is_empty() { "photo".into() } else { s }
}

pub fn thumb_slot(ctx: &Ctx, img: &Image) -> Slot {
    slot(ctx, img.project_id, format!("{}_thumb.jpg", stem_of(&img.orig_path)))
}

pub fn proxy_slot(ctx: &Ctx, img: &Image, edge: usize) -> Slot {
    slot(ctx, img.project_id, format!("{}_proxy{edge}.jpg", stem_of(&img.orig_path)))
}

pub(crate) fn write_bytes(p: &Path, buf: &[u8]) -> Result<()> {
    let dir = p.parent().ok_or_else(|| AppError::bad("srv.image.noParent"))?;
    std::fs::create_dir_all(dir).map_err(|e| AppError::fail_detail("srv.common.mkdirFail", e))?;
    // 先写 .part 再改名：同一张图可能被导入回调和启动补空当同时加工，
    // 直接覆写会让中间态被 /file/ 读出去（半张 JPEG 在浏览器里就是破图）
    let tmp = p.with_extension("part");
    let path_arg = || json!({ "path": p.display().to_string() });
    std::fs::write(&tmp, buf).map_err(|e| AppError::detailed_args(500, "srv.image.slotWrite", path_arg(), e))?;
    std::fs::rename(&tmp, p).map_err(|e| AppError::detailed_args(500, "srv.image.slotMove", path_arg(), e))
}

/// 涂抹层 → 原图尺寸。前端在 proxy 分辨率上画，本机链路要把蒙版喂给 ComfyUI，
/// 尺寸必须和原图一致，否则 InpaintCrop 的裁切几何会整体错位。
/// 已经是原图尺寸（历史蒙版就是满尺寸的）直接原样返回，不重编码。
pub fn mask_to_orig(ctx: &Ctx, mask_rel: &str, w: usize, h: usize) -> Result<Vec<u8>> {
    let bytes = std::fs::read(util::data_file(&ctx.data, mask_rel).ok_or_else(|| AppError::bad("srv.submit.maskGone"))?)
        .map_err(|e| AppError::fail_detail("srv.image.maskRead", e))?;
    let m = codec::decode(&bytes)?;
    if m.w == w && m.h == h {
        return Ok(bytes);
    }
    Ok(codec::encode_png(&crop_scale_rgba(&m, 0, 0, m.w, m.h, w, h)))
}

/// 删图 / 删项目时连带清掉的派生档：thumb、proxy（旧档位名也可能留过）、瓦片目录
pub fn purge(ctx: &Ctx, img: &Image) {
    for rel in [img.thumb_path.as_deref(), img.proxy_path.as_deref()].into_iter().flatten() {
        let _ = std::fs::remove_file(ctx.data.join(rel));
    }
    // 档位改过的历史 proxy 文件名不同，按前缀扫一遍，别让它们永远躺在目录里
    if let Some(dir) = ctx.data.join(&img.orig_path).parent() {
        let stem = stem_of(&img.orig_path);
        if let Ok(rd) = std::fs::read_dir(dir) {
            let stem = stem.to_lowercase();
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_lowercase();
                if (n.starts_with(&format!("{stem}_thumb.")) || n.starts_with(&format!("{stem}_proxy"))) && e.path().is_file() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    let d = tiles_dir(ctx, img);
    let _ = std::fs::remove_dir_all(&d);
    // 缓存层里 `t/<id>` 与 `s/<id>` 是平铺的，没有"项目下的 tiles/ 空壳"要收了
}


mod cache;
mod derive;
mod pyramid;
pub use cache::{cache_bytes, cache_cap, cache_clear, cache_dir, cache_sweep, sweep_legacy_view_files, tiles_dir};
pub use derive::{derive, result_thumb, spawn_backfill, spawn_derive, write_result_thumb};
pub use pyramid::{pyramid, pyramid_async, tiles, tiles_async, tiles_rel_root};
#[cfg(test)]
use pyramid::level_sizes;
#[cfg(test)]
mod tests;
