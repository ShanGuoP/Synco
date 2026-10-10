//! 裁切-缝合主流程：把遮罩包围盒外扩成裁切区发给模型，回来再折回整图。
//! 服务端拿到的是解码后的像素数组，PNG 编解码在 `crates/server` 里做，内核不碰 IO。

use px_core::buffer::{Alpha, Box2, Rgba};
use crate::color::color_match;
use crate::geom::{fit_size, Fit};
use crate::mask::{dilate, ink_bbox, paste_alpha};
use px_core::par::par_chunks_mut;
use crate::pyramid::{pyramid_blend, pyramid_blend_boxed};
use px_core::resize::{crop_scale_alpha, crop_scale_rgba, to_u8};

pub const DEFAULT_CROP_EDGE: u32 = 1024;
pub const DEFAULT_EXPAND: f64 = 96.0;
pub const DEFAULT_CONTEXT: f64 = 0.35;
pub const DEFAULT_FEATHER: f64 = 48.0;
pub const DEFAULT_LEVELS: usize = 4;

/// 缝合的**规则版本**：`stitch_crop` 是纯函数，同样的入参在这段代码没改过的前提下逐位相同。
/// 一旦改了取值条件（外扩/羽化/层数）、取整口径或金字塔的锚点约定，旧结果行"按当时那套参数重算"
/// 就不再等于"当年那一张"——所以这个号要跟着 +1，落库的那份快照只在号相同的时候才允许重算无损。
pub const RULES_V: u32 = 1;

#[derive(Clone, Copy, Debug)]
pub struct StitchParams {
    /// 蒙版外扩（原图 px）：模型重绘范围比涂抹大多少，接缝落在这里面
    pub expand: f64,
    /// 裁切框在扩后包围盒之外再留的上下文比例
    pub context: f64,
    /// 裁切区送云端的目标长边
    pub crop_edge: u32,
    /// 贴回 alpha 的模糊半径
    pub feather: f64,
    pub levels: usize,
    /// 反向涂抹：**涂住的是要保住的**，其余整幅交给模型重绘（圈主体换背景）。
    /// 语义一次贯穿到裁切框、蒙版取反、贴回权重三处，禁止在调用点各写各的 `if invert`。
    pub invert: bool,
}

impl Default for StitchParams {
    fn default() -> Self {
        Self {
            expand: DEFAULT_EXPAND,
            context: DEFAULT_CONTEXT,
            crop_edge: DEFAULT_CROP_EDGE,
            feather: DEFAULT_FEATHER,
            levels: DEFAULT_LEVELS,
            invert: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CropPayload {
    /// 发给模型的裁切区原图（fit 后的尺寸）
    pub image: Rgba,
    /// 发给模型的蒙版：不透明=保留、透明=重绘（接口约定，已实跑证实）
    pub mask: Rgba,
    /// 裁切区在原图上的位置
    pub crop: Box2,
    pub fit: Fit,
    /// 反向涂抹且一笔没涂 = 整幅都要重绘，这时**不要带 mask 这个 part**
    /// （比发一张全透明的巨图稳，也绕开各家的遮罩体积上限）
    pub no_mask: bool,
}

#[derive(Clone, Debug)]
pub enum Build {
    Payload(CropPayload),
    /// 折不出合法档位（比例超 3:1、补到像素下限就顶破上限）
    Unfit,
    /// 没有可用笔迹
    NoInk,
}

/// 从整图 + 涂抹层生成裁切载荷。`base` 是原图分辨率，`mask` 是涂抹层分辨率（可以更低）。
///
/// 正向：笔迹 = 要重绘的那块，裁切框围着它建。
/// 反向（`invert`）：笔迹 = 要保住的那块，要重绘的是"其余"，包围盒就是整幅——
/// 裁切优化在反向里必然失效，这是这个功能的定义而不是实现取巧。
pub fn build_crop_payload(base: &Rgba, mask: &Alpha, p: &StitchParams) -> Build {
    let (iw, ih) = (base.w, base.h);
    if mask.w == 0 || mask.h == 0 || iw == 0 || ih == 0 {
        return Build::NoInk;
    }
    let bb = ink_bbox(mask, 8);
    if !p.invert && bb.is_none() {
        return Build::NoInk;
    }
    let k = iw as f64 / mask.w as f64;

    let (cx0, cy0, cx1, cy1) = match bb {
        Some(b) if !p.invert => {
            let pad = (b.w.max(b.h) as f64 * k * p.context).round() as usize + 8;
            (
                (b.x as f64 * k - p.expand - pad as f64).floor().max(0.0) as usize,
                (b.y as f64 * k - p.expand - pad as f64).floor().max(0.0) as usize,
                ((b.x + b.w) as f64 * k + p.expand + pad as f64).ceil().min(iw as f64) as usize,
                ((b.y + b.h) as f64 * k + p.expand + pad as f64).ceil().min(ih as f64) as usize,
            )
        }
        // 反向：整幅进裁切区，上下文没有"框外"可留（pad 在这里语义是空的）
        _ => (0, 0, iw, ih),
    };
    let (cw, ch) = (cx1 - cx0, cy1 - cy0);
    if cw == 0 || ch == 0 {
        return Build::NoInk;
    }

    let fit = fit_size(cw as u32, ch as u32, p.crop_edge);
    if fit.unfit {
        return Build::Unfit;
    }
    let (pw, ph) = (fit.w as usize, fit.h as usize);

    let image = crop_scale_rgba(base, cx0, cy0, cw, ch, pw, ph);

    // 发给模型的蒙版在涂抹层分辨率上外扩（缩放自动把半径带到原图比例），再裁切缩放。
    // 外扩的永远是"被涂的那一块"：正向它等于扩大重绘区，反向它等于把保护区再撑一圈，方向天然对。
    let grown = dilate(mask, p.expand / k);
    let sx = (cx0 as f64 / k).round() as usize;
    let sy = (cy0 as f64 / k).round() as usize;
    let sw = (cw as f64 / k).round() as usize;
    let sh = (ch as f64 / k).round() as usize;
    let scaled = crop_scale_alpha(&grown, sx, sy, sw, sh, pw, ph);
    // 黑底：接口约定 alpha=0 才是重绘。正向取反（涂抹处变透明），反向直接用（涂住处保持不透明）
    let mut msk = Rgba::new(pw, ph);
    for i in 0..pw * ph {
        msk.px[i * 4 + 3] = if p.invert { scaled.v[i] } else { 255 - scaled.v[i] };
    }
    // 反向 + 一笔没涂 = 整幅重绘，mask 全透明不如干脆不带这个 part
    let no_mask = p.invert && bb.is_none();

    Build::Payload(CropPayload {
        image,
        mask: msk,
        crop: Box2::new(cx0, cy0, cw, ch),
        fit,
        no_mask,
    })
}

/// 涂抹层折算到裁切区坐标系
fn crop_mask(base: &Rgba, mask: &Alpha, crop: Box2) -> Alpha {
    let s = mask.w as f64 / base.w as f64;
    crop_scale_alpha(
        mask,
        (crop.x as f64 * s).round() as usize,
        (crop.y as f64 * s).round() as usize,
        (crop.w as f64 * s).round() as usize,
        (crop.h as f64 * s).round() as usize,
        crop.w,
        crop.h,
    )
}

/// 贴回权重（裁切区坐标系）：涂抹区外扩一半再羽化。
/// 反向涂抹就是这张图的补——**涂住的权重为 0（逐字节保持原图），其余为 1（贴模型）**，
/// 两条权重的和恒等于 255，这条不变量由 `正向与反向的贴回权重恰好互补` 钉住。
/// 单独暴露是为了让回归能按"权重是否为 0"分档判漂移，而不是整张一刀切。
pub fn paste_weights(base: &Rgba, mask: &Alpha, crop: Box2, p: &StitchParams) -> Alpha {
    paste_from_user(&crop_mask(base, mask, crop), p)
}

/// 权重的唯一算法：`stitch_inner` 与回归用的 `paste_weights` 必须走这一个口径，
/// 分开写两次就会在下次调羽化时漂掉。
fn paste_from_user(user: &Alpha, p: &StitchParams) -> Alpha {
    // 羽化过渡带必须完整落在模型重绘区（外扩蒙版）内：上限 = 外扩 × 0.6
    let feather = p.feather.min((p.expand * 0.6).round());
    let a = paste_alpha(user, p.expand * 0.4, feather);
    if p.invert { complement(a) } else { a }
}

/// 正向权重的补集。分块走线程池：反向时裁切区就是整幅，24MP 逐字节取反不值得单核跑
fn complement(mut a: Alpha) -> Alpha {
    let chunk = (a.v.len() / 16).clamp(4096, 1 << 16);
    par_chunks_mut(&mut a.v, chunk, |blk, _| {
        for v in blk {
            *v = 255 - *v;
        }
    });
    a
}

/// 把模型返回的裁切区校正、融合后贴回整图。
/// `model` 是云端成图（尺寸可以是接口返回的任意档位，内部折回裁切区尺寸）。
/// 与 JS 同构的整幅金字塔走这条；`stitch_crop_boxed` 是盒内变体，两者差值见 bench。
pub fn stitch_crop(base: &Rgba, mask: &Alpha, crop: Box2, model: &Rgba, p: &StitchParams) -> Rgba {
    stitch_inner(base, mask, crop, model, p, false)
}

/// 盒内金字塔变体：只对贴回包围盒（外扩 8px）做逐层重采样，裁切区越大省得越多
pub fn stitch_crop_boxed(base: &Rgba, mask: &Alpha, crop: Box2, model: &Rgba, p: &StitchParams) -> Rgba {
    stitch_inner(base, mask, crop, model, p, true)
}

fn stitch_inner(base: &Rgba, mask: &Alpha, crop: Box2, model: &Rgba, p: &StitchParams, boxed: bool) -> Rgba {
    let (cw, ch) = (crop.w, crop.h);
    if cw == 0 || ch == 0 || model.w == 0 || model.h == 0 {
        return base.clone();
    }

    let mut out = crop_scale_rgba(model, 0, 0, model.w, model.h, cw, ch);
    let orig = crop_scale_rgba(base, crop.x, crop.y, cw, ch, cw, ch);

    let user = crop_mask(base, mask, crop);
    let send = dilate(&user, p.expand);
    // 统计带 = "模型本该原样画回来、用户没涂"的那一圈：正向是重绘区外的外扩环，
    // 反向是保护区外的外扩环，两个方向都是 send 有值而 user 没有，所以这条不用分档
    color_match(&mut out, &orig, &send, &user);

    // 贴回范围 = 涂抹区外扩一半 + 羽化；反向时取补集，接缝带仍完整落在模型重绘区内
    let alpha = paste_from_user(&user, p);
    let abox = match ink_bbox(&alpha, 2) {
        Some(b) => b,
        None => return base.clone(),
    };

    let blended = if boxed {
        pyramid_blend_boxed(&orig, &out, &alpha, abox, p.levels)
    } else {
        pyramid_blend(&orig, &out, &alpha, abox, p.levels)
    };
    let mut res = base.clone();
    composite_over(&mut res, &blended, crop.x, crop.y);
    res
}

/// canvas 的 source-over：`dst = src·a + dst·(1−a)`
fn composite_over(dst: &mut Rgba, src: &Rgba, dx: usize, dy: usize) {
    let stride = dst.w * 4;
    par_chunks_mut(&mut dst.px, stride, |blk, y| {
        if y < dy {
            return;
        }
        let sy = y - dy;
        if sy >= src.h {
            return;
        }
        let n = src.w.min(dst.w.saturating_sub(dx));
        for x in 0..n {
            let si = src.off(x, sy);
            let a = src.px[si + 3] as f32 / 255.0;
            if a == 0.0 {
                continue;
            }
            let di = x * 4 + dx * 4;
            let da = blk[di + 3] as f32 / 255.0;
            let out_a = a + da * (1.0 - a);
            for c in 0..3 {
                let v = src.px[si + c] as f32 * a + blk[di + c] as f32 * da * (1.0 - a);
                blk[di + c] = to_u8(if out_a > 0.0 { v / out_a } else { 0.0 });
            }
            blk[di + 3] = to_u8(out_a * 255.0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_core::buffer::Alpha;

    /// 渐变原图 + 一块涂抹，够代表真实形状
    fn scene(w: usize, h: usize) -> (Rgba, Alpha) {
        let mut base = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                base.set(x, y, [(x * 3 % 255) as u8, (y * 5 % 255) as u8, 90, 255]);
            }
        }
        let mut mask = Alpha::new(w, h);
        for y in h / 3..h / 2 {
            for x in w / 3..w / 2 {
                mask.v[y * w + x] = 255;
            }
        }
        (base, mask)
    }

    #[test]
    fn 无笔迹返回_noink() {
        let (base, mut mask) = scene(600, 400);
        mask.v.iter_mut().for_each(|v| *v = 0);
        assert!(matches!(build_crop_payload(&base, &mask, &StitchParams::default()), Build::NoInk));
    }

    #[test]
    fn 载荷尺寸都是16的倍数且蒙版语义是透明即重绘() {
        let (base, mask) = scene(1200, 900);
        let p = StitchParams::default();
        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else { panic!("应该成功") };
        assert_eq!(pl.fit.w % 16, 0);
        assert_eq!(pl.fit.h % 16, 0);
        // 涂抹处 alpha=0（透明=重绘），远离涂抹处 alpha=255（保留）
        let (mw, mh) = (pl.mask.w, pl.mask.h);
        assert_eq!(pl.mask.px[(mh / 2 * mw + mw / 2) * 4 + 3], 0);
        assert_eq!(pl.mask.px[4 + 3], 255);
        assert!(pl.crop.w <= base.w && pl.crop.h <= base.h);
    }

    #[test]
    fn 模型原样画回来时零权重像素逐字节不动() {
        // 零漂移的准确判据：贴回权重为 0 的地方原样保留；权重 >0 的过渡带只剩金字塔重构舍入。
        // 喂的是裁切区原尺寸的逐像素拷贝，不是载荷图——载荷经过缩放往返，本身就差几十个色阶。
        let (base, mask) = scene(1200, 900);
        let p = StitchParams::default();
        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else { panic!("应该成功") };
        let mut exact = Rgba::new(pl.crop.w, pl.crop.h);
        for y in 0..pl.crop.h {
            for x in 0..pl.crop.w {
                exact.set(x, y, base.get(pl.crop.x + x, pl.crop.y + y));
            }
        }
        let res = stitch_crop(&base, &mask, pl.crop, &exact, &p);
        let w = paste_weights(&base, &mask, pl.crop, &p);

        let mut zero_px = 0usize;
        let mut worst_weighted = 0i32;
        for y in 0..pl.crop.h {
            for x in 0..pl.crop.w {
                let b = base.get(pl.crop.x + x, pl.crop.y + y);
                let r = res.get(pl.crop.x + x, pl.crop.y + y);
                if w.get(x, y) == 0 {
                    assert_eq!(r, b, "({x},{y}) 权重为 0 却被改动");
                    zero_px += 1;
                } else {
                    for c in 0..3 {
                        worst_weighted = worst_weighted.max((r[c] as i32 - b[c] as i32).abs());
                    }
                }
            }
        }
        assert!(zero_px > 1_000, "裁切区里权重为 0 的像素只有 {zero_px}，样本不够");
        assert!(worst_weighted <= 6, "过渡带重构噪声 {worst_weighted} 太大");
    }

    #[test]
    fn 蒙版外区域绝对不被动过() {
        // 裁切区之外（外扩蒙版之外）必须逐字节相同
        let (base, mask) = scene(1000, 800);
        let p = StitchParams { expand: 24.0, crop_edge: 512, ..StitchParams::default() };
        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else { panic!("应该成功") };
        let mut fake = pl.image.clone();
        for i in 0..fake.w * fake.h {
            fake.px[i * 4] = fake.px[i * 4].saturating_add(60);
        }
        let res = stitch_crop(&base, &mask, pl.crop, &fake, &p);
        for y in 0..base.h {
            for x in 0..base.w {
                let inside = x >= pl.crop.x && x < pl.crop.x + pl.crop.w && y >= pl.crop.y && y < pl.crop.y + pl.crop.h;
                if !inside {
                    assert_eq!(res.get(x, y), base.get(x, y), "裁切区外 ({x},{y}) 被改动");
                }
            }
        }
    }

    #[test]
    fn 羽化被外扩夹住() {
        let (base, mask) = scene(900, 700);
        let p = StitchParams { expand: 10.0, feather: 200.0, ..StitchParams::default() };
        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else { panic!("应该成功") };
        // 只要求不 panic 且结果尺寸对：夹到 round(10*0.6)=6 后过渡带仍在重绘区内
        let res = stitch_crop(&base, &mask, pl.crop, &pl.image, &p);
        assert_eq!(res.w, base.w);
    }

    /// 反向涂抹的核心不变量：同一份笔迹，正向与反向的贴回权重逐像素互补（和恒为 255）。
    /// 这条不立住，以后每调一次羽化或外扩，两个方向就会各漂各的。
    #[test]
    fn 正向与反向的贴回权重恰好互补() {
        let (base, mask) = scene(1000, 800);
        // 两条权重要在同一个裁切框上比，反向的框就是整幅，正向这里也框整幅
        let crop = Box2::new(0, 0, 1000, 800);
        let f = StitchParams { expand: 40.0, feather: 18.0, ..StitchParams::default() };
        let r = StitchParams { invert: true, ..f };
        let wf = paste_weights(&base, &mask, crop, &f);
        let wr = paste_weights(&base, &mask, crop, &r);
        let mut painted_full = 0usize;
        let mut outside_zero = 0usize;
        // scene() 的笔迹是 x∈[w/3,w/2)、y∈[h/3,h/2)，取它内部与远角两头
        for y in 0..crop.h {
            for x in 0..crop.w {
                let a = wf.get(x, y) as usize;
                let b = wr.get(x, y) as usize;
                assert_eq!(a + b, 255, "({x},{y}) 权重不互补：{a} + {b}");
                // 涂住的主体**核心**：反向必须完全不贴模型。
                // 贴着笔迹边缘那一圈是羽化带（正向到不了满分），所以取样往中心收
                if x > 390 && x < 440 && y > 310 && y < 355 {
                    painted_full += 1;
                    assert_eq!(b, 0, "({x},{y}) 反向却在保护区内贴了模型（权重 {b}）");
                }
                // 远离笔迹的一角：反向应当整份取模型
                if x > 940 && y > 740 {
                    outside_zero += 1;
                    assert_eq!(b, 255, "({x},{y}) 反向没把远处的背景交给模型（权重 {b}）");
                }
            }
        }
        assert!(painted_full > 1_000 && outside_zero > 1_000, "取样不够：{painted_full} / {outside_zero}");
    }

    #[test]
    fn 反向涂住的逐字节不动_其余换成模型的() {
        let (base, mask) = scene(1000, 800);
        let p = StitchParams { expand: 24.0, feather: 10.0, crop_edge: 512, invert: true, ..StitchParams::default() };
        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else { panic!("应该成功") };
        // 裁切框退化到整幅是反向的定义：要改的是"笔迹之外"，那块包围盒就是全图
        assert_eq!((pl.crop.x, pl.crop.y), (0, 0));
        assert_eq!((pl.crop.w, pl.crop.h), (base.w, base.h));
        let mut model = pl.image.clone();
        for i in 0..model.w * model.h {
            model.px[i * 4] = model.px[i * 4].saturating_add(60);
        }
        let res = stitch_crop(&base, &mask, pl.crop, &model, &p);
        let w = paste_weights(&base, &mask, pl.crop, &p);
        let mut kept = 0usize;
        let mut swapped = 0usize;
        for y in 0..base.h {
            for x in 0..base.w {
                if w.get(x, y) == 0 {
                    assert_eq!(res.get(x, y), base.get(x, y), "({x},{y}) 权重 0 却被改动");
                    kept += 1;
                } else if y > 600 && x > 600 {
                    if res.get(x, y) != base.get(x, y) {
                        swapped += 1;
                    }
                }
            }
        }
        assert!(kept > 5_000, "保住的主体只有 {kept} 像素");
        assert!(swapped > 20_000, "远处的背景几乎没被换掉（{swapped}）");
    }

    #[test]
    fn 反向发给模型的蒙版方向是反的() {
        let (base, mask) = scene(800, 600);
        let p = StitchParams { invert: true, crop_edge: 512, ..StitchParams::default() };
        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else { panic!("应该成功") };
        assert!(!pl.no_mask, "涂了笔迹就该带蒙版");
        let (mw, mh) = (pl.mask.w, pl.mask.h);
        // 涂抹在原图中央偏上，折算到载荷坐标后应当是"保留"（alpha 255），远处一角是"重绘"（0）
        assert_eq!(pl.mask.px[(mh * 45 / 100 * mw + mw * 42 / 100) * 4 + 3], 255, "涂住处必须是保留");
        assert_eq!(pl.mask.px[4 + 3], 0, "画布一角必须交给模型重绘");
    }

    /// 方案 D10：反向时"一笔没涂"是合法输入 = 整幅重绘，这时不造一张全透明的遮罩发出去
    #[test]
    fn 反向没涂也允许提交且不带蒙版() {
        let (base, mut mask) = scene(800, 600);
        mask.v.iter_mut().for_each(|v| *v = 0);
        let p = StitchParams { invert: true, crop_edge: 512, ..StitchParams::default() };
        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else { panic!("反向没有笔迹不该判 NoInk") };
        assert!(pl.no_mask);
        assert_eq!((pl.crop.w, pl.crop.h), (800, 600));
        // 正向同样的空笔迹仍然要拦：那是"什么都没选中"，不是"整幅重绘"
        assert!(matches!(build_crop_payload(&base, &mask, &StitchParams::default()), Build::NoInk));
    }

    /// 分阶段计时：24MP 一趟里最贵的环节要量出来，优化别靠猜
    #[test]
    #[ignore]
    fn bench_分阶段() {
        use std::time::{Duration, Instant};
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let (w, h) = (4000usize, 6000usize);
        let mut base = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                base.set(x, y, [(x / 7) as u8, (y / 11) as u8, (x ^ y) as u8, 255]);
            }
        }
        let mut mask = Alpha::new(w, h);
        for y in h / 3..h / 2 {
            for x in w / 3..w / 2 {
                mask.v[y * w + x] = 255;
            }
        }
        let p = StitchParams::default();

        let t = Instant::now();
        let bb = ink_bbox(&mask, 8).unwrap();
        println!("ink_bbox 24MP          {:.1}ms 盒 {:?}", ms(t.elapsed()), bb);

        let t = Instant::now();
        let grown = dilate(&mask, p.expand);
        println!("dilate r=96 24MP       {:.1}ms", ms(t.elapsed()));

        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else { panic!("应该成功") };
        let c = pl.crop;
        println!("裁切区 {}x{} → 载荷 {}x{}", c.w, c.h, pl.fit.w, pl.fit.h);

        let t = Instant::now();
        let _ = crop_scale_rgba(&base, c.x, c.y, c.w, c.h, pl.fit.w as usize, pl.fit.h as usize);
        println!("载荷图缩放             {:.1}ms", ms(t.elapsed()));

        let t = Instant::now();
        let _ = crop_scale_alpha(&grown, (c.x as f64).round() as usize, (c.y as f64).round() as usize, c.w, c.h, pl.fit.w as usize, pl.fit.h as usize);
        println!("载荷蒙版缩放           {:.1}ms", ms(t.elapsed()));

        let user = crop_scale_alpha(&mask, c.x, c.y, c.w, c.h, c.w, c.h);
        let orig = crop_scale_rgba(&base, c.x, c.y, c.w, c.h, c.w, c.h);
        let mut out = orig.clone();
        let t = Instant::now();
        let send = dilate(&user, p.expand);
        println!("dilate r=96 裁切区      {:.1}ms", ms(t.elapsed()));
        let t = Instant::now();
        for i in 0..c.w * c.h {
            out.px[i * 4] = out.px[i * 4].saturating_add(12);
        }
        color_match(&mut out, &orig, &send, &user);
        println!("color_match            {:.1}ms", ms(t.elapsed()));

        let t = Instant::now();
        let alpha = paste_alpha(&user, p.expand * 0.4, p.feather);
        println!("paste_alpha(膨胀+羽化)  {:.1}ms", ms(t.elapsed()));

        let abox = ink_bbox(&alpha, 2).unwrap();
        let t = Instant::now();
        let blended = pyramid_blend(&orig, &out, &alpha, abox, p.levels);
        println!("pyramid_blend 盒 {}x{}  {:.1}ms", abox.w, abox.h, ms(t.elapsed()));

        let t = Instant::now();
        let mut res = base.clone();
        println!("整图 clone 24MP        {:.1}ms", ms(t.elapsed()));
        let t = Instant::now();
        composite_over(&mut res, &blended, c.x, c.y);
        println!("composite 贴回         {:.1}ms", ms(t.elapsed()));
    }

    /// 24MP 基准：方案 §5 明确"实测填表、不预设数值"，跑 `cargo test --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_两版金字塔比对() {
        use std::time::Instant;
        let (w, h) = (4000usize, 6000usize);
        let mut base = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                base.set(x, y, [(x / 7) as u8, (y / 11) as u8, (x ^ y) as u8, 255]);
            }
        }
        let mut mask = Alpha::new(w, h);
        for y in h / 3..h / 2 {
            for x in w / 3..w / 2 {
                mask.v[y * w + x] = 255;
            }
        }
        let p = StitchParams::default();
        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else { panic!("应该成功") };
        let mut model = pl.image.clone();
        for i in 0..model.w * model.h {
            model.px[i * 4] = model.px[i * 4].saturating_add(30);
        }

        let t = Instant::now();
        let full = stitch_crop(&base, &mask, pl.crop, &model, &p);
        let t_full = t.elapsed();
        let t = Instant::now();
        let boxed = stitch_crop_boxed(&base, &mask, pl.crop, &model, &p);
        let t_boxed = t.elapsed();

        let weights = paste_weights(&base, &mask, pl.crop, &p);
        let mut max = 0i32;
        let mut sum = 0u64;
        let mut n = 0u64;
        let mut over8 = 0u64;
        let mut zw_bad = 0u64;
        for y in 0..h {
            for x in 0..w {
                let a = full.off(x, y);
                let b = boxed.off(x, y);
                if x >= pl.crop.x
                    && x < pl.crop.x + pl.crop.w
                    && y >= pl.crop.y
                    && y < pl.crop.y + pl.crop.h
                    && weights.get(x - pl.crop.x, y - pl.crop.y) == 0
                    && (full.px[a] != base.px[a] || boxed.px[b] != base.px[b])
                {
                    // 裁切区内、权重为 0 的点：两版都必须逐字节等于原图
                    zw_bad += 1;
                }
                for c in 0..3 {
                    let d = (full.px[a + c] as i32 - boxed.px[b + c] as i32).abs();
                    if d > max {
                        max = d;
                    }
                    sum += d as u64;
                    n += 1;
                    if d > 8 {
                        over8 += 1;
                    }
                }
            }
        }
        println!(
            "两版比对 4000x6000（裁切区 {}x{}）: 整幅 {:.1}ms / 盒内 {:.1}ms；差值 max {} mean {:.4} >8 的占 {:.4}；零权重违例 {}",
            pl.crop.w,
            pl.crop.h,
            t_full.as_secs_f64() * 1000.0,
            t_boxed.as_secs_f64() * 1000.0,
            max,
            sum as f64 / n as f64,
            over8 as f64 / n as f64,
            zw_bad
        );
    }
    #[test]
    #[ignore]
    fn bench_24mp_缝合三段() {
        use std::time::Instant;
        let (w, h) = (4000usize, 6000usize);
        let mut base = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                base.set(x, y, [(x / 7) as u8, (y / 11) as u8, (x ^ y) as u8, 255]);
            }
        }
        let mut mask = Alpha::new(w, h);
        for y in h / 3..h / 2 {
            for x in w / 3..w / 2 {
                mask.v[y * w + x] = 255;
            }
        }
        let p = StitchParams::default();

        let t = Instant::now();
        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else { panic!("应该成功") };
        let build = t.elapsed();

        let mut model = Rgba::new(pl.image.w, pl.image.h);
        for i in 0..model.w * model.h {
            model.px[i * 4] = model.px[i * 4].saturating_add(30);
        }
        let t = Instant::now();
        let res = stitch_crop(&base, &mask, pl.crop, &model, &p);
        let stitch = t.elapsed();

        assert_eq!(res.w, w);
        println!(
            "bench 4000x6000: build_crop_payload {build:?} / stitch_crop {stitch:?}（裁切区 {}×{}）",
            pl.crop.w, pl.crop.h
        );
    }
}
