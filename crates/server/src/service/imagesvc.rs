//! 图像服务：把"解码 / 缩放 / 切瓦片"从浏览器搬到服务端。
//!
//! 落盘约定见方案 §4：派生档与原图同目录，档位写进文件名，所以重生成即换名，
//! 换名之后旧 URL 自然失效，新 URL 可以放心 immutable。
//! 这里的函数全是同步重活：调用方一律过 `util::blocking`，不要在 handler 里直接调（`*_async` 两个入口已经自带了）。

use crate::error::{AppError, Result};
use crate::models::entity::Image;
use crate::repo;
use crate::state::{Ctx, Shared};
use crate::util;
use crate::img::codec;
use serde_json::json;
use stitch_core::{crop_scale_rgba, resize_rgba, Rgba};
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

/// 可再生档的家：`data/runtime/cache/`。这一层里的东西**随时可以删**——原图、遮罩、窗口原图、
/// 参数快照与数据库都在别处，删掉的代价只是下一次要看时重切一次（24MP 实测发布档约 0.5 秒）。
/// 挑 `runtime/` 是因为它的定义本来就是"进程私有、备份可以不带"，正好对上缓存的语义。
///
/// 判据是"删掉之后**任何时候**都还能算回来"，不是"眼下算得回来"：
/// thumb/proxy 只有原图还在时才能重切（误删事故后那批行就是靠它留着最后的像素），
/// `_adjinput` 那一张的参数会被用户改掉就再也渲不回——所以这两类都留在 `projects/`，
/// 这一层只收瓦片与预览/成图那种"看一眼、下次再看会重算"的档。
pub fn cache_dir(ctx: &Ctx) -> PathBuf {
    crate::runtime_dir(&ctx.data).join("cache")
}

/// 瓦片目录：按 image_id 归位，id 变了（重新导入）就不会命中旧缓存
pub fn tiles_dir(ctx: &Ctx, img: &Image) -> PathBuf {
    cache_dir(ctx).join("t").join(img.id.to_string())
}

/// 缓存里"一张图"粒度的条目集合：`t/<id>` 与 `s/<id>` 底下那一层目录
fn cache_entries(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for group in ["t", "s"] {
        let Ok(rd) = std::fs::read_dir(root.join(group)) else { continue };
        for e in rd.flatten() {
            if e.path().is_dir() {
                out.push(e.path());
            }
        }
    }
    out
}

fn tree_stat(dir: &Path) -> (u64, std::time::SystemTime) {
    let mut bytes = 0u64;
    let mut newest = std::time::UNIX_EPOCH;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if let Ok(md) = e.metadata() {
                bytes += md.len();
                if let Ok(t) = md.modified() {
                    if t > newest {
                        newest = t;
                    }
                }
            }
        }
    }
    (bytes, newest)
}

/// 缓存层现在占多少（设置页要说清"清掉能省多少"）
pub fn cache_bytes(ctx: &Ctx) -> u64 {
    cache_entries(&cache_dir(ctx)).iter().map(|p| tree_stat(p).0).sum()
}

/// 把缓存压进上限之内，**最久没重切过的那张先丢**。
///
/// 粒度是"一张图"：半套瓦片比一整套更糟（屏幕上会看见没切完的那一格），所以整目录一起走。
/// 时间取目录里最新的文件时间——重切会更新它，只读不更新，所以这是"最近重算过"的近似，
/// 不是"最近看过"的精确；缓存满了要腾地方，近似够用。
pub fn cache_sweep(ctx: &Ctx, cap_bytes: u64) -> (usize, u64) {
    if cap_bytes == 0 {
        return (0, 0);
    }
    let root = cache_dir(ctx);
    let mut rows: Vec<(std::time::SystemTime, u64, PathBuf)> =
        cache_entries(&root).into_iter().map(|p| {
            let (b, t) = tree_stat(&p);
            (t, b, p)
        }).collect();
    let total: u64 = rows.iter().map(|r| r.1).sum();
    if total <= cap_bytes {
        return (0, 0);
    }
    rows.sort_by_key(|r| r.0);
    let mut freed = 0u64;
    let mut gone = 0usize;
    for (_, b, p) in rows {
        if total - freed <= cap_bytes {
            break;
        }
        if std::fs::remove_dir_all(&p).is_ok() {
            freed += b;
            gone += 1;
        }
    }
    (gone, freed)
}

/// 全清：设置页那颗「清掉可再生档」。只碰 cache_dir，`projects/` 一个字节都不动
pub fn cache_clear(ctx: &Ctx) -> (usize, u64) {
    let root = cache_dir(ctx);
    let mut gone = 0usize;
    let mut freed = 0u64;
    for p in cache_entries(&root) {
        let (b, _) = tree_stat(&p);
        if std::fs::remove_dir_all(&p).is_ok() {
            gone += 1;
            freed += b;
        }
    }
    (gone, freed)
}

/// 缓存上限（MB）：`0` = 不限。默认 4 GB——比这小的照片集根本碰不到清理
pub fn cache_cap(ctx: &Ctx) -> u64 {
    repo::settings::get(ctx, "cache_cap_mb")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(4096)
        .saturating_mul(1048576)
}

/// 这一族名字是"看一眼就扔"的档：<父图名>_<档种><8 位十六进制指纹>.jpg
fn view_artifact(name: &str) -> bool {
    let Some(rest) = name.strip_suffix(".jpg") else { return false };
    ["_adjprev", "_adjusted", "_adjthumb"].iter().any(|k| {
        rest.find(k).map(|i| {
            let tail = &rest[i + k.len()..];
            tail.len() == 8 && tail.chars().all(|c| c.is_ascii_hexdigit())
        }).unwrap_or(false)
    })
}

fn stat_bytes(path: &Path) -> u64 {
    if path.is_dir() { tree_stat(path).0 } else { path.metadata().map(|m| m.len()).unwrap_or(0) }
}

/// 派生档换了家之后，旧家留下的孤儿收一次：`projects/<号>/tiles/` 整棵，加上散在项目目录里的
/// 预览/成图/成图小档。这些在新家会现切现渲，删掉不丢任何一个像素。
///
/// 名单里没有 `_adjinput`（结果行的 `orig_path` 指着它，参数一改就再也渲不回那一张），
/// 没有 `_thumb`/`_proxy`（原图一旦没了就只剩它，误删事故那批行就是例子），成图那一族更不碰。
/// 判据也收得很紧：尾巴必须是 8 位十六进制指纹再接 `.jpg`，用户自己起的名字删不掉。
pub fn sweep_legacy_view_files(ctx: &Ctx) -> (usize, u64) {
    let mut gone = 0usize;
    let mut freed = 0u64;
    let Ok(projects) = std::fs::read_dir(ctx.data.join("projects")) else { return (0, 0) };
    for p in projects.flatten() {
        let Ok(entries) = std::fs::read_dir(p.path()) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_lowercase();
            let hit = (path.is_dir() && name == "tiles") || (path.is_file() && view_artifact(&name));
            if !hit {
                continue;
            }
            let bytes = stat_bytes(&path);
            let removed = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) }.is_ok();
            if removed {
                gone += 1;
                freed += bytes;
            }
        }
    }
    (gone, freed)
}

/// 瓦片在 `/file/` 下的 URL 根。与 `tiles_dir` 成对改——一个给盘、一个给浏览器，
/// 两边各写一份规则的话，换目录时瓦片会全 404 而测试还是一片绿
pub fn tiles_rel_root(img: &Image) -> String {
    util::rel_path(&["runtime".into(), "cache".into(), "t".into(), img.id.to_string()])
}

/// 金字塔各层尺寸：`v[0]` 是最粗的一层，最后一层就是 base 本身
fn level_sizes(w: usize, h: usize) -> Vec<(usize, usize)> {
    let mut v = vec![(w, h)];
    while v.last().map(|&(a, b)| a.max(b) > TILE).unwrap_or(false) {
        let (a, b) = *v.last().unwrap();
        v.push(((a.div_ceil(2)).max(1), (b.div_ceil(2)).max(1)));
    }
    v.reverse();
    v
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

/// 读原图并解码。库里有过这行不等于盘上还有这个文件，所以这里统一报可读的错。
/// 路径走 `util::data_file`：库里躺着的若是绝对路径或 `..`（被人改过的库），不该跟着它读到 data/ 外。
fn read_orig(ctx: &Ctx, img: &Image) -> Result<Rgba> {
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
    let ctx = ctx.clone();
    let rel = img.orig_path.clone();
    tokio::spawn(async move {
        let Ok(_slot) = ctx.slots.clone().acquire_owned().await else { return };
        // derive 里是一次全分辨率解码加两档重编码，秒级 CPU：留在当前 worker 上等于
        // 把一整条连接按住，其他请求跟着排队，所以要过一道阻塞池
        let ran = util::blocking(move || derive(&ctx, &img)).await;
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
        let _ = tokio::task::spawn_blocking(move || backfill(&ctx)).await;
    });
}

/// 启动后在阻塞线程池里逐张补齐派生档。单张失败只记一行——原图被手动删过是常态，
/// 补不出档就继续用 orig_url，不该因此中断后面几百张。
pub fn backfill(ctx: &std::sync::Arc<Ctx>) {
    let edge = proxy_edge(ctx);
    let rows = match repo::images::list_missing_thumb(ctx, BACKFILL_BATCH) {
        Ok(v) => v,
        Err(e) => {
            println!("  派生档补空当没跑成：{e}");
            return;
        }
    };
    if rows.is_empty() {
        return;
    }
    println!("  正在为 {n} 张存量图补派生档（长边 ≤{edge} 的不切 proxy）…", n = rows.len());
    let mut ok = 0usize;
    let mut bad = 0usize;
    for img in &rows {
        match derive(ctx, img) {
            Ok(true) => ok += 1,
            Ok(false) => {}
            Err(e) => {
                bad += 1;
                if bad <= 3 {
                    println!("  跳过 {}：{}", img.orig_path, e.text());
                }
            }
        }
    }
    println!("  派生档补齐 {ok} 张{t}", t = if bad > 0 { format!("，{bad} 张补不出（原图不在盘上或格式不认识）") } else { String::new() });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 层级尺寸从粗到细且封顶是原尺寸() {
        let v = level_sizes(4000, 6000);
        assert_eq!(*v.last().unwrap(), (4000, 6000));
        assert!(v[0].0.max(v[0].1) <= TILE);
        // 由粗到细逐级翻倍（向上取整），所以细级的一半就是上一级
        for i in 1..v.len() {
            assert_eq!(v[i].0.div_ceil(2), v[i - 1].0);
            assert_eq!(v[i].1.div_ceil(2), v[i - 1].1);
        }
        assert_eq!(level_sizes(300, 200), vec![(300, 200)]);
        assert_eq!(level_sizes(0, 0), vec![(0, 0)]);
    }

    #[test]
    fn 命名规律决定缓存分级() {
        assert_eq!(cache_policy("projects/1/a_mask.png").1, true);
        assert_eq!(cache_policy("projects/1/1234_ab_photo.jpg").1, false);
        assert!(cache_policy("projects/1/a_mask.png").0.contains("no-cache"));
        assert!(cache_policy("projects/1/tiles/3/4/0_0.jpg").0.contains("immutable"));
        // 画稿会被原地覆写（自动保存、取回快照），一年期 immutable 会把旧笔迹钉在浏览器里
        assert!(cache_policy("projects/1/1700_ab_画稿_sketch.png").0.contains("no-cache"));
        assert_eq!(cache_policy("projects/1/1700_ab_画稿_sketch.png").1, true);
    }

    /// 一张 4000×6000（24MP，就是你 data 里那个规格）走完整条派生链：
    /// thumb/proxy/瓦片都要出文件，层级和瓦片数要数得对。跑一次把耗时打出来，别靠估。
    #[test]
    fn 二四mp_的派生档与瓦片全链() {
        use std::time::Instant;
        let dir = std::env::temp_dir().join(format!("synco-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
        let pid = crate::repo::projects::create(&ctx, "大图").unwrap();
        let (w, h) = (4000usize, 6000usize);
        // 渐变叠噪点：纯色对 JPEG 太友好，耗时会假得像成功了
        let mut img = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let n = ((x * 7 + y * 13) % 251) as u8;
                img.set(x, y, [(x % 255) as u8, (y % 255) as u8, n, 255]);
            }
        }
        let rel = util::rel_path(&["projects".into(), pid.to_string(), "big.png".into()]);
        std::fs::create_dir_all(dir.join("projects").join(pid.to_string())).unwrap();
        std::fs::write(dir.join(&rel), codec::encode_png(&img)).unwrap();
        let iid = crate::repo::images::insert(&ctx, pid, "big.png", &rel, w as i64, h as i64).unwrap();
        let row = crate::repo::images::by_id(&ctx, iid).unwrap().unwrap();

        let t = Instant::now();
        assert!(derive(&ctx, &row).unwrap(), "thumb 应当被生成");
        let derive_ms = t.elapsed().as_millis();
        let after = crate::repo::images::by_id(&ctx, iid).unwrap().unwrap();
        assert!(after.thumb_path.as_deref().map(|p| dir.join(p).is_file()).unwrap_or(false), "thumb 要落盘并入库");
        assert!(after.proxy_path.as_deref().map(|p| dir.join(p).is_file()).unwrap_or(false), "24MP 超出档位，proxy 必须出");

        let t = Instant::now();
        let meta = tiles(&ctx, &after).unwrap();
        let tiles_ms = t.elapsed().as_millis();
        assert_eq!(meta["tile"], serde_json::json!(TILE));
        let levels = meta["levels"].as_array().unwrap();
        assert_eq!(levels.last().unwrap()["w"].as_u64().unwrap() as usize, w);
        let mut want = 0usize;
        for l in levels {
            want += (l["cols"].as_u64().unwrap() as usize) * (l["rows"].as_u64().unwrap() as usize);
        }
        let mut got = 0usize;
        count_jpg(&tiles_dir(&ctx, &after), &mut got);
        assert_eq!(got, want, "清单里数出来的瓦片数要和盘上的一一对应");
        assert!(got > 100, "24MP 至少该切出上百张，实得 {got}");
        println!("  24MP 实测（debug 构建）：thumb+proxy {derive_ms}ms，瓦片 {tiles_ms}ms（{got} 张 / {} 级）", levels.len());
        drop(ctx);   // 58MB 的目录以前就是这么一年年留在临时盘上的：连接没撒手，删除在 Windows 上静默失败
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 缓存层的两条规矩：回收**只碰** `runtime/cache/`，丢的是最久没重切的那一张，而且是整目录走
    /// （半套瓦片比一整套更糟）。`projects/` 里那些不可再生的东西永远不可能被这条扫到。
    #[test]
    fn 缓存按上限回收_只碰缓存层() {
        let dir = std::env::temp_dir().join(format!("synco-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
        // 不可再生的那份放 projects/：清理扫到它就算越界
        std::fs::create_dir_all(dir.join("projects").join("1")).unwrap();
        std::fs::write(dir.join("projects").join("1").join("a.png"), vec![b'x'; 4096]).unwrap();
        // 三张图的瓦片目录，各 1024 字节；后建的"更新"
        let root = cache_dir(&ctx).join("t");
        for id in 1..=3 {
            let d = root.join(id.to_string());
            std::fs::create_dir_all(&d).unwrap();
            for k in 0..2 {
                std::fs::write(d.join(format!("{k}.jpg")), vec![b'x'; 512]).unwrap();
            }
        }
        assert_eq!(cache_bytes(&ctx), 3072, "该数出三张图的量");
        // 上限 1600：先丢最久的那张（1024）还超，再丢第二张，剩一张才停
        let (gone, freed) = cache_sweep(&ctx, 1600);
        assert_eq!((gone, freed), (2, 2048), "该丢掉最久没重切的两处");
        assert!(root.join("3").is_dir(), "最新的那张被误丢了");
        assert!(!root.join("1").exists() && !root.join("2").exists(), "旧目录没走干净");
        assert!(dir.join("projects").join("1").join("a.png").is_file(), "清理伸进 projects/ 了");
        // 0 = 不限：多大的缓存都不该动
        assert_eq!(cache_sweep(&ctx, 0), (0, 0));
        let (n, b) = cache_clear(&ctx);
        assert_eq!((n, b), (1, 1024), "手动清一次该把剩下那张带走");
        assert_eq!(cache_bytes(&ctx), 0);
        drop(ctx);   // 连接没撒手的话 Windows 删不掉 app.db
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 搬家之后旧家的孤儿只收"看一眼"那一族。样张跨在判据两侧：尾巴不是 8 位十六进制的
    /// 用户文件名、被结果行指着的 `_adjinput`、原图没了就只剩它的 `_thumb`/`_proxy`，都必须在。
    #[test]
    fn 旧位置的派生档只收那一族() {
        let dir = std::env::temp_dir().join(format!("synco-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
        let p1 = dir.join("projects").join("1");
        std::fs::create_dir_all(p1.join("tiles").join("3").join("0")).unwrap();
        std::fs::write(p1.join("tiles").join("3").join("0").join("0_0.jpg"), vec![b'x'; 700]).unwrap();
        let keep = ["a.png", "a_mask.png", "a_thumb.jpg", "a_proxy1024.jpg",
            "a_adjinputdeadbeef.jpg",              // 结果行的 orig_path 指着它
            "旅行 照片_adjprevzzzzzzzz.jpg"];      // 尾巴不是十六进制指纹：判据两侧的另一半，用户的东西
        for n in &keep { std::fs::write(p1.join(n), vec![b'x'; 100]).unwrap(); }
        let go = ["a_adjprev1a2b3c4d.jpg", "a_adjustedDEADBEEF.jpg", "a_adjthumb00112233.jpg"];
        for n in &go { std::fs::write(p1.join(n), vec![b'x'; 300]).unwrap(); }
        // 这条是全局走的，判据只认名字那一族：别的项目里被库指着的一样不许扫掉
        std::fs::create_dir_all(dir.join("projects").join("2")).unwrap();
        std::fs::write(dir.join("projects").join("2").join("c.png"), vec![b'x'; 300]).unwrap();
        std::fs::write(dir.join("projects").join("2").join("c_adjinput0000abcd.jpg"), vec![b'x'; 300]).unwrap();

        let (n, b) = sweep_legacy_view_files(&ctx);
        // 4 处：三个渲染档 + 那一棵 tiles/（用户文件与记录都不进名单，删了就是事故）
        assert_eq!(n, 3 + 1, "该请走的：预览/成图/成图小档 + tiles/");
        assert_eq!(b, 300 * 3 + 700, "字节账要连瓦片子目录一起数：{b}");
        for name in &keep { assert!(p1.join(name).is_file(), "{name} 不该被碰"); }
        for name in &go { assert!(!p1.join(name).exists(), "{name} 还留在旧家"); }
        assert!(!p1.join("tiles").exists(), "旧瓦片目录没收到");
        assert!(dir.join("projects").join("2").join("c.png").is_file(), "别的项目的原图被扫了");
        assert!(dir.join("projects").join("2").join("c_adjinput0000abcd.jpg").is_file(), "提交记录被扫了");
        // 再跑一次什么都找不到：这条是每次启动都走的，不该留下"第二次要少删点"的状态
        assert_eq!(sweep_legacy_view_files(&ctx), (0, 0));
        drop(ctx);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 递归数瓦片目录里的 jpg
    fn count_jpg(dir: &std::path::Path, n: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if e.path().is_dir() {
                count_jpg(&e.path(), n);
            } else if e.path().extension().map(|x| x == "jpg").unwrap_or(false) {
                *n += 1;
            }
        }
    }
}
