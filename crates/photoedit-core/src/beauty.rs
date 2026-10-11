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

/// 双边滤波：空间高斯 × 灰度差高斯。按行带并行，带上下各多读 `radius` 行，
/// 这样每个输出像素看到的邻域完全一致——分块不会在带边界留下接缝。
pub fn bilateral(src: &[u8], w: usize, h: usize, radius: usize, spatial: f32, range: f32) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    if radius == 0 {
        out.copy_from_slice(src);
        return out;
    }
    let s2 = 2.0 * spatial * spatial;
    let r2 = 2.0 * range * range;
    let stride = 2 * radius + 1;
    // 预置空间核与量化到 1/4 灰度步长的范围核：范围核查表把内层三重循环砍成两层
    let mut kw = vec![0f32; stride * stride];
    for dy in -(radius as i32)..=(radius as i32) {
        for dx in -(radius as i32)..=(radius as i32) {
            let d2 = (dx * dx + dy * dy) as f32;
            kw[((dy + radius as i32) * stride as i32 + (dx + radius as i32)) as usize] = (-d2 / s2).exp();
        }
    }
    // 灰度差量化到 1/4 档：|差| 最大 255 → 索引最大 1020
    let mut kr = vec![0f32; 1025];
    for i in 0..=1024 {
        let d = i as f32 / 4.0;
        kr[i] = (-(d * d) / r2).exp();
    }
    let rows = h.div_ceil(16).max(1);
    par_chunks_mut(&mut out, rows * w, |blk, band| {
        let y0 = band * rows;
        for (i, o) in blk.iter_mut().enumerate() {
            let g = y0 * w + i;
            let x = g % w;
            let y = g / w;
            let c = src[g] as i32;
            let (x0, x1) = (x.saturating_sub(radius), (x + radius).min(w - 1));
            let (y0b, y1b) = (y.saturating_sub(radius), (y + radius).min(h - 1));
            let mut acc = 0f32;
            let mut sum = 0f32;
            for sy in y0b..=y1b {
                let ky = (sy as i32 - y as i32 + radius as i32) as usize;
                let row = &src[sy * w..(sy + 1) * w];
                for sx in x0..=x1 {
                    let v = row[sx] as f32;
                    let d = ((c - row[sx] as i32).abs() * 4).min(1024) as usize;
                    let k = kw[ky * stride + (sx as i32 - x as i32 + radius as i32) as usize] * kr[d];
                    acc += v * k;
                    sum += k;
                }
            }
            *o = if sum > 0.0 { (acc / sum).round().clamp(0.0, 255.0) as u8 } else { src[g] };
        }
    });
    out
}

/// 美白：肤色域软掩码 × gamma 提亮。非肤色（蓝天、灰墙）几乎不动，避免整张过曝。
fn brighten_luma(y: &mut [f32], cb: &[f32], cr: &[f32], msk: &Option<Vec<f32>>, strength: f32) {
    for i in 0..y.len() {
        let k = m_at(msk, i) * skin_weight(cb[i], cr[i]) * strength;
        if k <= 1e-3 {
            continue;
        }
        let v = y[i] / 255.0;
        // gamma 从 1 走到 0.72：满档时中灰提到约 186
        let g = 1.0 - 0.28 * k;
        let lifted = v.powf(g) * 255.0;
        y[i] += (lifted - y[i]) * k;
    }
}

/// 肤色隶属度：Cb/Cr 的经典盒式范围 + 软边，范围外平滑到 0。
/// `to_ycbcr` 给出的是 ±128 那套量纲，所以要加回 128 才是常见的 Cb/Cr 数值。
fn skin_weight(cb: f32, cr: f32) -> f32 {
    let b = cb + 128.0;
    let r = cr + 128.0;
    let soft = |v: f32, lo: f32, hi: f32| {
        if v <= lo || v >= hi {
            0.0
        } else {
            ((v - lo) / 12.0).min(1.0) * ((hi - v) / 12.0).min(1.0)
        }
    };
    soft(b, 77.0, 127.0) * soft(r, 133.0, 173.0)
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

/// 匀肤：把 Cb/Cr 往大尺度那一层搬，亮度与细节都不动。
/// 只在肤色域里做——蓝天与白墙的色度也照搬就会整片发灰。
fn even_tone_chroma(w: usize, h: usize, cb: &mut [f32], cr: &mut [f32], msk: &Option<Vec<f32>>, strength: f32) {
    let low_b = Plane { w, h, v: cb.to_vec() }.into_box_blur_passes(EVEN_TONE_RADIUS, 3);
    let low_r = Plane { w, h, v: cr.to_vec() }.into_box_blur_passes(EVEN_TONE_RADIUS, 3);
    for i in 0..cb.len() {
        // 门限读搬动前的色度：先取值再写，不然第二格用到的就是被第一格改过的
        let (b0, r0) = (cb[i], cr[i]);
        let k = skin_weight(b0, r0) * m_at(msk, i) * strength * EVEN_TONE_MAX;
        if k <= 1e-3 {
            continue;
        }
        cb[i] = b0 + (low_b.v[i] - b0) * k;
        cr[i] = r0 + (low_r.v[i] - r0) * k;
    }
}

/// 去油光：肤色域里超过 `SHINE_FLOOR` 的那一截高光按比例压回线上，线以下不动。
/// 压的是"超出量"而不是整块调暗，所以 T 区的油光掉了而鼻梁的立体感还在。
fn de_shine_luma(y: &mut [f32], cb: &[f32], cr: &[f32], msk: &Option<Vec<f32>>, strength: f32) {
    for i in 0..y.len() {
        let over = y[i] - SHINE_FLOOR;
        if over <= 0.0 {
            continue;
        }
        let k = skin_weight(cb[i], cr[i]) * m_at(msk, i) * strength;
        if k <= 1e-3 {
            continue;
        }
        let target = SHINE_FLOOR + over * SHINE_KEEP;
        y[i] += (target - y[i]) * k;
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

/// 一次行+列盒式模糊，作用在 u8 平面上
pub fn box_blur_u8(src: &[u8], w: usize, h: usize, radius: usize) -> Vec<u8> {
    let mut tmp = vec![0f32; w * h];
    let mut rows = vec![0f32; w];
    for y in 0..h {
        let win = 2 * radius + 1;
        let mut acc = 0f32;
        for k in 0..win {
            acc += src[y * w + k.saturating_sub(radius).min(w - 1)] as f32;
        }
        for x in 0..w {
            rows[x] = acc / win as f32;
            acc += src[y * w + (x + radius + 1).min(w - 1)] as f32 - src[y * w + x.saturating_sub(radius)] as f32;
        }
        tmp[y * w..y * w + w].copy_from_slice(&rows);
    }
    let mut out = vec![0u8; w * h];
    let win = 2 * radius + 1;
    for x in 0..w {
        let mut acc = 0f32;
        for k in 0..win {
            acc += tmp[k.saturating_sub(radius).min(h - 1) * w + x];
        }
        for y in 0..h {
            out[y * w + x] = (acc / win as f32).round().clamp(0.0, 255.0) as u8;
            acc += tmp[(y + radius + 1).min(h - 1) * w + x] - tmp[y.saturating_sub(radius) * w + x];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{Beauty, Slider};
    use px_core::Alpha;

    /// 合成"皮肤块"：底色 180 上加确定性噪声，再叠一条硬边缘当下颌线
    fn skin(w: usize, h: usize) -> Rgba {
        let mut img = Rgba::new(w, h);
        let mut s = 12345u32;
        for y in 0..h {
            for x in 0..w {
                s = s.wrapping_mul(1103515245).wrapping_add(12345);
                let n = ((s >> 16) % 41) as i32 - 20;
                let base = if x < w / 2 { 190 } else { 90 };
                // 偏暖的皮肤色：R>G>B，落在 Cb/Cr 的肤色盒里
                let v = (base + n).clamp(0, 255) as u8;
                img.set(x, y, [v, (v as i32 * 82 / 100) as u8, (v as i32 * 66 / 100) as u8, 255]);
            }
        }
        img
    }

    fn stats(img: &Rgba, x0: usize, x1: usize) -> (f32, f32) {
        let (mut s, mut s2) = (0f64, 0f64);
        let n = (x1 - x0) * img.h;
        for y in 0..img.h {
            for x in x0..x1 {
                let p = img.get(x, y);
                let v = luma(p[0] as f32, p[1] as f32, p[2] as f32) as f64;
                s += v;
                s2 += v * v;
            }
        }
        let m = s / n as f64;
        (m as f32, ((s2 / n as f64) - m * m).sqrt() as f32)
    }

    fn b(smooth: i32, brighten: i32, sharpen: i32) -> Beauty {
        Beauty { smooth: Slider(smooth), brighten: Slider(brighten), sharpen: Slider(sharpen), ..Default::default() }
    }

    #[test]
    fn 全零美颜逐位不变() {
        let img = skin(64, 32);
        assert_eq!(apply(&img, &Beauty::default(), None), img);
    }

    #[test]
    fn 磨皮压噪声不压硬边缘() {
        let img = skin(64, 32);
        let (m0, sd0) = stats(&img, 4, 28);
        let out = apply(&img, &b(70, 0, 0), None);
        let (m1, sd1) = stats(&out, 4, 28);
        assert!(sd1 < sd0 * 0.75, "块内噪声该被压掉一大截：{sd0} → {sd1}");
        assert!((m1 - m0).abs() < 6.0, "均值不该漂走：{m0} → {m1}");
        // 中间那条硬边仍在：跨界梯度还比块内噪声标准差大得多
        let left = stats(&out, 24, 28).1;
        let edge = (stats(&out, 32, 36).0 - stats(&out, 26, 30).0).abs();
        assert!(edge > 40.0, "边缘被磨平了：落差 {edge}");
        assert!(left < 30.0);
    }

    #[test]
    fn 强度越大磨得越狠() {
        let img = skin(48, 24);
        let a = stats(&apply(&img, &b(30, 0, 0), None), 4, 20).1;
        let bb = stats(&apply(&img, &b(90, 0, 0), None), 4, 20).1;
        assert!(bb < a, "{a} 应该大于 {bb}");
    }

    #[test]
    fn 蒙版之外的像素一点不动() {
        let img = skin(40, 20);
        let mut m = Alpha::new(40, 20);
        for y in 0..20 {
            for x in 0..40 {
                m.set(x, y, if x < 10 { 255 } else { 0 });
            }
        }
        let out = apply(&img, &b(90, 90, 90), Some(&m));
        for y in 0..20 {
            for x in 12..40 {
                assert_eq!(out.get(x, y), img.get(x, y), "蒙版外的被动过：({x},{y})");
            }
        }
        assert_ne!(out.get(0, 10), img.get(0, 10), "蒙版内要有效果");
    }

    #[test]
    fn 美白偏爱肤色不偏爱蓝天() {
        let mut img = Rgba::new(8, 2);
        for x in 0..8 {
            img.set(x, 0, [200, 170, 150, 255]); // 暖肤色
            img.set(x, 1, [120, 170, 230, 255]); // 偏蓝的背景
        }
        let out = apply(&img, &b(0, 100, 0), None);
        let skin_up = luma(out.get(0, 0)[0] as f32, out.get(0, 0)[1] as f32, out.get(0, 0)[2] as f32)
            - luma(200.0, 170.0, 150.0);
        let sky_up = luma(out.get(0, 1)[0] as f32, out.get(0, 1)[1] as f32, out.get(0, 1)[2] as f32) - luma(120.0, 170.0, 230.0);
        assert!(skin_up > 8.0, "肤色没提亮：{skin_up}");
        assert!(skin_up > sky_up * 2.0, "肤色 {skin_up} 应明显强于非肤色 {sky_up}");
    }

    #[test]
    fn 美颜锐化让边缘更陡() {
        let mut img = Rgba::new(40, 8);
        for y in 0..8 {
            for x in 0..40 {
                let v = if x < 20 { 60u8 } else { 190u8 };
                img.set(x, y, [v, v, v, 255]);
            }
        }
        let out = apply(&img, &b(0, 0, 80), None);
        assert!(out.get(19, 4)[0] < 60, "暗侧要更暗：{}", out.get(19, 4)[0]);
        assert!(out.get(20, 4)[0] > 190, "亮侧要更亮：{}", out.get(20, 4)[0]);
        assert_eq!(out.get(2, 4)[0], 60, "远离边缘的不该动");
    }

    #[test]
    fn 双边滤波对常数图恒等() {
        let src = vec![128u8; 64 * 64];
        let out = bilateral(&src, 64, 64, 4, 2.4, 20.0);
        assert!(out.iter().all(|v| *v == 128), "常数图被改动了");
    }

    #[test]
    fn 双边滤波压噪声但保住台阶() {
        // 一侧 40、一侧 220，两侧各自叠 ±8 的确定性噪声
        let mut src = vec![0u8; 64 * 64];
        let mut s = 7u32;
        for y in 0..64 {
            for x in 0..64 {
                s = s.wrapping_mul(1103515245).wrapping_add(12345);
                let n = ((s >> 20) % 17) as i32 - 8;
                src[y * 64 + x] = ((if x < 32 { 40 } else { 220 }) + n).clamp(0, 255) as u8;
            }
        }
        let out = bilateral(&src, 64, 64, 3, 1.8, 12.0);
        // 远离台阶的地方：噪声被压掉
        let var = |v: &[u8], lo: usize, hi: usize| {
            let m = v[lo..hi].iter().map(|x| *x as f64).sum::<f64>() / (hi - lo) as f64;
            v[lo..hi].iter().map(|x| (*x as f64 - m).powi(2)).sum::<f64>() / (hi - lo) as f64
        };
        assert!(var(&out, 2, 26) < var(&src, 2, 26), "块内噪声没被压掉：{} vs {}", var(&out, 2, 26), var(&src, 2, 26));
        // 台阶位置不漂：跨过边界的两个像素仍然分处两端
        assert!(out[32 * 64 + 30] < 60, "暗侧被抬了：{}", out[32 * 64 + 30]);
        assert!(out[32 * 64 + 34] > 200, "亮侧被拖了：{}", out[32 * 64 + 34]);
        // 常数一侧不该出现第二个值域
        assert!(out[32 * 64 + 4] < 60 && out[32 * 64 + 60] > 200, "两端基准值漂了");
    }

    #[test]
    fn 分块并行与串行同结果() {
        // 行带切分最怕带边界接缝：拿一张有噪声的图整体比对
        let img = skin(129, 97);
        let mut y = Vec::new();
        for i in 0..129 * 97 {
            y.push(img.px[i * 4] as f32);
        }
        let a = bilateral(&y.iter().map(|v| q8_flat(*v)).collect::<Vec<u8>>(), 129, 97, 5, 3.0, 24.0);
        // 同一个核再单线程跑一遍
        let mut b = vec![0u8; a.len()];
        let w = 129;
        let h = 97;
        let (s2, r2) = (2.0 * 3.0 * 3.0, 2.0 * 24.0 * 24.0);
        for yy in 0..h {
            for xx in 0..w {
                let c = y[yy * w + xx] as i32;
                let (mut acc, mut sum) = (0f32, 0f32);
                for dy in -5..=5i32 {
                    for dx in -5..=5i32 {
                        let sx = xx as i32 + dx;
                        let sy = yy as i32 + dy;
                        if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                            continue;
                        }
                        let v = y[sy as usize * w + sx as usize] as f32;
                        let kw = (-((dx * dx + dy * dy) as f32) / s2).exp();
                        let dw = v - c as f32;
                        let kr = (-(dw * dw) / r2).exp();
                        acc += v * kw * kr;
                        sum += kw * kr;
                    }
                }
                b[yy * w + xx] = (acc / sum).round().clamp(0.0, 255.0) as u8;
            }
        }
        // 边缘钳位策略一致时最多差一档
        let mut worst = 0i32;
        for i in 0..a.len() {
            worst = worst.max((a[i] as i32 - b[i] as i32).abs());
        }
        assert!(worst <= 2, "并行与串行差 {worst} 档");
    }

    fn fp(img: &Rgba, ops: &Beauty, m: Option<&Alpha>) -> u64 {
        let o = apply(img, ops, m);
        let mut s: u64 = 0xcbf29ce484222325;
        for v in &o.px {
            s ^= *v as u64;
            s = s.wrapping_mul(0x100000001b3);
        }
        s
    }

    /// 无噪声皮肤底：跨阈值的断言要在平底上做——`skin()` 自带 ±20 的噪声，
    /// 那噪声本身就够把阈值顶过去，用它验阈值等于没验
    fn flat_skin(w: usize, h: usize, v0: u8) -> Rgba {
        let mut img = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set(x, y, [v0, (v0 as u32 * 82 / 100) as u8, (v0 as u32 * 66 / 100) as u8, 255]);
            }
        }
        img
    }

    /// 在皮肤色 ramp 上按明度偏移画一块 2r+1 的方点
    fn dot(img: &mut Rgba, cx: usize, cy: usize, rad: usize, dv: i32) {
        let (w, h) = (img.w, img.h);
        for y in cy.saturating_sub(rad)..=cy.min(h - 1) {
            for x in cx.saturating_sub(rad)..=cx.min(w - 1) {
                let v = (img.get(x, y)[0] as i32 + dv).clamp(0, 255) as u8;
                img.set(x, y, [v, (v as u32 * 82 / 100) as u8, (v as u32 * 66 / 100) as u8, 255]);
            }
        }
    }

    fn nb(blemish: i32, even_tone: i32, de_shine: i32) -> Beauty {
        Beauty { blemish: Slider(blemish), even_tone: Slider(even_tone), de_shine: Slider(de_shine), ..Default::default() }
    }

    fn tex(smooth: i32, texture: i32) -> Beauty {
        Beauty { smooth: Slider(smooth), texture: Slider(texture), ..Default::default() }
    }

    /// 合成从"亮度差整体加到三通道"改成"亮度 + 色度两条增量"之后，
    /// 老三根杆必须逐位不变（色度增量为 0 时新写法退化回原式）。指纹取自改写前那一版。
    #[test]
    fn 改合成后老参数逐位不变() {
        let a = skin(64, 32);
        let c = skin(48, 24);
        let mut m = Alpha::new(64, 32);
        for y in 0..32 {
            for x in 0..64 {
                m.v[y * 64 + x] = if x < 20 { 255 } else if x < 30 { 96 } else { 0 };
            }
        }
        assert_eq!(fp(&a, &b(70, 0, 0), None), 0x810f878710c9900f);
        assert_eq!(fp(&a, &b(0, 40, 0), None), 0x97e629841d0b2ef2);
        assert_eq!(fp(&a, &b(0, 0, 30), None), 0xb03b58ab31019e6c);
        assert_eq!(fp(&a, &b(70, 40, 30), None), 0x749624b042d68b39);
        assert_eq!(fp(&c, &b(100, 100, 100), None), 0x8c1b0d0aa3d056ba);
        assert_eq!(fp(&a, &b(70, 40, 30), Some(&m)), 0x5c3a1078d93b3133);
        assert_eq!(fp(&a, &b(0, 0, 0), Some(&m)), 0xc9bf88fadfb30f34);
    }

    /// 阈值两侧都要有样本：深痣被收、浅印一格不动
    #[test]
    fn 祛瑕疵收深痣不抹浅印() {
        let mut img = flat_skin(60, 60, 180);
        dot(&mut img, 12, 12, 1, -60); // 深痣：ΔY ≈ -51，远在阈值外
        dot(&mut img, 40, 40, 1, 12); // 浅印：ΔY ≈ 10，在 35 档的阈值以内
        let y_at = |im: &Rgba, x: usize, yy: usize| {
            let p = im.get(x, yy);
            luma(p[0] as f32, p[1] as f32, p[2] as f32)
        };
        let base = y_at(&img, 30, 30);
        let dev = |im: &Rgba, x: usize, yy: usize| (y_at(im, x, yy) - base).abs();

        let mid = apply(&img, &nb(35, 0, 0), None);
        for yy in 38..43 {
            for x in 38..43 {
                assert_eq!(mid.get(x, yy), img.get(x, yy), "浅印 ({x},{yy}) 被动了：阈值失效");
            }
        }
        assert!(dev(&mid, 12, 12) < dev(&img, 12, 12) * 0.85, "深痣没被收掉：{} → {}", dev(&img, 12, 12), dev(&mid, 12, 12));

        let full = apply(&img, &nb(100, 0, 0), None);
        assert!(dev(&full, 12, 12) < dev(&img, 12, 12) * 0.4, "满档该把深痣收掉大半：{}", dev(&full, 12, 12));
        // 离痣远的平底一格都不该动
        for yy in 25..35 {
            for x in 25..35 {
                assert_eq!(full.get(x, yy), img.get(x, yy), "平底 ({x},{yy}) 被抹了");
            }
        }
    }

    /// 蒙版决定作用范围：涂到的那颗收掉，没涂的那颗原样留着
    #[test]
    fn 祛瑕疵只改涂过的那一块() {
        let mut img = flat_skin(60, 40, 180);
        dot(&mut img, 10, 20, 1, -60);
        dot(&mut img, 45, 20, 1, -60);
        let mut m = Alpha::new(60, 40);
        for y in 0..40 {
            for x in 0..60 {
                m.v[y * 60 + x] = if x < 30 { 255 } else { 0 };
            }
        }
        let out = apply(&img, &nb(100, 0, 0), Some(&m));
        let y_at = |im: &Rgba, x: usize, yy: usize| {
            let p = im.get(x, yy);
            luma(p[0] as f32, p[1] as f32, p[2] as f32)
        };
        assert!(y_at(&out, 10, 20) > y_at(&img, 10, 20) + 12.0, "涂过的那颗没收掉");
        for yy in 18..23 {
            for x in 43..48 {
                assert_eq!(out.get(x, yy), img.get(x, yy), "没涂的那颗被改了 ({x},{yy})");
            }
        }
    }

    /// 匀肤改的是色度：偏红那块朝周围靠，蓝天一格不动，亮度基本不漂
    #[test]
    fn 匀肤搬色度不碰蓝天不漂亮度() {
        let mut img = flat_skin(80, 40, 180);
        // 中间一块偏红：R 抬 26、B 压 8（Cr 升、Cb 降），仍在肤色盒内
        for y in 14..26 {
            for x in 30..46 {
                let p = img.get(x, y);
                img.set(x, y, [(p[0] as i32 + 26).clamp(0, 255) as u8, p[1], (p[2] as i32 - 8).max(0) as u8, 255]);
            }
        }
        // 右侧一条蓝天（非肤色）
        for y in 0..40 {
            for x in 72..80 {
                img.set(x, y, [90, 140, 210, 255]);
            }
        }
        let cr_at = |im: &Rgba, x: usize, yy: usize| {
            let p = im.get(x, yy);
            to_ycbcr(p[0] as f32, p[1] as f32, p[2] as f32).2
        };
        let before = (cr_at(&img, 38, 20) - cr_at(&img, 8, 20)).abs();
        assert!(before > 6.0, "样张本身得真的偏红，否则这条断言是空的：{before}");
        let out = apply(&img, &nb(0, 100, 0), None);
        let after = (cr_at(&out, 38, 20) - cr_at(&out, 8, 20)).abs();
        assert!(after < before * 0.85, "偏红那块没被拉近：{before} → {after}");
        assert_ne!(cr_at(&out, 38, 20), cr_at(&img, 38, 20), "色度一点没动：合成那步把色度丢了");
        for y in 0..40 {
            for x in 72..80 {
                assert_eq!(out.get(x, y), img.get(x, y), "蓝天 ({x},{y}) 被匀了");
            }
        }
        let (m0, _) = stats(&img, 30, 46);
        let (m1, _) = stats(&out, 30, 46);
        assert!((m1 - m0).abs() < 1.5, "匀肤把亮度带跑了：{m0} → {m1}");
    }

    /// 去油光只压肤色里超线的那一截：中间调与白衬衫逐格不动
    #[test]
    fn 去油光压肤色高光不碰中间调与白衬衫() {
        let mut img = flat_skin(80, 40, 150);
        dot(&mut img, 12, 12, 3, 95); // 油光斑：皮肤 ramp 上到 245，Y ≈ 207 越过 185 那条线
        for y in 0..40 {
            for x in 60..80 {
                img.set(x, y, [240, 240, 240, 255]); // 白衬衫：同样超线但不是肤色
            }
        }
        let y_at = |im: &Rgba, x: usize, yy: usize| {
            let p = im.get(x, yy);
            luma(p[0] as f32, p[1] as f32, p[2] as f32)
        };
        let out = apply(&img, &nb(0, 0, 100), None);
        assert!(y_at(&out, 12, 12) < y_at(&img, 12, 12) - 8.0, "油光没压下去：{} → {}", y_at(&img, 12, 12), y_at(&out, 12, 12));
        for y in 0..40 {
            for x in 60..80 {
                assert_eq!(out.get(x, y), img.get(x, y), "白衬衫 ({x},{y}) 被压暗了");
            }
            for x in 30..56 {
                assert_eq!(out.get(x, y), img.get(x, y), "中间调 ({x},{y}) 被动了");
            }
        }
    }

    /// 质感保留是磨皮的高频回填下限：拉到高档必须留下更多细节
    #[test]
    fn 质感保留越高留下的细节越多() {
        let img = skin(64, 32);
        let lo = apply(&img, &tex(80, 0), None);
        let hi = apply(&img, &tex(80, 90), None);
        let (sd0, sd_lo, sd_hi) = (stats(&img, 4, 20).1, stats(&lo, 4, 20).1, stats(&hi, 4, 20).1);
        assert!(sd_lo < sd0, "满磨先要压掉噪声：{sd0} → {sd_lo}");
        assert!(sd_hi > sd_lo * 1.15, "质感 90 该比 0 留下更多细节：{sd_lo} vs {sd_hi}");
    }

    /// 三根新杆全开，但蒙版是空的：一格都不许动
    #[test]
    fn 空蒙版下新算子不动一格() {
        let img = skin(64, 32);
        let m = Alpha::new(64, 32);
        assert_eq!(apply(&img, &nb(100, 100, 100), Some(&m)), img);
    }
}
