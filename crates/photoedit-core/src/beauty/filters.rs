//! 双边与盒式滤波；不含美颜强度和皮肤判定策略。
use px_core::par::par_chunks_mut;
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
