//! 美颜段：磨皮、美白、锐化。
//!
//! 磨皮不在原分辨率上直接跑双边滤波——24MP 逐像素邻域是分钟级。
//! 流程是把亮度单独拎出来、降到长边 2048 的域里滤、再升回原尺寸，
//! 最后只把"被抹掉的那部分细节"按强度回填：颜色与细节留在原域，模糊发生在低频域。

use crate::ops::Beauty;
use crate::px::{luma, q8_flat, q8, to_ycbcr};
use stitch_core::{par::par_chunks_mut, Alpha, Rgba};

/// 双边滤波的邻域半径随强度从 2 走到 8
pub const BILATERAL_MIN_R: usize = 2;
pub const BILATERAL_MAX_R: usize = 8;
/// 磨皮只在长边不超过这个尺寸的域里做
pub const SMOOTH_DOMAIN: usize = 2048;
/// 强度满档时保留的细节比例：全抹平就成了油画，留 15% 的高频才像皮肤
pub const DETAIL_KEEP: f32 = 0.15;

/// 美颜主入口。`mask` 是"涂哪儿磨哪儿"那张 alpha（与 `img` 同尺寸）；
/// `None` 表示全图生效。
pub fn apply(img: &Rgba, b: &Beauty, mask: Option<&Alpha>) -> Rgba {
    if b.is_empty() {
        return img.clone();
    }
    let n = img.w * img.h;
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
    let msk = mask_plane(img.w, img.h, mask);

    if !b.smooth.is_zero() {
        smooth_luma(img, &mut y, &msk, b.smooth.pos());
    }
    if !b.brighten.is_zero() {
        brighten_luma(&mut y, &cb, &cr, &msk, b.brighten.pos());
    }
    if !b.sharpen.is_zero() {
        unsharp_luma(img.w, img.h, &mut y, &msk, b.sharpen.pos());
    }

    // Y 的改动以"整体加到三通道"的方式回 RGB：色度平面不动，肤色不会被推偏
    let mut out = Rgba::new(img.w, img.h);
    let rows = img.h.div_ceil(16).max(1);
    let src = &img.px;
    let w = img.w;
    let dy_ref = &y;
    par_chunks_mut(&mut out.px, rows * w * 4, |blk, band| {
        let y0 = band * rows;
        for (i, px) in blk.chunks_exact_mut(4).enumerate() {
            let g = y0 * w + i;
            let (x, yy) = (g % w, g / w);
            let o = g * 4;
            let [r0, gc0, b0] = [src[o] as f32, src[o + 1] as f32, src[o + 2] as f32];
            let base = luma(r0, gc0, b0);
            let d = dy_ref[g] - base;
            px.copy_from_slice(&[q8(r0 + d, x, yy), q8(gc0 + d, x, yy), q8(b0 + d, x, yy), src[o + 3]]);
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

/// 磨皮：Y 降采样 → 双边 → 升回 → 细节回填
fn smooth_luma(img: &Rgba, y: &mut [f32], msk: &Option<Vec<f32>>, strength: f32) {
    let (w, h) = (img.w, img.h);
    // u8 的亮度平面走项目既有的重采样通道（Alpha 就是单通道 u8）
    let plane = Alpha::from_vec(w, h, y.iter().map(|v| q8_flat(*v)).collect());
    let scale = (SMOOTH_DOMAIN as f32 / (w.max(h) as f32)).min(1.0);
    let (sw, sh) = if scale < 1.0 {
        (((w as f32 * scale).round() as usize).max(1), ((h as f32 * scale).round() as usize).max(1))
    } else {
        (w, h)
    };
    let small = stitch_core::resize_alpha(&plane, sw, sh);
    let radius = BILATERAL_MIN_R + ((BILATERAL_MAX_R - BILATERAL_MIN_R) as f32 * strength).round() as usize;
    let spatial = radius as f32 * 0.6;
    let range = 8.0 + 40.0 * strength;
    let smoothed = bilateral(&small.v, sw, sh, radius, spatial, range);
    // 升回原尺寸后与本地亮度做差，差值就是"被抹掉的高频"
    let up = stitch_core::resize_alpha(&Alpha::from_vec(sw, sh, smoothed), w, h);
    let keep = DETAIL_KEEP + (1.0 - DETAIL_KEEP) * (1.0 - strength);
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
    use crate::ops::Beauty;
    use stitch_core::Alpha;

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
        Beauty { smooth: crate::ops::Slider(smooth), brighten: crate::ops::Slider(brighten), sharpen: crate::ops::Slider(sharpen), by_mask: false }
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
}
