//! 重采样统一走 fast_image_resize：`use_alpha(true)` 的预乘语义就是 canvas drawImage 的语义，
//! 卷积核用 CatmullRom（Skia 的 kHigh 档也是三次核）。
//!
//! 但"和 Chromium 逐像素相同"这件事没有依据，差值由 M1-6 的对拍 harness 出数字。

use crate::buffer::{Alpha, Rgba};
use fast_image_resize::images::Image as FirImage;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use std::cell::RefCell;

/// 金字塔的 `half`/`grow` 与裁切区缩放共用这一个核。
/// 选 Bilinear 而不是三次核：CatmullRom 的负瓣在"框外全透明"的中间画布上会被预乘反除放大，
/// 实测把权重满处的输出顶到 221（模型输出是 200）；而 Chromium 的 `half` 走的是 mipmap 三线性，
/// 本来也不是三次卷积。最终差异以 M1-6 对 JS 的实测为准，改这一行就能换档复测。
pub const RESIZE_FILTER: FilterType = FilterType::Bilinear;

thread_local! {
    /// Resizer 内部会复用卷积缓冲，每次都新建会把分配摊到每个金字塔层上
    static FIR: RefCell<Resizer> = RefCell::new(Resizer::new());
}

fn opts() -> ResizeOptions {
    ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(RESIZE_FILTER))
        .use_alpha(true)
}

fn fir(src: Vec<u8>, sw: usize, sh: usize, dw: usize, dh: usize, pt: PixelType) -> Vec<u8> {
    if sw == dw && sh == dh {
        return src; // 1:1 不经过卷积，保证逐像素搬运
    }
    let src_img = FirImage::from_vec_u8(sw as u32, sh as u32, src, pt).expect("源缓冲与尺寸不符");
    let mut dst_img = FirImage::new(dw as u32, dh as u32, pt);
    let o = opts();
    FIR.with(|r| r.borrow_mut().resize(&src_img, &mut dst_img, Some(&o)))
        .expect("fast_image_resize 调用失败");
    dst_img.into_vec()
}

pub fn resize_rgba(src: &Rgba, dw: usize, dh: usize) -> Rgba {
    crop_scale_rgba(src, 0, 0, src.w, src.h, dw, dh)
}

pub fn resize_alpha(src: &Alpha, dw: usize, dh: usize) -> Alpha {
    crop_scale_alpha(src, 0, 0, src.w, src.h, dw, dh)
}

/// 源矩形 → 目标尺寸，等价于 canvas 的 `drawImage(cv, sx, sy, sw, sh, 0, 0, pw, ph)`
pub fn crop_scale_rgba(src: &Rgba, sx: usize, sy: usize, sw: usize, sh: usize, dw: usize, dh: usize) -> Rgba {
    if dw == 0 || dh == 0 {
        return Rgba::new(dw, dh);
    }
    let sx = sx.min(src.w);
    let sy = sy.min(src.h);
    let sw = sw.min(src.w - sx);
    let sh = sh.min(src.h - sy);
    if sw == 0 || sh == 0 {
        return Rgba::new(dw, dh);
    }
    if sw == dw && sh == dh {
        let mut out = Vec::with_capacity(sw * sh * 4);
        for y in 0..sh {
            let row = (sy + y) * src.w * 4 + sx * 4;
            out.extend_from_slice(&src.px[row..row + sw * 4]);
        }
        return Rgba::from_pixels(dw, dh, out);
    }
    let mut buf = Vec::with_capacity(sw * sh * 4);
    for y in 0..sh {
        let row = (sy + y) * src.w * 4 + sx * 4;
        buf.extend_from_slice(&src.px[row..row + sw * 4]);
    }
    Rgba::from_pixels(dw, dh, fir(buf, sw, sh, dw, dh, PixelType::U8x4))
}

pub fn crop_scale_alpha(src: &Alpha, sx: usize, sy: usize, sw: usize, sh: usize, dw: usize, dh: usize) -> Alpha {
    if dw == 0 || dh == 0 {
        return Alpha::new(dw, dh);
    }
    let sx = sx.min(src.w);
    let sy = sy.min(src.h);
    let sw = sw.min(src.w - sx);
    let sh = sh.min(src.h - sy);
    if sw == 0 || sh == 0 {
        return Alpha::new(dw, dh);
    }
    let mut buf = Vec::with_capacity(sw * sh);
    for y in 0..sh {
        let row = (sy + y) * src.w + sx;
        buf.extend_from_slice(&src.v[row..row + sw]);
    }
    Alpha::from_vec(dw, dh, fir(buf, sw, sh, dw, dh, PixelType::U8))
}

/// `half()`：Math.max(1, Math.ceil(x / 2))
pub fn half_rgba(src: &Rgba) -> Rgba {
    resize_rgba(src, (src.w + 1) / 2, (src.h + 1) / 2)
}

pub fn half_alpha(src: &Alpha) -> Alpha {
    resize_alpha(src, (src.w + 1) / 2, (src.h + 1) / 2)
}

/// `grow()`：放大到指定尺寸
pub fn grow_rgba(src: &Rgba, w: usize, h: usize) -> Rgba {
    resize_rgba(src, w, h)
}

pub fn grow_alpha(src: &Alpha, w: usize, h: usize) -> Alpha {
    resize_alpha(src, w, h)
}

/// Uint8ClampedArray 的写入行为：夹到 0~255 后按 tie-to-even 取整（WebIDL 规定），
/// 用 `round()` 会让 .5 全部向上，和 JS 版差 1 LSB，对拍时会被当成真实差异。
pub fn to_u8(v: f32) -> u8 {
    if v.is_nan() || v <= 0.0 {
        return 0;
    }
    if v >= 255.0 {
        return 255;
    }
    let f = v.floor();
    let d = v - f;
    let n = if d > 0.5 {
        f + 1.0
    } else if d < 0.5 {
        f
    } else if (f as i32) % 2 == 0 {
        f
    } else {
        f + 1.0
    };
    (n as i32).clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 源矩形偏移对_alpha_生效() {
        // 回归：曾经漏加 sx，读到 x=0 起的内容，裁切区整块左移
        let mut a = Alpha::new(20, 2);
        for y in 0..2 {
            for x in 0..20 {
                a.v[y * 20 + x] = if x >= 10 { 255 } else { 0 };
            }
        }
        assert_eq!(crop_scale_alpha(&a, 10, 0, 10, 2, 10, 2).v[0], 255);
        assert_eq!(crop_scale_alpha(&a, 0, 0, 10, 2, 10, 2).v[0], 0);
        // 纵向偏移同理
        let mut b = Alpha::new(4, 4);
        for y in 2..4 {
            for x in 0..4 {
                b.v[y * 4 + x] = 255;
            }
        }
        let s = crop_scale_alpha(&b, 0, 2, 4, 2, 4, 2);
        assert_eq!(s.v[0], 255);
        assert_eq!(crop_scale_alpha(&b, 0, 0, 4, 2, 4, 2).v[0], 0);
    }

    #[test]
    fn 源矩形偏移对_rgba_生效() {
        let mut a = Rgba::new(20, 1);
        for x in 0..20 {
            a.set(x, 0, [if x >= 10 { 200 } else { 0 }, 0, 0, 255]);
        }
        assert_eq!(crop_scale_rgba(&a, 10, 0, 10, 1, 10, 1).get(0, 0)[0], 200);
        assert_eq!(crop_scale_rgba(&a, 0, 0, 10, 1, 10, 1).get(9, 0)[0], 0);
    }

    #[test]
    fn 等尺寸是逐像素搬运() {
        // 内核里 orig = crop_scale(base 裁切区 → 同尺寸) 走的是这条，必须严格等于原像素
        let mut a = Rgba::new(9, 7);
        for y in 0..7 {
            for x in 0..9 {
                a.set(x, y, [(x * 29) as u8, (y * 37) as u8, 7, 255]);
            }
        }
        let s = crop_scale_rgba(&a, 2, 1, 5, 4, 5, 4);
        for y in 0..4 {
            for x in 0..5 {
                assert_eq!(s.get(x, y), a.get(2 + x, 1 + y));
            }
        }
    }

    #[test]
    fn 放大不改变平坦区数值() {
        let mut a = Rgba::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                a.set(x, y, [120, 60, 200, 255]);
            }
        }
        let up = resize_rgba(&a, 32, 32);
        assert_eq!(up.get(17, 19), [120, 60, 200, 255]);
    }

    #[test]
    fn 缩小对透明像素按预乘处理() {
        // 一半不透明红、一半完全透明：预乘平均后 R 仍是 255，alpha 折半
        let mut a = Rgba::new(2, 1);
        a.set(0, 0, [255, 0, 0, 255]);
        a.set(1, 0, [0, 0, 0, 0]);
        let s = resize_rgba(&a, 1, 1);
        assert_eq!(s.get(0, 0)[0], 255);
        assert!((s.get(0, 0)[3] as i32 - 128).abs() <= 1, "alpha {:?}", s.get(0, 0));
    }

    /// 金字塔的 half/grow 走卷积重采样：边缘必须归一，否则整幅图的四周会被拉暗，
    /// 贴回后画布边缘出现一圈不该有的暗边（和 box blur 零填充是同一类坑）
    #[test]
    fn 缩放常量图不在边缘拉低() {
        let mut a = Rgba::new(41, 41);
        for v in a.px.iter_mut() {
            *v = 200;
        }
        for (dw, dh) in [(20, 20), (21, 21), (61, 61), (17, 41)] {
            let b = resize_rgba(&a, dw, dh);
            let corners = [0, (dw - 1) * 4, (b.h - 1) * b.w * 4, (b.w * b.h - 1) * 4];
            for i in corners {
                assert!(b.px[i].abs_diff(200) <= 1, "{dw}x{dh} 角上读到 {}", b.px[i]);
            }
            assert!(b.px[(b.h - 1) * b.w * 4 + (b.w - 1) * 4 + 3].abs_diff(200) <= 1, "alpha 边缘掉了");
        }
    }
}
