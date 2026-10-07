//! 图像服务：把"解码 / 缩放 / 切瓦片"从浏览器搬到服务端。
//!
//! 落盘约定见方案 §4：派生档与原图同目录，档位写进文件名，所以重生成即换名，
//! 换名之后旧 URL 自然失效，新 URL 可以放心 immutable。
//! 这里的函数全是同步重活，调用方负责丢进 `spawn_blocking`（只有 `*_async` 两个入口碰 tokio）。

use crate::models::entity::Image;
use crate::repo;
use crate::state::{Ctx, Shared};
use crate::util;
use crate::img::codec;
use stitch_core::{crop_scale_rgba, resize_rgba, Rgba};
use std::path::{Path, PathBuf};

pub const THUMB_EDGE: usize = 320;
pub const TILE: usize = 512;
pub const DEFAULT_PROXY_EDGE: usize = 3072;
const THUMB_Q: u8 = 82;
const PROXY_Q: u8 = 88;
const TILE_Q: u8 = 88;
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

/// 瓦片目录：按 image_id 归位，id 变了（重新导入）就不会命中旧缓存
pub fn tiles_dir(ctx: &Ctx, img: &Image) -> PathBuf {
    ctx.data.join("projects").join(img.project_id.to_string()).join("tiles").join(img.id.to_string())
}
fn tiles_rel_root(img: &Image) -> String {
    util::rel_path(&["projects".into(), img.project_id.to_string(), "tiles".into(), img.id.to_string()])
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

pub(crate) fn write_bytes(p: &Path, buf: &[u8]) -> std::result::Result<(), String> {
    let dir = p.parent().ok_or_else(|| "派生档路径没有父目录".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    // 先写 .part 再改名：同一张图可能被导入回调和启动补空当同时加工，
    // 直接覆写会让中间态被 /file/ 读出去（半张 JPEG 在浏览器里就是破图）
    let tmp = p.with_extension("part");
    std::fs::write(&tmp, buf).map_err(|e| format!("写 {} 失败：{e}", p.display()))?;
    std::fs::rename(&tmp, p).map_err(|e| format!("落位 {} 失败：{e}", p.display()))
}

/// 读原图并解码。库里有过这行不等于盘上还有这个文件，所以这里统一报可读的错。
/// 路径走 `util::data_file`：库里躺着的若是绝对路径或 `..`（被人改过的库），不该跟着它读到 data/ 外。
fn read_orig(ctx: &Ctx, img: &Image) -> std::result::Result<Rgba, String> {
    let p = util::data_file(&ctx.data, &img.orig_path)
        .ok_or_else(|| format!("原图文件已不在磁盘上：{}", img.orig_path))?;
    let bytes = std::fs::read(&p).map_err(|e| format!("读原图失败：{e}"))?;
    codec::decode(&bytes)
}

/// 生成 thumb（必出）与 proxy（只在原图超出档位时出）。已入库就整体跳过。
/// 返回 `false` 表示没动（已有或原图读不出来，调用方不用重复报错）。
pub fn derive(ctx: &Ctx, img: &Image) -> std::result::Result<bool, String> {
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
        repo::images::set_dims(ctx, img.id, base.w as i64, base.h as i64).map_err(|e| e.to_string())?;
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
    repo::images::set_derived(ctx, img.id, Some(&th.rel), px.as_deref()).map_err(|e| e.to_string())?;
    Ok(true)
}

/// 存量补空当：一次最多补这么多张，免得打开一个 500 张的项目就把 CPU 占满
const BACKFILL_BATCH: i64 = 400;

/// 同时最多两份派生档在加工：一次全分辨率解码就是 100MB 级，
/// 让阻塞池随便铺开会把内存压穿，比慢几秒严重得多。
static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// 后台补一张图的派生档：导入和"首次读到这张图"都走这条，接口不等着它
pub fn spawn_derive(ctx: &Shared, img: Image) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        let Ok(_slot) = SLOTS.acquire().await else { return };
        // 补不出来只记一行：原图被手动删过是常态，前端回退 orig_url 就行
        if let Err(e) = derive(&ctx, &img) {
            tracing_lite(&img.orig_path, &e);
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
                    println!("  跳过 {}：{e}", img.orig_path);
                }
            }
        }
    }
    println!("  派生档补齐 {ok} 张{t}", t = if bad > 0 { format!("，{bad} 张补不出（原图不在盘上或格式不认识）") } else { String::new() });
}

/// 成图的 320 缩略图：历史列靠它，不再回落到 20–33MB 的成图
pub fn result_thumb(ctx: &Ctx, rid: i64, pid: i64, final_rel: &str) -> Option<String> {
    let bytes = std::fs::read(util::data_file(&ctx.data, final_rel)?).ok()?;
    let decoded = codec::decode(&bytes).ok()?;
    let th = slot(ctx, pid, format!("r{rid}_{}_thumb.jpg", util::now_ms()));
    write_bytes(&th.abs, &codec::encode_jpeg(&codec::scale_to_long_edge(&decoded, THUMB_EDGE), THUMB_Q)).ok()?;
    repo::results::set_thumb(ctx, rid, &th.rel).ok()?;
    Some(th.rel)
}

/// 涂抹层 → 原图尺寸。前端在 proxy 分辨率上画，本机链路要把蒙版喂给 ComfyUI，
/// 尺寸必须和原图一致，否则 InpaintCrop 的裁切几何会整体错位。
/// 已经是原图尺寸（历史蒙版就是满尺寸的）直接原样返回，不重编码。
pub fn mask_to_orig(ctx: &Ctx, mask_rel: &str, w: usize, h: usize) -> std::result::Result<Vec<u8>, String> {
    let bytes = std::fs::read(util::data_file(&ctx.data, mask_rel).ok_or_else(|| "遮罩文件已不在磁盘上".to_string())?)
        .map_err(|e| format!("读遮罩失败：{e}"))?;
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
pub fn tiles(ctx: &Ctx, img: &Image) -> std::result::Result<serde_json::Value, String> {
    let dir = tiles_dir(ctx, img);
    let meta_path = dir.join("meta.json");
    let base = if meta_path.is_file() {
        std::fs::read(&meta_path).map_err(|e| format!("读瓦片清单失败：{e}"))?
    } else {
        let full = read_orig(ctx, img)?;
        if full.w.max(full.h) < TILE_MIN {
            // 这种图整张塞进显存都不心疼，不值得为它维护一套瓦片目录
            let small = serde_json::json!({ "w": full.w, "h": full.h, "tile": 0, "levels": [], "url": format!("/file/{}", img.orig_path) });
            write_bytes(&meta_path, small.to_string().as_bytes())?;
            return Ok(small);
        }
        build_tiles(ctx, img, &full)?;
        std::fs::read(&meta_path).map_err(|e| format!("读瓦片清单失败：{e}"))?
    };
    serde_json::from_slice(&base).map_err(|e| format!("瓦片清单坏了：{e}"))
}

/// 瓦片清单的 tokio 入口：与派生档共用同一对槽。
/// 首次访问那张图要切一套（一整幅解码 + 每层重采样），而编辑器进来可能一次打上好几个请求；
/// 不限流就是 N 份全分辨率解码同时铺开。拿到槽之后 `tiles()` 自己会先看 meta.json，
/// 已经被别人切完的那次就直接读清单回来。
pub async fn tiles_async(ctx: &Shared, img: Image) -> std::result::Result<serde_json::Value, String> {
    let Ok(_slot) = SLOTS.acquire().await else { return Err("瓦片的限流槽已经关了".into()) };
    let ctx2 = ctx.clone();
    tokio::task::spawn_blocking(move || tiles(&ctx2, &img)).await.unwrap_or_else(|e| Err(format!("瓦片线程崩了：{e}")))
}

fn build_tiles(ctx: &Ctx, img: &Image, full: &Rgba) -> std::result::Result<(), String> {
    let sizes = level_sizes(full.w, full.h);
    let root = tiles_rel_root(img);
    let dir = tiles_dir(ctx, img);
    // 从最细一层开始，逐级减半往下走：每级只重采样上一级，比每级都从 24MP 原图重采便宜得多
    let mut cur = full.clone();
    let mut levels = Vec::with_capacity(sizes.len());
    for z in (0..sizes.len()).rev() {
        let (w, h) = sizes[z];
        if cur.w != w || cur.h != h {
            return Err(format!("瓦片层级算不通：期望 {w}×{h}，实有 {}×{}", cur.w, cur.h));
        }
        let cols = w.div_ceil(TILE);
        let rows = h.div_ceil(TILE);
        let jobs: Vec<(usize, usize, usize, usize, PathBuf)> = (0..rows)
            .flat_map(|ty| (0..cols).map(move |tx| (tx, ty)))
            .map(|(tx, ty)| (tx * TILE, ty * TILE, TILE.min(w - tx * TILE), TILE.min(h - ty * TILE), dir.join(z.to_string()).join(format!("{tx}_{ty}.jpg"))))
            .collect();
        // 单张瓦片编码不到 1ms，但一次要出上百张，分核跑完比串行快一个数量级
        let err: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
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
    // 项目里的 tiles/ 空壳也顺手收掉：非空时 remove_dir 自己会失败，不用先数一遍
    if let Some(parent) = d.parent() {
        let _ = std::fs::remove_dir(parent);
    }
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
