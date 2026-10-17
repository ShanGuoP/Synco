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
pub mod gpu;
pub mod histogram;
pub mod lut_import;

pub fn automatic(ctx:&Ctx,img:&Image,point:Option<[f32;2]>)->Result<Value> {
    let base=orig_rgba(ctx,img)?;
    let base=codec::scale_to_long_edge(&base,1200);
    let (color,basic)=photoedit_core::develop::automatic::analyze(&base,point);
    if point.is_some() {Ok(json!({"color":{"temp":color.temp,"tint":color.tint}}))}
    else {Ok(json!({"color":{"exposure":color.exposure,"contrast":color.contrast,"highlights":color.highlights,"shadows":color.shadows},"basic":basic}))}
}

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

/// Face-dependent pixels get a different immutable URL when analysis changes.
/// Submitted inputs are records: a refresh must never delete their history.
fn render_hash(ctx: &Ctx, img: &Image, ops: &EditOps) -> Result<String> {
    if ops.warp.auto.is_empty() && !ops.beauty.auto_face && !ops.portrait.needs_faces() {
        return Ok(if ops.lut.is_some() {ops_hash(&json!({"ops":ops,"lut_domain":2}).to_string())}else{ops_hash(&ops.to_json())});
    }
    let points=radj::landmarks(ctx,img.id)?;
    Ok(ops_hash(&json!({"ops":ops,"face_pipeline":4,"lut_domain":2,"points":points}).to_string()))
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

fn beauty_mask(ctx: &Ctx, img: &Image, ops: &EditOps, w: usize, h: usize) -> Result<Option<Alpha>> {
    let painted = if ops.beauty.by_mask { mask_plane(ctx,img,w,h)? } else { None };
    if !ops.beauty.auto_face { return Ok(painted); }
    // Empty detection is an empty mask, never permission to beautify the background.
    let mut skin = crate::service::face::auto_mask(ctx,img,w,h)?;
    if let Some(m) = painted { for (a,b) in skin.v.iter_mut().zip(m.v) { *a=((*a as u16*b as u16)/255) as u8; } }
    Ok(Some(skin))
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
    let mask = beauty_mask(ctx,img,ops,base.w,base.h)?;
    let shape = if ops.warp.auto.is_empty() { None } else { shape_for(ctx, img) };
    let table = match ops.lut.as_ref() {
        Some(l) => lut_table(ctx, &l.name)?,
        None => None,
    };
    let meshes = if ops.portrait.needs_faces() { crate::service::face::cached_meshes(ctx,img)? } else { Vec::new() };
    let portrait_mask = if ops.portrait.faces.iter().any(|f| f.beauty.by_mask) { mask_plane(ctx,img,base.w,base.h)? } else { None };
    let chain = Chain { mask: mask.as_ref(), shape: shape.as_ref(), lut: table.as_ref(), meshes:&meshes, portrait_mask:portrait_mask.as_ref() };
    Ok(apply_chain(&base, ops, &chain))
}

/// 预览：算 → 落 `_adjprev<指纹>.jpg` → 回 URL。同参数已有文件直接复用。
/// 报出去的宽高是**成图那一档**（`out_size`），不是这张 proxy 档自己的：
/// 前端拿它当画幅尺寸，报了预览档的尺寸就等于把画布缩到 proxy 那么大——图会当场小一圈，
/// 1:1 与放大看到的也只是那张糊图。两条分支（新建与复用）必须同一口径，否则拖两下滑杆画幅会跳。
pub fn build_preview(ctx: &Ctx, img: &Image, ops: &EditOps) -> Result<Built> {
    let h = render_hash(ctx,img,ops)?;
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
    histogram::store(&abs.with_extension("hist.json"),&out)?;
    Ok(Built { rel, abs, ms: util::now_ms().saturating_sub(t0), reused: false, w, h: hh })
}

/// 成图：原分辨率落 `_adjusted<指纹>.jpg`，并配一张 320 小档给列表用。
/// 返回的第二个值是缩略档的相对路径（缩略档写失败不影响成图，回 `None` 就行）。
pub fn build_render(ctx: &Ctx, img: &Image, ops: &EditOps) -> Result<(Built, Option<String>)> {
    let h = render_hash(ctx,img,ops)?;
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
    let h = render_hash(ctx,img,ops)?;
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
    let (rel, abs) = input_slot(ctx, img, &render_hash(ctx,img,&ops)?);
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


mod snapshot;
pub(crate) use snapshot::{photo_from_snapshot, snapshot_inputs};
#[cfg(test)]
mod tests;
