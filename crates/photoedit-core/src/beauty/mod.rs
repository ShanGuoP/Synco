//! 美颜段：磨皮、美白、锐化。
//!
//! 磨皮不在原分辨率上直接跑双边滤波——24MP 逐像素邻域是分钟级。
//! 流程是把亮度单独拎出来、降到长边 2048 的域里滤、再升回原尺寸，
//! 最后只把"被抹掉的那部分细节"按强度回填：颜色与细节留在原域，模糊发生在低频域。

use crate::ops::Beauty;
use crate::px::{luma, q8_flat, q8, to_ycbcr, Plane};
use px_core::{par::par_chunks_mut, Alpha, Rgba};

/// 双边滤波的邻域半径随强度从 2 走到 8
pub const BILATERAL_MIN_R: usize = 2;
pub const BILATERAL_MAX_R: usize = 8;
/// 磨皮只在长边不超过这个尺寸的域里做
pub const SMOOTH_DOMAIN: usize = 2048;
/// 强度满档时保留的细节比例：全抹平就成了油画，留 15% 的高频才像皮肤
pub const DETAIL_KEEP: f32 = 0.15;
/// 质感保留滑杆满档时的回填下限。0 档沿用 `DETAIL_KEEP`，那条曲线与 0.3.0 逐位一致
pub const TEXTURE_MAX: f32 = 0.9;
/// 祛瑕疵的软阈值（Y 的 0..255 量纲）：强度满档收到下限，零强度停在上限。
/// 阈值以下一律不动——把这一刀去掉，祛瑕疵就退化成第二支磨皮。
pub const BLEMISH_T_HI: f32 = 22.0;
pub const BLEMISH_T_LO: f32 = 6.0;
/// 匀肤搬色度的那一层低频：24 格摊 3 次，色斑的尺寸比它小而皮肤走向比它大
pub const EVEN_TONE_RADIUS: usize = 24;
/// 匀肤满档最多把色度搬掉的比例（1 = 全搬平就成了塑料片）
pub const EVEN_TONE_MAX: f32 = 0.6;
/// 去油光的作用线与压回后剩下的超出量比例：线以下中间调一根不动
pub const SHINE_FLOOR: f32 = 185.0;
pub const SHINE_KEEP: f32 = 0.25;

/// 美颜主入口。`mask` 是"涂哪儿磨哪儿"那张 alpha（与 `img` 同尺寸）；
/// `None` 表示全图生效。
pub fn apply(img: &Rgba, b: &Beauty, mask: Option<&Alpha>) -> Rgba {
    if b.is_empty() {
        return img.clone();
    }
    let (w, h) = (img.w, img.h);
    let n = w * h;
    let mut y = vec![0f32; n];
    let mut cb = vec![0f32; n];
    let mut cr = vec![0f32; n];
    for i in 0..n {
        let o = i * 4;
        let (yy, b_, r_) = to_ycbcr(img.px[o] as f32, img.px[o + 1] as f32, img.px[o + 2] as f32);
        y[i] = yy;
        cb[i] = b_;
        cr[i] = r_;
    }
    // 蒙版只展开一次，强度由各段自己乘：免得同一个滑杆被折两遍
    let msk = mask_plane(w, h, mask);

    // 匀肤就地改 cb/cr，合成时要拿改动前的值算增量。它真要跑才留这一份原件：
    // 不跑就是两次满幅 f32 拷贝，白付。
    let (cb0, cr0): (Vec<f32>, Vec<f32>) = if b.even_tone.is_zero() { (Vec::new(), Vec::new()) } else { (cb.clone(), cr.clone()) };

    if !b.blemish.is_zero() {
        blemish_luma(w, h, &mut y, &msk, b.blemish.pos());
    }
    if !b.smooth.is_zero() {
        smooth_luma(img, &mut y, &msk, b.smooth.pos(), b.texture.pos());
    }
    if !b.even_tone.is_zero() {
        even_tone_chroma(w, h, &mut cb, &mut cr, &msk, b.even_tone.pos());
    }
    if !b.brighten.is_zero() {
        brighten_luma(&mut y, &cb, &cr, &msk, b.brighten.pos());
    }
    if !b.de_shine.is_zero() {
        de_shine_luma(&mut y, &cb, &cr, &msk, b.de_shine.pos());
    }
    if !b.sharpen.is_zero() {
        unsharp_luma(img.w, img.h, &mut y, &msk, b.sharpen.pos());
    }

    // Y 的改动按"亮度 + 色度"两条增量回 RGB：只动亮度时色度增量为 0，
    // 三个通道各自加同一个 d，与 0.3.0 的合成写法逐位相同
    let mut out = Rgba::new(img.w, img.h);
    let rows = img.h.div_ceil(16).max(1);
    let src = &img.px;
    let w = img.w;
    let dy_ref = &y;
    let chroma = !cb0.is_empty();
    par_chunks_mut(&mut out.px, rows * w * 4, |blk, band| {
        let y0 = band * rows;
        for (i, px) in blk.chunks_exact_mut(4).enumerate() {
            let g = y0 * w + i;
            let (x, yy) = (g % w, g / w);
            let o = g * 4;
            let [r0, gc0, b0] = [src[o] as f32, src[o + 1] as f32, src[o + 2] as f32];
            let base = luma(r0, gc0, b0);
            let d = dy_ref[g] - base;
            // 反变换（BT.601 偏置式）：R = Y + Cr，B = Y + Cb，G = Y − 0.509·Cr − 0.194·Cb
            let (dcb, dcr) = if chroma { (cb[g] - cb0[g], cr[g] - cr0[g]) } else { (0.0, 0.0) };
            px.copy_from_slice(&[
                q8(r0 + d + dcr, x, yy),
                q8(gc0 + d - 0.509 * dcr - 0.194 * dcb, x, yy),
                q8(b0 + d + dcb, x, yy),
                src[o + 3],
            ]);
        }
    });
    out
}

/// 蒙版平面搬到 0..1。`None` 或尺寸对不上都按"全图"处理——
/// 调用方负责把 proxy 上的涂抹升采样到工作尺寸，内核不猜该怎么缩。
fn mask_plane(w: usize, h: usize, mask: Option<&Alpha>) -> Option<Vec<f32>> {
    match mask {
        Some(m) if m.w == w && m.h == h => Some(m.v.iter().map(|v| *v as f32 / 255.0).collect()),
        _ => None,
    }
}

#[inline]
fn m_at(m: &Option<Vec<f32>>, i: usize) -> f32 {
    match m {
        Some(v) => v[i],
        None => 1.0,
    }
}

/// 磨皮：Y 降采样 → 双边 → 升回 → 细节回填。
/// `texture` 是回填比例的下限：0 走 `DETAIL_KEEP`（与 0.3.0 那条曲线逐位相同），
/// 满档走到 `TEXTURE_MAX`——想要"磨了但还看得见毛孔"就往右上拖。
fn smooth_luma(img: &Rgba, y: &mut [f32], msk: &Option<Vec<f32>>, strength: f32, texture: f32) {
    let (w, h) = (img.w, img.h);
    // u8 的亮度平面走项目既有的重采样通道（Alpha 就是单通道 u8）
    let plane = Alpha::from_vec(w, h, y.iter().map(|v| q8_flat(*v)).collect());
    let scale = (SMOOTH_DOMAIN as f32 / (w.max(h) as f32)).min(1.0);
    let (sw, sh) = if scale < 1.0 {
        (((w as f32 * scale).round() as usize).max(1), ((h as f32 * scale).round() as usize).max(1))
    } else {
        (w, h)
    };
    let small = px_core::resize_alpha(&plane, sw, sh);
    let radius = BILATERAL_MIN_R + ((BILATERAL_MAX_R - BILATERAL_MIN_R) as f32 * strength).round() as usize;
    let spatial = radius as f32 * 0.6;
    let range = 8.0 + 40.0 * strength;
    let smoothed = bilateral(&small.v, sw, sh, radius, spatial, range);
    // 升回原尺寸后与本地亮度做差，差值就是"被抹掉的高频"
    let up = px_core::resize_alpha(&Alpha::from_vec(sw, sh, smoothed), w, h);
    let base = DETAIL_KEEP + (TEXTURE_MAX - DETAIL_KEEP) * texture;
    let keep = base + (1.0 - base) * (1.0 - strength);
    for i in 0..w * h {
        let k = m_at(msk, i) * strength;
        if k <= 0.0 {
            continue;
        }
        let s = up.v[i] as f32;
        let target = s + (y[i] - s) * keep;
        y[i] += (target - y[i]) * k;
    }
}

/// 祛瑕疵：与局部均值比，只把"超出阈值那一截"的偏差收回来。
/// 均值窗只有几格宽，所以它管的是痘、斑、胡青这类点状偏离；
/// 成片的红绿不均不在这条算子的活里，那是匀肤的事。
fn blemish_luma(w: usize, h: usize, y: &mut [f32], msk: &Option<Vec<f32>>, strength: f32) {
    let plane = Plane { w, h, v: y.to_vec() };
    // 窗口随强度放大：轻档只收最扎眼的那几个点，满档连浅斑一起收
    let radius = 2 + (4.0 * strength).round() as usize;
    let low = plane.into_box_blur_passes(radius, 2);
    let t = BLEMISH_T_HI - (BLEMISH_T_HI - BLEMISH_T_LO) * strength;
    for i in 0..y.len() {
        let k = m_at(msk, i) * strength;
        if k <= 0.0 {
            continue;
        }
        let d = y[i] - low.v[i];
        let ad = d.abs();
        if ad <= t {
            continue; // 阈值以下是正常纹理，一根手指都不碰
        }
        y[i] -= d * ((ad - t) / ad) * k;
    }
}

/// 锐化：亮度域一次 3×3 高屏回填，同样受蒙版与强度控制
fn unsharp_luma(w: usize, h: usize, y: &mut [f32], msk: &Option<Vec<f32>>, strength: f32) {
    let mut p = Alpha::new(w, h);
    for i in 0..y.len() {
        p.v[i] = q8_flat(y[i]);
    }
    let low = box_blur_u8(&p.v, w, h, 1);
    let amt = 1.4 * strength;
    for i in 0..y.len() {
        let k = m_at(msk, i);
        if k <= 0.0 {
            continue;
        }
        y[i] += (y[i] - low[i] as f32) * amt * k;
    }
}


mod filters;
mod tone;
pub use filters::{bilateral, box_blur_u8};
use tone::{brighten_luma, even_tone_chroma, de_shine_luma};
#[cfg(test)]
mod tests;
