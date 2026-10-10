//! 本地精修的服务端编排：存参数、出预览、落成图、另存为新图，以及"AI 重绘吃调整后的图"那一步。
//!
//! axum 的解包全在 `api::adjust`，这一层只收已经拆好的值（分层纪律 R3 钉的就是这件事）。
//! 所有同步重活（读盘、解码、算链、编码、写盘）都在这里，handler 那侧统一过阻塞池。
//!
//! 派生档命名：缓存层 `runtime/cache/s/<图号>/adjprev<参数指纹>.jpg`（预览）、`adjusted…`（成图）、
//! `adjthumb…`（成图的 320 档），只有提交给 AI 的那一张 `<名>_adjinput<参数指纹>.jpg` 留在 `projects/`。
//! 指纹进名字是为了**换参数就换 URL**——新 URL 可以放心 immutable，
//! 同名文件存在也就等价于"这套参数已经渲过了"，重复点预览直接复用而不重算。

use crate::error::{AppError, Result};
use crate::img::codec;
use crate::models::entity::Image;
use crate::repo::{adjust as radj, images as rimg, projects as rproj};
use crate::service::imagesvc;
use crate::state::Ctx;
use crate::util;
use photoedit_core::{apply_chain, Chain, EditOps, FaceShape, LutTable};
use serde_json::{json, Value};
use stitch_core::{Alpha, Rgba};
use std::path::PathBuf;

/// 预览档质量：与 proxy 同一档偏上，滑杆拖动时要看得清细节
pub const PREV_Q: u8 = 90;
/// 成图档质量：落盘产物按项目惯例走 JPEG，只有蒙版才用 PNG
pub const RENDER_Q: u8 = 92;
/// 成图旁边那张 320 小档的质量，与 thumb 同一档
const ADJ_THUMB_Q: u8 = 82;

/// 算链的两个档位。预览走 proxy（长边由 `proxy_edge` 那个旋钮决定），成图走原分辨率。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grade {
    Proxy,
    Full,
}

/// 一次渲染的产出：库内相对路径、盘上路径、耗时、"有没有复用旧文件"与成图的实际宽高。
pub struct Built {
    pub rel: String,
    pub abs: PathBuf,
    pub ms: u128,
    pub reused: bool,
    pub w: usize,
    pub h: usize,
}

/// 走完几何段之后这张图会是多大。渲染之前就能算出来（内核与 `apply` 共用同一套取整），
/// 复用旧文件那一条分支靠它给出宽高，不用为此再解一次盘。
pub fn out_size(img: &Image, ops: &EditOps) -> (usize, usize) {
    photoedit_core::geometry::out_size(img.w.max(1) as usize, img.h.max(1) as usize, &ops.geometry)
}

/// 参数链的内容指纹。fnv1a 够用了：它只用来判"这套参数渲过没有"，不当身份凭证。
pub fn ops_hash(json: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in json.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:016x}", h)[..8].to_string()
}

fn stem(img: &Image) -> String {
    let s = util::stem_of(&img.orig_path);
    if s.is_empty() {
        "photo".into()
    } else {
        s
    }
}

/// 给人看的那截名字取库里的 name，不取文件名：导入时文件名前会拼上时间戳与随机缀
/// （`1791445033272_d6f7_sample.png`），拿它当"这张图叫什么"会在胶片条上长出一串数字。
/// 派生档仍按 `stem()` 走文件名，那一路要的是与源文件同前缀的稳定命名。
fn label_stem(img: &Image) -> String {
    let s = util::stem_of(&img.name);
    if s.is_empty() {
        stem(img)
    } else {
        s
    }
}

/// 调整档住在缓存层（`runtime/cache/s/<图号>/`）：这三张全是"原图 + 库里那套参数"渲出来的，
/// 删了下次重渲，一个像素都不会丢。文件名不再带父图名——按 image_id 归位，
/// 删这张图时整目录请走，不用再去扫前缀。
fn slot(ctx: &Ctx, img: &Image, kind: &str, hash: &str) -> (String, PathBuf) {
    let rel = util::rel_path(&["runtime".into(), "cache".into(), "s".into(), img.id.to_string(), format!("{kind}{hash}.jpg")]);
    (rel.clone(), ctx.data.join(&rel))
}

/// 提交给 AI 的那一张是**记录**不是缓存，仍落 `projects/`：路径写进了结果行的 `orig_path`，
/// 「对比原图」按它取图；而那套参数之后会被用户改掉，改完就再也渲不回当时发出去的那一张。
/// 同一套参数只写一份（名字里有指纹），所以重复提交不会越长越多。
fn input_slot(ctx: &Ctx, img: &Image, hash: &str) -> (String, PathBuf) {
    let rel = util::rel_path(&["projects".into(), img.project_id.to_string(), format!("{}_adjinput{hash}.jpg", stem(img))]);
    (rel.clone(), ctx.data.join(&rel))
}

/// 这张图在缓存层里的那一个目录（`purge_all` 与单测共用同一个构造处，规则别写两遍）
pub fn slots_dir(ctx: &Ctx, img: &Image) -> PathBuf {
    imagesvc::cache_dir(ctx).join("s").join(img.id.to_string())
}

/// 提交记录的名字前缀：删这张图时按它扫项目目录，判"什么算这张图的"只用这一个口径
pub fn input_prefix(img: &Image) -> String {
    format!("{}_adjinput", stem(img)).to_lowercase()
}

/// 读库里的参数。JSON 坏了不报错而是退全默认并在标准输出说一声：
/// 一张能看的图不该因为一次写坏的参数就整条链路打不开。
pub fn load_ops(ctx: &Ctx, id: i64) -> EditOps {
    match radj::ops_of(ctx, id) {
        Ok(Some(raw)) => match EditOps::parse(&raw) {
            Ok((mut ops, _)) => {
                ops.clamp();
                ops
            }
            Err(e) => {
                eprintln!("  图 {id} 的调整参数读不出（{e}），按没调整处理");
                EditOps::default()
            }
        },
        Ok(None) => EditOps::default(),
        Err(e) => {
            eprintln!("  图 {id} 的调整参数查库失败（{e}），按没调整处理");
            EditOps::default()
        }
    }
}

/// 这张图有没有被调整过（AI 链路用它决定要不要动输入）
pub fn adjusted(ctx: &Ctx, img: &Image) -> bool {
    !load_ops(ctx, img.id).is_identity()
}

fn orig_rgba(ctx: &Ctx, img: &Image) -> Result<Rgba> {
    let p = util::data_file(&ctx.data, &img.orig_path).ok_or_else(|| AppError::bad("srv.image.origOutside"))?;
    let bytes = std::fs::read(&p).map_err(|e| AppError::fail_detail("srv.image.origRead", e))?;
    codec::decode(&bytes)
}

/// 涂抹层：库里存的是 proxy 分辨率，按当前工作尺寸重采样后再取 alpha。
/// 没涂过就是 `None`，美颜按全图走——正是拍板的那句"没涂 = 全图温和"。
fn mask_plane(ctx: &Ctx, img: &Image, w: usize, h: usize) -> Result<Option<Alpha>> {
    let Some(rel) = img.mask_path.clone().filter(|s| !s.is_empty()) else { return Ok(None) };
    if util::data_file(&ctx.data, &rel).is_none() {
        return Ok(None);
    }
    let png = imagesvc::mask_to_orig(ctx, &rel, w, h)?;
    Ok(Some(codec::decode(&png)?.alpha()))
}

/// LUT 文件：只认 `data/luts/` 里的单层文件名。
/// 名字是参数里来的，`..`、绝对路径、子目录一概不读。
fn lut_table(ctx: &Ctx, name: &str) -> Result<Option<LutTable>> {
    if name.is_empty() {
        return Ok(None);
    }
    let dir = ctx.data.join("luts");
    let p = dir.join(name);
    if !util::inside(&dir, &p) || p.parent().map(|x| x != dir).unwrap_or(true) {
        return Err(AppError::bad_args("srv.adjust.lutName", json!({ "name": name.to_string() })));
    }
    let text = std::fs::read_to_string(&p).map_err(|e| AppError::detail("srv.adjust.lutRead", e))?;
                // 细节自己也是一条带钥匙的理由：中英各查各的，不拼半句
let table = photoedit_core::parse_cube(&text).map_err(|e| {
            AppError::bad_args("srv.adjust.lutParse", json!({ "msg": { "code": e.code, "args": e.args } }))
        })?;
    Ok(Some(table))
}

/// 一键塑形要的那组控制点。关键点表是空的（还没检出过、或这条链路没接上）就 `None`，
/// 链上的自动变形整段跳过——手动液化不依赖它，照常可用。
fn shape_for(ctx: &Ctx, img: &Image) -> Option<FaceShape> {
    crate::service::face::cached_shape(ctx, img)
}

/// 把参数链作用到这张图上（同步重活，调用方负责过阻塞池）
pub fn pixels(ctx: &Ctx, img: &Image, ops: &EditOps, grade: Grade) -> Result<Rgba> {
    let base = match grade {
        Grade::Proxy => match img.proxy_path.as_deref().and_then(|r| util::data_file(&ctx.data, r)) {
            // 有 proxy 档就读它：24MP 原图解开一次是几百毫秒，滑杆每动一下都付这个钱不值得
            Some(p) if p.is_file() => codec::decode(&std::fs::read(&p).map_err(|e| AppError::fail_detail("srv.image.proxyRead", e))?)?,
            _ => codec::scale_to_long_edge(&orig_rgba(ctx, img)?, imagesvc::proxy_edge(ctx)),
        },
        Grade::Full => orig_rgba(ctx, img)?,
    };
    let mask = if ops.beauty.by_mask { mask_plane(ctx, img, base.w, base.h)? } else { None };
    let shape = if ops.warp.auto.is_empty() { None } else { shape_for(ctx, img) };
    let table = match ops.lut.as_ref() {
        Some(l) => lut_table(ctx, &l.name)?,
        None => None,
    };
    let chain = Chain { mask: mask.as_ref(), shape: shape.as_ref(), lut: table.as_ref() };
    Ok(apply_chain(&base, ops, &chain))
}

/// 预览：算 → 落 `_adjprev<指纹>.jpg` → 回 URL。同参数已有文件直接复用。
/// 报出去的宽高是**成图那一档**（`out_size`），不是这张 proxy 档自己的：
/// 前端拿它当画幅尺寸，报了预览档的尺寸就等于把画布缩到 proxy 那么大——图会当场小一圈，
/// 1:1 与放大看到的也只是那张糊图。两条分支（新建与复用）必须同一口径，否则拖两下滑杆画幅会跳。
pub fn build_preview(ctx: &Ctx, img: &Image, ops: &EditOps) -> Result<Built> {
    let h = ops_hash(&ops.to_json());
    let (rel, abs) = slot(ctx, img, "adjprev", &h);
    let (w, hh) = out_size(img, ops);
    if abs.is_file() {
        return Ok(Built { rel, abs, ms: 0, reused: true, w, h: hh });
    }
    let t0 = util::now_ms();
    let out = pixels(ctx, img, ops, Grade::Proxy)?;
    imagesvc::write_bytes(&abs, &codec::encode_jpeg(&out, PREV_Q))?;
    // 旧参数的预览档没人再引用了，扫同一张图的前缀清掉，别让目录越堆越厚
    purge_old(ctx, img, "adjprev", &h);
    Ok(Built { rel, abs, ms: util::now_ms().saturating_sub(t0), reused: false, w, h: hh })
}

/// 成图：原分辨率落 `_adjusted<指纹>.jpg`，并配一张 320 小档给列表用。
/// 返回的第二个值是缩略档的相对路径（缩略档写失败不影响成图，回 `None` 就行）。
pub fn build_render(ctx: &Ctx, img: &Image, ops: &EditOps) -> Result<(Built, Option<String>)> {
    let h = ops_hash(&ops.to_json());
    let (rel, abs) = slot(ctx, img, "adjusted", &h);
    let (th_rel, th_abs) = slot(ctx, img, "adjthumb", &h);
    if abs.is_file() {
        let (w, hh) = out_size(img, ops);
        return Ok((Built { rel, abs, ms: 0, reused: true, w, h: hh }, Some(th_rel)));
    }
    let t0 = util::now_ms();
    let out = pixels(ctx, img, ops, Grade::Full)?;
    imagesvc::write_bytes(&abs, &codec::encode_jpeg(&out, RENDER_Q))?;
    let small = codec::scale_to_long_edge(&out, imagesvc::THUMB_EDGE);
    let thumb = imagesvc::write_bytes(&th_abs, &codec::encode_jpeg(&small, ADJ_THUMB_Q)).map(|_| th_rel.clone()).ok();
    Ok((Built { rel, abs, ms: util::now_ms().saturating_sub(t0), reused: false, w: out.w, h: out.h }, thumb))
}

/// 提交给工作流的那张遮罩：与 `submit_artifact` 同一个域。
///
/// 涂抹层存的是 proxy 分辨率，先上采样到**源图**尺寸（工作流里 DrawMaskOnImage 要和照片同幅），
/// 再跟照片一样过一遍几何段。少了后半段，转 90° 的照片配一张没转的遮罩，
/// `InpaintCropImproved` 圈到的就是另一块地方——而且工作流不会抱怨，它只会照单画。
/// 遮罩的笔迹同时活在 R 与 A 两条通道里（ComfyUI 读 red、内核读 alpha），所以整幅 RGBA 一起转。
pub fn submit_mask(ctx: &Ctx, img: &Image) -> Result<Vec<u8>> {
    let ops = load_ops(ctx, img.id);
    let rel = img.mask_path.clone().unwrap_or_default();
    let mut png = imagesvc::mask_to_orig(ctx, &rel, img.w.max(1) as usize, img.h.max(1) as usize)?;
    if photoedit_core::geometry::needs_pass(&ops.geometry) {
        let m = codec::decode(&png)?;
        png = codec::encode_png(&photoedit_core::geometry::apply(&m, &ops.geometry));
    }
    Ok(png)
}

/// 调整视图的瓦片：切的是**成图**（原分辨率）那一张。
/// 预览档只有 proxy 那么粗，而 1:1 与放大要看的正是真实像素——与源图那套同一个口径，
/// 差别只在目录多一层参数指纹：换参数就换 URL，既不会命中旧内容的 immutable 缓存，
/// 也不会把没渲过的档位算第二遍。
pub fn tiles(ctx: &Ctx, img: &Image, ops: &EditOps) -> Result<Value> {
    let (b, _) = build_render(ctx, img, ops)?;
    let h = ops_hash(&ops.to_json());
    let abs = b.abs.clone();
    let url = format!("/file/{}", b.rel);
    let out = imagesvc::pyramid(&adj_tile_dir(ctx, img, &h), &adj_tile_rel(img, &h), || {
        let bytes = std::fs::read(&abs).map_err(|e| AppError::fail_detail("srv.image.renderRead", e))?;
        codec::decode(&bytes)
    }, &url)?;
    purge_adj_tiles(ctx, img, &h);
    Ok(out)
}

fn adj_tile_dir(ctx: &Ctx, img: &Image, hash: &str) -> PathBuf {
    imagesvc::tiles_dir(ctx, img).join(format!("adj{hash}"))
}

/// 调整档瓦片的 URL 根：跟着 `imagesvc::tiles_rel_root` 走，路径规则只有一处
fn adj_tile_rel(img: &Image, hash: &str) -> String {
    format!("{}/adj{hash}", imagesvc::tiles_rel_root(img))
}

/// 只留当前这套参数的瓦片：一次要出上百张，换一根滑杆就再长一整目录，旧的没人引用
fn purge_adj_tiles(ctx: &Ctx, img: &Image, keep_hash: &str) {
    let parent = imagesvc::tiles_dir(ctx, img);
    let keep = format!("adj{keep_hash}");
    if let Ok(rd) = std::fs::read_dir(&parent) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.starts_with("adj") && n != keep && e.path().is_dir() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

/// 清掉同一张图、同一种档的旧参数版本（只留当前这一份）
fn purge_old(ctx: &Ctx, img: &Image, kind: &str, keep_hash: &str) {
    let dir = slots_dir(ctx, img);
    // keep 必须是**整条文件名**：只比后半段会把刚写好的那一份也当成旧文件请走
    let keep = format!("{kind}{keep_hash}.jpg");
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.starts_with(kind) && n != keep && e.path().is_file() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// 删图时清掉这张图的全部调整档。缓存层那三张整目录请走就够了；
/// 提交记录那张（`_adjinput`）在 `projects/` 里、名字按父图 stem 拼，库里只有指向它的结果行
/// 而不留文件清单，只能按前缀扫一遍——前缀带着 stem，不会碰到同项目别的图。
pub fn purge_all(ctx: &Ctx, img: &Image) {
    let _ = std::fs::remove_dir_all(slots_dir(ctx, img));
    let prefix = input_prefix(img);
    // 目录从原图那条路径倒推，且必须过 data_file：库里躺着绝对路径或 `..`（被人改过的库）时不该扫到 data/ 外
    let dir = util::data_file(&ctx.data, &img.orig_path).and_then(|p| p.parent().map(|d| d.to_path_buf()));
    if let Some(dir) = dir {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() && e.file_name().to_string_lossy().to_lowercase().starts_with(&prefix) {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    }
}

/// 保存前的收口：滑杆夹逼 + 轨迹抽稀。
/// 抽稀容差按当前图的长边换成归一化量，这样"1px"的判据与画幅无关。
pub fn normalize(ops: &mut EditOps, long_edge: usize) -> Vec<String> {
    let mut rep = ops.clamp();
    let tol = 1.0 / (long_edge.max(1) as f32);
    for s in ops.warp.strokes.iter_mut() {
        let n = s.points.len();
        s.points = photoedit_core::simplify(&s.points, tol);
        if s.points.len() != n {
            rep.push(format!("warp.strokes.points(-{})", n - s.points.len()));
        }
    }
    rep
}

/// 「另存为新图」：把成图复制成一条独立图片行的原图，父图是这张。
/// 走的是与结果行 fork 同一条谱系（`derived_from`），项目页的派生入口因此不用改。
pub fn fork_from(ctx: &Ctx, img: &Image, built_rel: &str, w: i64, h: i64) -> Result<i64> {
    let src = util::data_file(&ctx.data, built_rel).ok_or_else(|| AppError::bad("srv.adjust.renderGone"))?;
    let name = format!("{}_{r4} 调整.jpg", util::now_ms(), r4 = util::r4()); // i18n-keep
    let rel = util::rel_path(&["projects".into(), img.project_id.to_string(), name]);
    let dst = ctx.data.join(&rel);
    // 复制一份而不是改名：调整档要留在父图名下，用户回退参数时还能拿它判新
    std::fs::copy(&src, &dst).map_err(|e| AppError::fail_detail("srv.adjust.forkFail", e))?;
    let id = rimg::insert_derived(ctx, img.project_id, &format!("{} 调整.jpg", label_stem(img)), &rel, w, h, img.id, 0)?; // i18n-keep
    // 项目卡片的"最近改动"要跟着走，与导入/另存同一惯例
    rproj::touch(ctx, img.project_id)?;
    Ok(id)
}

/// 提交给 AI 重绘的输入字节（拍板 4：调整后的图，所见即所得）。
/// 没动参数就原样交文件字节——"未调整图片的行为与 0.2.1 逐字段一致"靠的就是这个分支。
pub fn submit_bytes(ctx: &Ctx, img: &Image) -> Result<Vec<u8>> {
    Ok(submit_artifact(ctx, img)?.0)
}

/// 同上，但连带给出"这一张落在盘上的哪条路径"。
/// 调整过的图要把**真正发出去的那一张**留在库里当结果行的原图：不然「对比原图」
/// 拿一张没裁切、没调色的旧文件去比刚回来的成图，看着就像程序出了错。
/// 同一套参数只写一次（文件名里有参数指纹），重复提交直接复用。
pub fn submit_artifact(ctx: &Ctx, img: &Image) -> Result<(Vec<u8>, String)> {
    let ops = load_ops(ctx, img.id);
    if ops.is_identity() {
        let p = util::data_file(&ctx.data, &img.orig_path).ok_or_else(|| AppError::bad("srv.image.origOutside"))?;
        return Ok((std::fs::read(&p).map_err(|e| AppError::fail_detail("srv.image.origRead", e))?, img.orig_path.clone()));
    }
    let bytes = codec::encode_png(&pixels(ctx, img, &ops, Grade::Full)?);
    let (rel, abs) = input_slot(ctx, img, &ops_hash(&ops.to_json()));
    if !abs.is_file() {
        imagesvc::write_bytes(&abs, &bytes)?;
    }
    Ok((bytes, rel))
}

/// 提交给 AI 重绘的像素（云端缝合那一路要在内存里接着算，不能再解一次盘）
pub fn photo(ctx: &Ctx, img: &Image) -> Result<Rgba> {
    photo_with(ctx, img, &load_ops(ctx, img.id))
}

/// 同上，但参数由调用方给——云端那条链一次要同时知道"有没有调整"和"调整成什么样"，
/// 分两次读库就是把同一条查询跑两遍
pub fn photo_with(ctx: &Ctx, img: &Image, ops: &EditOps) -> Result<Rgba> {
    if ops.is_identity() {
        return orig_rgba(ctx, img);
    }
    pixels(ctx, img, ops, Grade::Full)
}

/// 面板要的初始状态：参数 + 内置预设 + 盘上可用的 LUT 名单。
/// LUT 名单从 `data/luts/` 现扫，只认 `.cube`，按名字排序——前端不需要第二个接口就能把下拉摆满。
pub fn panel(ctx: &Ctx, id: i64) -> Result<Value> {
    let ops = load_ops(ctx, id);
    let mut luts: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(ctx.data.join("luts")) {
        for e in rd.flatten() {
            let p = e.path();
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                if name.to_lowercase().ends_with(".cube") && p.is_file() {
                    luts.push(name.to_string());
                }
            }
        }
    }
    luts.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
    // 预设的数值只有 color::preset 那一份：前端摆滑杆要的是同一组数，不再抄一遍表
    let presets: Vec<Value> = photoedit_core::PRESETS.iter()
        .filter_map(|(k, label)| photoedit_core::preset(k).map(|c| json!({ "id": k, "name": label, "color": c })))
        .collect();
    Ok(json!({ "ops": ops, "luts": luts, "presets": presets }))
}

/// 预览接口的响应形状。`w`/`h` 是这张预览的实际宽高：
/// 前端的裁切 overlay 要按它摆位（转 90° 之后换边长，客户端不该再算一遍），
/// `identity` 让前端在"没参数"时直接沿用现有档位，不白跑一次渲染。
pub fn preview_json(b: &Built) -> Value {
    json!({ "preview_url": format!("/file/{}", b.rel), "w": b.w, "h": b.h, "ms": b.ms, "reused": b.reused })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{db, images as rimg};
    use crate::state::Ctx;

    /// 一个隔离的小库：临时目录 + 一张真的 PNG。测试绝不碰用户的 data/
    fn fixture(name: &str) -> (std::path::PathBuf, std::sync::Arc<Ctx>, Image) {
        let dir = std::env::temp_dir().join(format!("synco-adj-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = Ctx::new(dir.clone(), dir.clone(), db::open(&dir).unwrap());
        crate::repo::projects::create(&ctx, "测试项目").unwrap();
        // 一张 40×20 的渐变图：够小、又不退化（左右两半颜色不同，调色后能有可见差别）
        let mut img = Rgba::new(40, 20);
        for y in 0..20 {
            for x in 0..40 {
                img.set(x, y, [(x * 6) as u8, (y * 12) as u8, 128, 255]);
            }
        }
        let rel = util::rel_path(&["projects".into(), "1".into(), "p.png".into()]);
        std::fs::create_dir_all(dir.join("projects").join("1")).unwrap();
        std::fs::write(dir.join(&rel), codec::encode_png(&img)).unwrap();
        let id = rimg::insert(&ctx, 1, "p.png", &rel, 40, 20).unwrap();
        let row = rimg::by_id(&ctx, id).unwrap().unwrap();
        (dir, ctx, row)
    }

    /// 用完就连目录一起删掉：连接还开着的时候 Windows 不让删，所以先把 ctx 交回来再丢
    fn cleanup(ctx: std::sync::Arc<Ctx>, dir: &std::path::PathBuf) {
        drop(ctx);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn 参数指纹随内容变随顺序不变() {
        let a = ops_hash(r#"{"v":1,"color":{"exposure":10}}"#);
        let b = ops_hash(r#"{"v":1,"color":{"exposure":10}}"#);
        let c = ops_hash(r#"{"v":1,"color":{"exposure":11}}"#);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn 存参数读回来是夹逼后的那一份() {
        let (dir, ctx, img) = fixture("save");
        let (mut ops, mut rep) = EditOps::parse(r#"{"color":{"exposure":999,"contrast":-30},"beauty":{"smooth":40,"by_mask":true}}"#).unwrap();
        rep.extend(normalize(&mut ops, 40));
        assert!(rep.iter().any(|x| x == "color.exposure"), "{rep:?}");
        radj::put(&ctx, img.id, &ops.to_json()).unwrap();
        let back = load_ops(&ctx, img.id);
        assert_eq!(back.color.exposure.0, 100);
        assert_eq!(back.color.contrast.0, -30);
        assert!(back.beauty.by_mask);
        assert!(!back.is_identity());
        cleanup(ctx, &dir);
    }

    #[test]
    fn 坏参数退全默认而不是把图卡死() {
        let (dir, ctx, img) = fixture("badops");
        radj::put(&ctx, img.id, "{不是 JSON").unwrap();
        assert!(load_ops(&ctx, img.id).is_identity());
        cleanup(ctx, &dir);
    }

    #[test]
    fn 轨迹入库前抽稀() {
        let (dir, ctx, _img) = fixture("simplify");
        let mut ops = EditOps::default();
        // 一条直线上的 31 个点：容差 1px（图宽 40 → 归一化 1/40）该压成两端
        ops.warp.strokes = vec![photoedit_core::Stroke {
            tool: photoedit_core::Tool::Push,
            points: (0..31).map(|i| [i as f32 / 40.0, i as f32 / 40.0]).collect(),
            radius: 0.1,
            strength: photoedit_core::Slider(70),
        }];
        let rep = normalize(&mut ops, 40);
        assert_eq!(ops.warp.strokes[0].points.len(), 2, "抽稀没生效：{:?}", ops.warp.strokes[0].points);
        assert!(rep.iter().any(|x| x.starts_with("warp.strokes.points")), "{rep:?}");
        cleanup(ctx, &dir);
    }

    #[test]
    fn 预览落档并复用() {
        let (dir, ctx, img) = fixture("prev");
        let mut ops = EditOps::default();
        ops.color.exposure = photoedit_core::Slider(40);
        let b = build_preview(&ctx, &img, &ops).unwrap();
        assert!(!b.reused);
        assert!(b.abs.is_file(), "预览档没落盘：{}", b.abs.display());
        assert!(b.rel.contains("adjprev"));
        assert!(b.h > 0, "预览响应缺高度");
        let again = build_preview(&ctx, &img, &ops).unwrap();
        assert!(again.reused, "同参数应当复用而不是重算");
        assert_eq!(again.rel, b.rel);
        // 换个参数就是另一个文件，旧的那份被清掉（目录里只留当前参数）
        ops.color.exposure = photoedit_core::Slider(-40);
        let third = build_preview(&ctx, &img, &ops).unwrap();
        assert_ne!(third.rel, b.rel);
        assert!(!b.abs.exists(), "旧预览档没被请走");
        cleanup(ctx, &dir);
    }

    #[test]
    fn 成图尺寸跟着几何段走() {
        let (dir, ctx, img) = fixture("render");
        let mut ops = EditOps::default();
        ops.geometry.rotate_deg = 90.0;
        ops.color.saturation = photoedit_core::Slider(-100);
        let (b, thumb) = build_render(&ctx, &img, &ops).unwrap();
        assert_eq!((b.w, b.h), (20, 40), "转了 90° 却没换边长");
        assert!(b.abs.is_file());
        assert!(thumb.is_some(), "成图该配一张 320 小档");
        cleanup(ctx, &dir);
    }

    #[test]
    fn 另存为新图走派生谱系且新行不带参数() {
        let (dir, ctx, img) = fixture("fork");
        let mut ops = EditOps::default();
        ops.color.contrast = photoedit_core::Slider(50);
        let (b, _) = build_render(&ctx, &img, &ops).unwrap();
        let new_id = fork_from(&ctx, &img, &b.rel, b.w as i64, b.h as i64).unwrap();
        let kid = rimg::by_id(&ctx, new_id).unwrap().unwrap();
        assert_eq!(kid.derived_from, Some(img.id));
        assert_eq!(kid.derived_result, None, "调整另存不挂结果行");
        assert!(kid.thumb_path.is_none() && kid.proxy_path.is_none(), "新行的派生档由后台补，不该在这里半成品");
        assert!(load_ops(&ctx, new_id).is_identity(), "新图不该继承父图的参数");
        // 父图的成图还在（另存是复制，不是改名）
        assert!(b.abs.is_file());
        cleanup(ctx, &dir);
    }

    #[test]
    fn 没调整时提交字节就是文件字节() {
        let (dir, ctx, img) = fixture("submit0");
        let (bytes, rel) = submit_artifact(&ctx, &img).unwrap();
        assert_eq!(rel, img.orig_path);
        assert_eq!(bytes, std::fs::read(ctx.data.join(&img.orig_path)).unwrap(), "零回归要求逐字节一致");
        cleanup(ctx, &dir);
    }

    #[test]
    fn 调整后提交的是重编码那一张并留在盘上() {
        let (dir, ctx, img) = fixture("submit1");
        let mut ops = EditOps::default();
        ops.color.temp = photoedit_core::Slider(60);
        radj::put(&ctx, img.id, &ops.to_json()).unwrap();
        let (bytes, rel) = submit_artifact(&ctx, &img).unwrap();
        assert_ne!(rel, img.orig_path);
        // 提交记录不进缓存层：它被结果行的 orig_path 指着，而这套参数之后会被改掉，删了就是真丢了
        assert!(rel.starts_with(&format!("projects/{}/", img.project_id)), "提交记录不该离开项目目录：{rel}");
        assert!(rel.to_lowercase().contains(&input_prefix(&img)), "名字没带前缀，删图时那条前缀扫描就捞不到它：{rel}");
        assert!(ctx.data.join(&rel).is_file(), "发出去那张要能在库里指着看");
        assert_eq!(&bytes[1..4], b"PNG", "本机那条要的是 PNG");
        assert!(adjusted(&ctx, &img));
        cleanup(ctx, &dir);
    }

    #[test]
    fn 删图清掉附属行与调整档() {
        let (dir, ctx, img) = fixture("purge");
        let mut ops = EditOps::default();
        ops.beauty.smooth = photoedit_core::Slider(50);
        radj::put(&ctx, img.id, &ops.to_json()).unwrap();
        build_preview(&ctx, &img, &ops).unwrap();
        build_render(&ctx, &img, &ops).unwrap();
        // 提交记录那张不在缓存层（它被结果行指着），删图时得靠前缀扫把它一起带走
        let (_, sent_rel) = submit_artifact(&ctx, &img).unwrap();
        assert!(ctx.data.join(&sent_rel).is_file());
        // 数的是缓存层里那一个目录，而且用生产那套路径构造器：规则再挪一次，这条测试要跟着动，
        // 不该在自己身体里再抄一份路径
        let slot_dir = || slots_dir(&ctx, &img);
        let names = || -> Vec<String> {
            std::fs::read_dir(slot_dir()).unwrap().flatten()
                .map(|e| e.file_name().to_string_lossy().to_string()).collect()
        };
        let files = names();
        assert!(files.len() >= 3, "预览/成图/缩略都该在：{files:?}");
        purge_all(&ctx, &img);
        let left = std::fs::read_dir(slot_dir()).map(|_| names()).unwrap_or_default();
        assert!(left.is_empty(), "调整档没清干净：{left:?}");
        assert!(!ctx.data.join(&sent_rel).is_file(), "projects/ 里那张提交记录还在");
        radj::clear(&ctx, img.id).unwrap();
        assert!(radj::ops_of(&ctx, img.id).unwrap().is_none());
        cleanup(ctx, &dir);
    }

    #[test]
    fn 蒙版限定只作用在涂过的那半边() {
        let (dir, ctx, img) = fixture("mask");
        // 左半涂满、右半空白的蒙版 PNG（与图同尺寸）
        let mut m = Rgba::new(40, 20);
        for y in 0..20 {
            for x in 0..40 {
                let v = if x < 20 { 255u8 } else { 0u8 };
                m.set(x, y, [v, v, v, v]);
            }
        }
        let mrel = util::rel_path(&["projects".into(), "1".into(), "p_mask.png".into()]);
        std::fs::write(ctx.data.join(&mrel), codec::encode_png(&m)).unwrap();
        rimg::set_mask(&ctx, img.id, Some(&mrel)).unwrap();
        // 重新取一次行：img 是 set_mask 之前拿的，mask_path 还带着 None
        let img = rimg::by_id(&ctx, img.id).unwrap().unwrap();
        // 底图换成一片肤色：美白只认肤色域，渐变图上的那些像素本来就不该被提亮，
        // 拿它测"蒙版内有效果"会测到算子的第一道闸门而不是蒙版
        let mut skin = Rgba::new(40, 20);
        for y in 0..20 {
            for x in 0..40 {
                skin.set(x, y, [200, 170, 150, 255]);
            }
        }
        std::fs::write(ctx.data.join(&img.orig_path), codec::encode_png(&skin)).unwrap();
        let src = codec::decode(&std::fs::read(ctx.data.join(&img.orig_path)).unwrap()).unwrap();
        let mut ops = EditOps::default();
        ops.beauty.brighten = photoedit_core::Slider(100);
        ops.beauty.by_mask = true;
        let out = pixels(&ctx, &img, &ops, Grade::Full).unwrap();
        let same = |x: usize| (out.get(x, 10)[1] as i32 - src.get(x, 10)[1] as i32).abs() <= 1;
        let off: Vec<(usize, [u8; 4])> = (22..40).filter(|x| !same(*x)).map(|x| (x, out.get(x, 10))).collect();
        assert!(off.is_empty(), "没涂的那半边被提亮了：{off:?}");
        assert!(!same(4), "涂过的那半边没提亮");
        assert!(out.get(4, 10)[1] as i32 > src.get(4, 10)[1] as i32 + 6, "提亮量不够可见");
        cleanup(ctx, &dir);
    }

    #[test]
    fn 面板带上预设与盘上lut名单() {
        let (dir, ctx, img) = fixture("panel");
        std::fs::create_dir_all(ctx.data.join("luts")).unwrap();
        std::fs::write(ctx.data.join("luts").join("B.cube"), "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
        std::fs::write(ctx.data.join("luts").join("A.cube"), "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
        std::fs::write(ctx.data.join("luts").join("忽略.txt"), "x").unwrap();
        let v = panel(&ctx, img.id).unwrap();
        assert_eq!(v["luts"].as_array().unwrap().len(), 2, "LUT 名单混进了非 .cube：{v:?}");
        // 名单按小写排序，A 在 B 前
        assert_eq!(v["luts"].as_array().unwrap()[0].as_str().unwrap(), "A.cube");
        let ps = v["presets"].as_array().unwrap();
        assert!(ps.len() >= 8);
        // 数值随面板一起下来：前端摆滑杆读这一份，不再自己抄一张表
        assert!(ps.iter().all(|p| p["name"].as_str().is_some() && p["color"].as_object().is_some()), "有预设没带数值：{ps:?}");
        let clean = ps.iter().find(|p| p["id"].as_str() == Some("clean")).unwrap();
        assert_eq!(clean["color"]["exposure"].as_i64(), Some(6), "预设数值与 color::preset 对不上：{clean}");
        assert!(clean["color"]["preset"].is_null(), "预设自己不该带 preset 字段：{clean}");
        assert_eq!(v["ops"]["v"].as_i64(), Some(1));
        cleanup(ctx, &dir);
    }

    #[test]
    fn lut只在data_luts那一层里找() {
        let (dir, ctx, _img) = fixture("lutpath");
        std::fs::create_dir_all(ctx.data.join("luts")).unwrap();
        for bad in ["../app.db", "luts/x.cube", "/etc/passwd"] {
            let err = lut_table(&ctx, bad).unwrap_err().text();
            assert!(err.contains("不合法") || err.contains("读不到"), "{bad} → {err}");
        }
        // 名字合法但文件不存在：说"读不到"而不是崩
        assert!(lut_table(&ctx, "没有这个.cube").unwrap_err().text().contains("读不到"));
        std::fs::write(ctx.data.join("luts").join("ok.cube"), "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
        assert!(lut_table(&ctx, "ok.cube").unwrap().is_some());
        assert!(lut_table(&ctx, "").unwrap().is_none(), "没选 LUT 就是没选，不该报错");
        cleanup(ctx, &dir);
    }

    #[test]
    fn 提交用的遮罩跟着照片一起换坐标系() {
        let (dir, ctx, img) = fixture("submask");
        // 遮罩按 proxy 分辨率存；这张 40×20 没有单独的 proxy 档，就按源图尺寸给
        let mut m = Rgba::new(40, 20);
        for y in 0..20usize {
            for x in 0..40usize {
                let v = if x < 20 { 255u8 } else { 0u8 };
                m.set(x, y, [v, v, v, v]);
            }
        }
        let mrel = util::rel_path(&["projects".into(), "1".into(), "p_mask.png".into()]);
        std::fs::write(dir.join(&mrel), codec::encode_png(&m)).unwrap();
        rimg::set_mask(&ctx, img.id, Some(&mrel)).unwrap();
        let row = rimg::by_id(&ctx, img.id).unwrap().unwrap();
        let file = std::fs::read(dir.join(&mrel)).unwrap();
        // 1) 没有几何段：一个字节都不该动（与 0.2.1 那条上传逐位一致）
        radj::put(&ctx, row.id, &EditOps::default().to_json()).unwrap();
        assert_eq!(submit_mask(&ctx, &row).unwrap(), file, "没转没裁的时候遮罩被重编码了");
        // 2) 右转 90°：照片会换成 20×40，遮罩必须一起换，笔迹从"左半"变成"上半"
        let mut ops = EditOps::default();
        ops.geometry.rotate_deg = 90.0;
        radj::put(&ctx, row.id, &ops.to_json()).unwrap();
        let d = codec::decode(&submit_mask(&ctx, &row).unwrap()).unwrap();
        assert_eq!((d.w, d.h), (20, 40), "遮罩没跟着照片换边长，工作流圈到的就是另一块");
        assert_eq!(d.get(0, 0)[0], 255, "源图左半的笔迹转过去应当落在上半");
        assert_eq!(d.get(0, 30)[0], 0, "下半原本没涂，转过去也不该有笔迹");
        cleanup(ctx, &dir);
    }

    #[test]
    fn 关键点是空的时候一键塑形不改动图() {
        let (dir, ctx, img) = fixture("noshape");
        let mut ops = EditOps::default();
        ops.warp.auto.face_slim = photoedit_core::Slider(80);
        // image_landmark 里一行都没有 → shape_for 给 None → 链上这一段跳过
        let src = codec::decode(&std::fs::read(ctx.data.join(&img.orig_path)).unwrap()).unwrap();
        let out = pixels(&ctx, &img, &ops, Grade::Full).unwrap();
        assert_eq!(out.px, src.px, "没有关键点却动了像素");
        cleanup(ctx, &dir);
    }

    /// 一张明显大于 proxy 档的图：源 1600×1200，proxy 只有 400×300。
    /// 0.3.0 那版「本地调整只剩一张小图、放大看不到真像素」就出在这种尺寸差的组合上，
    /// 40×20 的小图测不出来——它的 proxy 档与源图同尺寸，两个口径报出来的数一样。
    fn big_fixture(name: &str) -> (std::path::PathBuf, std::sync::Arc<Ctx>, Image) {
        let dir = std::env::temp_dir().join(format!("synco-adj-{name}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("projects").join("1")).unwrap();
        let ctx = Ctx::new(dir.clone(), dir.clone(), db::open(&dir).unwrap());
        crate::repo::projects::create(&ctx, "测试项目").unwrap();
        let mut src = Rgba::new(1600, 1200);
        for y in 0..1200usize {
            for x in 0..1600usize {
                src.set(x, y, [(x / 7) as u8, (y / 5) as u8, 90, 255]);
            }
        }
        let rel = util::rel_path(&["projects".into(), "1".into(), "big.png".into()]);
        std::fs::write(dir.join(&rel), codec::encode_png(&src)).unwrap();
        let prel = util::rel_path(&["projects".into(), "1".into(), "big_proxy400.jpg".into()]);
        std::fs::write(dir.join(&prel), codec::encode_jpeg(&codec::scale_to_long_edge(&src, 400), 88)).unwrap();
        let id = rimg::insert(&ctx, 1, "big.png", &rel, 1600, 1200).unwrap();
        rimg::set_derived(&ctx, id, None, Some(&prel)).unwrap();
        let row = rimg::by_id(&ctx, id).unwrap().unwrap();
        (dir, ctx, row)
    }

    #[test]
    fn 预览报的是成图那一档的尺寸而不是proxy档() {
        let (dir, ctx, img) = big_fixture("prevdims");
        assert!(img.proxy_path.is_some(), "fixture 没接上 proxy 档");
        let mut ops = EditOps::default();
        ops.color.exposure = photoedit_core::Slider(40);
        let b = build_preview(&ctx, &img, &ops).unwrap();
        // 关键一条：预览档只有 400×300，报出去的必须是 1600×1200。
        // 报了档自己的尺寸，前端就把它当画幅用——图当场小一圈，1:1 也只是那张糊图
        assert_eq!((b.w, b.h), (1600, 1200));
        let again = build_preview(&ctx, &img, &ops).unwrap();
        assert!(again.reused && (again.w, again.h) == (1600, 1200), "复用那条分支与新建口径不一致");
        // 转过 90° 长宽换边的还是成图那一档，不是 proxy 那一档
        let mut rot = ops.clone();
        rot.geometry.rotate_deg = 90.0;
        let t = build_preview(&ctx, &img, &rot).unwrap();
        assert_eq!((t.w, t.h), (1200, 1600));
        cleanup(ctx, &dir);
    }

    #[test]
    fn 调整视图的瓦片切的是成图那一档() {
        let (dir, ctx, img) = big_fixture("adjtiles");
        let mut ops = EditOps::default();
        ops.beauty.smooth = photoedit_core::Slider(50);
        let m = tiles(&ctx, &img, &ops).unwrap();
        assert_eq!(m["w"].as_i64(), Some(1600));
        assert_eq!(m["h"].as_i64(), Some(1200));
        assert_eq!(m["tile"].as_i64(), Some(imagesvc::TILE as i64));
        let url = m["url"].as_str().unwrap_or_default().to_string();
        assert!(url.contains(&format!("cache/t/{}/adj", img.id)), "瓦片路径没带参数指纹：{url}");
        let levels = m["levels"].as_array().cloned().unwrap_or_default();
        assert!(!levels.is_empty(), "长边 1600 该切出一套金字塔");
        // 末元素是最细一层（build_tiles_at 把顺序反转过）
        let z = levels[levels.len() - 1]["z"].as_i64().unwrap_or(-1);
        let hash = ops_hash(&ops.to_json());
        let root = imagesvc::tiles_dir(&ctx, &img);
        let finest = root.join(format!("adj{hash}")).join(z.to_string()).join("0_0.jpg");
        assert!(finest.is_file(), "最细一层的第一格没落盘：{}", finest.display());
        // 换一套参数：新目录建起来，旧的那套要被请走——一次就是上百张，留着等于白堆
        let mut other = ops.clone();
        other.color.contrast = photoedit_core::Slider(30);
        let m2 = tiles(&ctx, &img, &other).unwrap();
        assert_ne!(m2["url"].as_str().unwrap_or_default(), url.as_str());
        let root = imagesvc::tiles_dir(&ctx, &img);
        assert!(root.join(format!("adj{}", ops_hash(&other.to_json()))).is_dir());
        assert!(!root.join(format!("adj{hash}")).exists(), "旧参数的瓦片目录没清掉");
        cleanup(ctx, &dir);
    }
}
