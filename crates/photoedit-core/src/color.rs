//! 调色段：十根滑杆 + 内置预设。
//!
//! 分工是"**能在表里做完的绝不在像素里做**"：曝光走线性域增益、对比走编码域绕中灰缩放，
//! 两者合成一张 256×3 的表，每图像建一次；剩下的算子都要看亮度，只能在像素里算。
//! 高光/阴影的提升量按亮度权重给，色温暖/冷是 R/B 通道增益，清晰度与锐化是高屏回填。

use crate::ops::Color;
use crate::px::{luma, q8, srgb_to_linear, linear_to_srgb, Plane};
use stitch_core::{par::par_chunks_mut, Rgba};

/// 曝光满档 = 1.5 EV
pub const EXPOSURE_EV: f32 = 1.5;
/// 对比满档 = 斜率 1.9，负满档收到 0.1（几乎压成一片灰）
pub const CONTRAST_GAIN: f32 = 0.9;
/// 中灰锚点（编码域）
const ANCHOR: f32 = 127.5;
/// 高光/阴影满档的最大位移，编码域 0..255 上的像素值
pub const TONE_PUSH: f32 = 70.0;
/// 色温满档的 R/B 增益量
pub const TEMP_GAIN: f32 = 0.12;
/// 色调满档的 G 增益量（正 = 偏品红，G 减、B 补）
pub const TINT_GAIN: f32 = 0.10;
/// 饱和度满档 = 0..2 倍
pub const SAT_GAIN: f32 = 1.0;
/// 清晰度满档的高频回填倍率
pub const CLARITY_GAIN: f32 = 0.8;
/// 锐化满档的高频回填倍率
pub const SHARP_GAIN: f32 = 1.0;

/// 曝光+对比的合成表。索引是入图字节，值是 0..255 的浮点（还没量化，量化在出口带抖动做）
pub struct Tone {
    r: [f32; 256],
    g: [f32; 256],
    b: [f32; 256],
}

impl Tone {
    /// `exposure`/`contrast` 都是归一化 -1..1
    pub fn build(exposure: f32, contrast: f32) -> Tone {
        let gain = 2f32.powf(exposure * EXPOSURE_EV);
        // 正端 1→1.9 斜率，负端 1→0.1：两端都不越过"全灰"和"全平"这两个退化点
        let k = 1.0 + CONTRAST_GAIN * contrast;
        let mut t = Tone { r: [0.0; 256], g: [0.0; 256], b: [0.0; 256] };
        let identity = gain == 1.0 && k == 1.0;
        for v in 0..=255usize {
            if identity {
                // 恒等档直接写整数：往返的浮点残差配上抖动偏置会偶尔少 1，
                // 而"没动滑杆就不该动一个字节"是硬要求
                t.r[v] = v as f32;
                t.g[v] = v as f32;
                t.b[v] = v as f32;
                continue;
            }
            let lin = srgb_to_linear(v as f32 / 255.0) * gain;
            let enc = linear_to_srgb(lin) * 255.0;
            let out = (enc - ANCHOR) * k + ANCHOR;
            t.r[v] = out;
            t.g[v] = out;
            t.b[v] = out;
        }
        t
    }

    #[inline]
    fn apply(&self, r: u8, g: u8, b: u8) -> [f32; 3] {
        [self.r[r as usize], self.g[g as usize], self.b[b as usize]]
    }
}

/// 平滑阶跃：高光权重在 0.5..1.0 之间升起，阴影权重在 0.0..0.5 之间落下
#[inline]
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 调色主循环。全零参数直接返回副本（一位都不动，恒等是硬要求）。
pub fn apply(img: &Rgba, c: &Color) -> Rgba {
    if c.is_empty() {
        return img.clone();
    }
    let tone = Tone::build(c.exposure.norm(), c.contrast.norm());
    let (hi, sh) = (c.highlights.norm(), c.shadows.norm());
    let (temp, tint) = (c.temp.norm(), c.tint.norm());
    let (sat, vib) = (c.saturation.norm(), c.vibrance.norm());
    let (clar, sharp) = (c.clarity.norm(), c.sharpen.norm());
    let (rg, bg) = (1.0 + TEMP_GAIN * temp, 1.0 - TEMP_GAIN * temp);
    let (gg, gb) = (1.0 - TINT_GAIN * tint, 1.0 + TINT_GAIN * tint * 0.5);

    // 清晰度要"大半径低量"，锐化要"小半径"——两张高屏平面，都只在各自滑杆非零时才算
    let detail_clar = if clar.abs() > 1e-4 { Some(luma_detail(img, clarity_radius(img.w, img.h), 3)) } else { None };
    let detail_sharp = if sharp.abs() > 1e-4 { Some(luma_detail(img, 1, 1)) } else { None };

    let mut out = Rgba::new(img.w, img.h);
    let rows = img.h.div_ceil(16).max(1);
    let src = &img.px;
    let w = img.w;
    par_chunks_mut(&mut out.px, rows * w * 4, |blk, band| {
        let y0 = band * rows;
        for (i, px) in blk.chunks_exact_mut(4).enumerate() {
            let g = y0 * w + i;
            let (x, y) = (g % w, g / w);
            let [mut r, mut gc, mut b] = tone.apply(src[g * 4], src[g * 4 + 1], src[g * 4 + 2]);
            let a = src[g * 4 + 3];

            // 高光/阴影：按亮度权重加同一个偏移，色相不被拆散
            if hi.abs() > 1e-4 || sh.abs() > 1e-4 {
                let yn = luma(r, gc, b) / 255.0;
                let mut d = 0.0f32;
                if hi.abs() > 1e-4 {
                    d += hi * TONE_PUSH * smoothstep(0.5, 1.0, yn) * (1.0 - yn);
                }
                if sh.abs() > 1e-4 {
                    d += sh * TONE_PUSH * smoothstep(0.5, 0.0, yn) * yn;
                }
                r += d;
                gc += d;
                b += d;
            }

            r *= rg;
            gc *= gg;
            b *= bg;
            b *= gb;

            // 饱和度：绕亮度拉伸；自然饱和度按"已经多饱和"打折，肤色不至于一下糊掉
            if sat.abs() > 1e-4 || vib.abs() > 1e-4 {
                let yv = luma(r, gc, b);
                let mx = r.max(gc).max(b);
                let mn = r.min(gc).min(b);
                let already = (mx - mn) / 255.0;
                let mut f = 1.0 + SAT_GAIN * sat;
                if vib.abs() > 1e-4 {
                    f += SAT_GAIN * vib * (1.0 - already);
                }
                r = yv + (r - yv) * f;
                gc = yv + (gc - yv) * f;
                b = yv + (b - yv) * f;
            }

            if let Some(p) = &detail_clar {
                let d = p.at(x, y) * CLARITY_GAIN * clar;
                r += d;
                gc += d;
                b += d;
            }
            if let Some(p) = &detail_sharp {
                let d = p.at(x, y) * SHARP_GAIN * sharp;
                r += d;
                gc += d;
                b += d;
            }

            px.copy_from_slice(&[q8(r, x, y), q8(gc, x, y), q8(b, x, y), a]);
        }
    });
    out
}

/// 亮度高屏：原亮度减掉模糊亮度。三次盒式摊过的叫"低频"，一次 3×3 摊过的叫"邻域"。
fn luma_detail(img: &Rgba, radius: usize, passes: usize) -> Plane {
    let mut y = Plane::new(img.w, img.h);
    for i in 0..img.w * img.h {
        let o = i * 4;
        y.v[i] = luma(img.px[o] as f32, img.px[o + 1] as f32, img.px[o + 2] as f32);
    }
    let low = y.box_blur_passes(radius, passes);
    for i in 0..img.w * img.h {
        y.v[i] -= low.v[i];
    }
    y
}

/// 清晰度的模糊半径随画幅走，这样同一滑杆在不同档位上是同一个"相对粗细"
fn clarity_radius(w: usize, h: usize) -> usize {
    (w.min(h) / 48).clamp(2, 32)
}

/// 内置预设 = 滑杆参数包，没有任何额外算子。第二列是给界面的**钥匙**（`ad.pn*`），不是名字本身。
pub const PRESETS: &[(&str, &str)] = &[
    ("clean", "ad.pnClean"),
    ("warm", "ad.pnWarm"),
    ("film", "ad.pnFilm"),
    ("mono", "ad.pnMono"),
    ("cool", "ad.pnCool"),
    ("soft", "ad.pnSoft"),
    ("crisp", "ad.pnCrisp"),
    ("teal", "ad.pnTeal"),
    ("faded", "ad.pnFaded"),
    ("night", "ad.pnNight"),
];

/// 取预设的滑杆值；未知名字返回 `None`，由调用方决定报错还是忽略
pub fn preset(name: &str) -> Option<Color> {
    let mut c = Color::default();
    match name {
        "clean" => {
            c.exposure.0 = 6;
            c.contrast.0 = 10;
            c.vibrance.0 = 12;
            c.clarity.0 = 6;
            c.sharpen.0 = 14;
        }
        "warm" => {
            c.exposure.0 = 8;
            c.temp.0 = 34;
            c.highlights.0 = -12;
            c.shadows.0 = 14;
            c.vibrance.0 = 10;
        }
        "film" => {
            c.contrast.0 = 22;
            c.highlights.0 = -18;
            c.shadows.0 = 20;
            c.temp.0 = 12;
            c.saturation.0 = -14;
            c.clarity.0 = 10;
        }
        "mono" => {
            c.contrast.0 = 40;
            c.saturation.0 = -100;
            c.clarity.0 = 24;
            c.sharpen.0 = 20;
        }
        "cool" => {
            c.exposure.0 = 4;
            c.temp.0 = -32;
            c.tint.0 = -6;
            c.vibrance.0 = 8;
        }
        "soft" => {
            c.exposure.0 = 10;
            c.contrast.0 = -12;
            c.highlights.0 = -10;
            c.shadows.0 = 18;
            c.temp.0 = 10;
            c.saturation.0 = -10;
            c.clarity.0 = -14;
        }
        "crisp" => {
            c.contrast.0 = 26;
            c.shadows.0 = -10;
            c.clarity.0 = 30;
            c.sharpen.0 = 26;
            c.vibrance.0 = 14;
        }
        "teal" => {
            c.temp.0 = -22;
            c.tint.0 = 10;
            c.contrast.0 = 18;
            c.shadows.0 = 12;
            c.saturation.0 = 12;
        }
        "faded" => {
            c.contrast.0 = -28;
            c.shadows.0 = 26;
            c.saturation.0 = -22;
            c.exposure.0 = 8;
        }
        "night" => {
            c.exposure.0 = -14;
            c.contrast.0 = 20;
            c.temp.0 = -26;
            c.tint.0 = -10;
            c.shadows.0 = -16;
            c.clarity.0 = 18;
        }
        _ => return None,
    }
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stitch_core::Rgba;

    fn flat(r: u8, g: u8, b: u8, w: usize, h: usize) -> Rgba {
        let mut img = Rgba::new(w, h);
        for i in 0..w * h {
            img.px[i * 4..i * 4 + 4].copy_from_slice(&[r, g, b, 255]);
        }
        img
    }

    /// 色卡：8×8 的灰阶 × 三原色混合，任何算子都在上面跑一遍它
    fn card() -> Rgba {
        let mut img = Rgba::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                let v = (x * 255 / 7) as u8;
                let c = match y {
                    0 => [v, v, v],
                    1 => [v, 0, 0],
                    2 => [0, v, 0],
                    3 => [0, 0, v],
                    4 => [v, v, 0],
                    5 => [0, v, v],
                    6 => [v, 0, v],
                    _ => [255, 255, 255],
                };
                img.set(x, y, [c[0], c[1], c[2], 255]);
            }
        }
        img
    }

    #[test]
    fn 全零参数逐位不变() {
        let img = card();
        assert_eq!(apply(&img, &Color::default()), img, "恒等必须一个字节都不动");
    }

    #[test]
    fn 曝光零就是恒等表() {
        let t = Tone::build(0.0, 0.0);
        for v in 0..=255 {
            assert!((t.r[v] - v as f32).abs() < 0.02, "{v} → {}", t.r[v]);
        }
    }

    #[test]
    fn 曝光满档等于一点五个ev() {
        let t = Tone::build(1.0, 0.0);
        // 手算：128 的线性值 0.215876 × 2^1.5 = 0.610736，回到编码域 ≈ 205.03
        assert!((t.r[128] - 205.03).abs() < 0.5, "{}", t.r[128]);
        let t = Tone::build(-1.0, 0.0);
        assert!((t.r[128] - 78.08).abs() < 0.5, "{}", t.r[128]);
        // 增益是对数的：+1.5EV 与 −1.5EV 互为倒数关系，黑永远还是黑
        let t = Tone::build(1.0, 0.0);
        assert_eq!(t.r[0], 0.0);
    }

    #[test]
    fn 对比锚在中灰不动() {
        let img = flat(128, 128, 128, 4, 4);
        let mut c = Color::default();
        c.contrast.0 = 100;
        let r = apply(&img, &c);
        assert!((r.get(1, 1)[0] as i32 - 128).abs() <= 1, "中灰必须几乎不动：{}", r.get(1, 1)[0]);
        // 亮的更亮、暗的更暗
        assert!(apply(&flat(200, 200, 200, 4, 4), &c).get(0, 0)[0] > 200);
        let mut d = Color::default();
        d.contrast.0 = -80;
        assert!(apply(&flat(200, 200, 200, 4, 4), &d).get(0, 0)[0] < 200, "负端要压平");
    }

    #[test]
    fn 高光只抬亮部阴影只抬暗部() {
        let mut hi = Color::default();
        hi.highlights.0 = 100;
        let a = apply(&flat(20, 20, 20, 4, 4), &hi).get(0, 0)[0];
        let b = apply(&flat(230, 230, 230, 4, 4), &hi).get(0, 0)[0];
        assert_eq!(a, 20, "暗部不该被高光滑杆动到");
        assert!(b > 230, "亮部要抬起来：{b}");

        let mut sh = Color::default();
        sh.shadows.0 = 100;
        let c = apply(&flat(230, 230, 230, 4, 4), &sh).get(0, 0)[0];
        let d = apply(&flat(30, 30, 30, 4, 4), &sh).get(0, 0)[0];
        assert_eq!(c, 230, "亮部不该被阴影滑杆动到");
        assert!(d > 30, "暗部要抬起来：{d}");
    }

    #[test]
    fn 色温是rb对立色调是gm对立() {
        let mut t = Color::default();
        t.temp.0 = 100;
        let r = apply(&flat(128, 128, 128, 4, 4), &t).get(0, 0);
        assert!(r[0] > 128 && r[2] < 128, "暖端要 R 升 B 降：{r:?}");
        assert_eq!(r[1], 128, "色温不该动 G");
        let mut ti = Color::default();
        ti.tint.0 = 100;
        let r = apply(&flat(128, 128, 128, 4, 4), &ti).get(0, 0);
        assert!(r[1] < 128 && r[2] > 128, "品红端要 G 降 B 补：{r:?}");
    }

    #[test]
    fn 饱和度负满档就是灰度() {
        let mut c = Color::default();
        c.saturation.0 = -100;
        let img = flat(255, 0, 0, 4, 4);
        let r = apply(&img, &c).get(0, 0);
        // BT.601 的红色亮度
        assert!(r[0] == r[1] && r[1] == r[2], "三通道该相等：{r:?}");
        assert!((r[0] as f32 - 0.299 * 255.0).abs() <= 1.0, "{r:?}");
    }

    #[test]
    fn 自然饱和度偏爱不饱和的那头() {
        let mut c = Color::default();
        c.vibrance.0 = 100;
        let dull = [128u8, 130, 132, 255];
        let vivid = [255u8, 20, 20, 255];
        let span = |p: [u8; 4]| p[0].max(p[1]).max(p[2]) as i32 - p[0].min(p[1]).min(p[2]) as i32;
        let g = apply(&flat(dull[0], dull[1], dull[2], 4, 4), &c).get(0, 0);
        let v = apply(&flat(vivid[0], vivid[1], vivid[2], 4, 4), &c).get(0, 0);
        // 相对增幅：近灰的至少翻倍，已经很艳的最多再涨一成
        assert!(span(g) >= span(dull) * 2, "灰的那头没被拉开：{dull:?} → {g:?}");
        assert!(span(v) <= (span(vivid) as f32 * 1.12) as i32, "艳的那头加爆了的倾向：{vivid:?} → {v:?}");
        assert!(span(v) >= span(vivid), "完全没作用也不对：{vivid:?} → {v:?}");
    }

    #[test]
    fn 清晰度与锐化在平色上不动() {
        // 平面色没有高频，任何"细节回填"都必须保持原样——这是它们最容易作弊的地方
        let img = flat(120, 90, 200, 64, 64);
        let mut c = Color::default();
        c.clarity.0 = 100;
        let r = apply(&img, &c);
        // 平色区没有高频，回填量本该是 0；浮点漂移叠上抖动出口可能差出一档，容一档
        for y in 4..60 {
            for x in 4..60 {
                let p = r.get(x, y);
                assert!((p[0] as i32 - 120).abs() <= 1 && (p[1] as i32 - 90).abs() <= 1 && (p[2] as i32 - 200).abs() <= 1, "平色区被清晰度改过头：({x},{y}) → {p:?}");
            }
        }
        let mut c = Color::default();
        c.sharpen.0 = 100;
        assert_eq!(apply(&img, &c), img, "平色区被锐化改了");
    }

    #[test]
    fn 清晰度把边缘加得比原图更陡() {
        let mut img = Rgba::new(64, 8);
        for y in 0..8 {
            for x in 0..64 {
                let v = if x < 32 { 60u8 } else { 190u8 };
                img.set(x, y, [v, v, v, 255]);
            }
        }
        let mut c = Color::default();
        c.clarity.0 = 100;
        let r = apply(&img, &c);
        assert!(r.get(30, 4)[0] < 60, "暗侧边缘要更暗：{}", r.get(30, 4)[0]);
        assert!(r.get(33, 4)[0] > 190, "亮侧边缘要更亮：{}", r.get(33, 4)[0]);
        // 远离边缘的地方还是原来的台阶（容一档，同上）
        assert!((r.get(4, 4)[0] as i32 - 60).abs() <= 1, "{}", r.get(4, 4)[0]);
        assert!((r.get(60, 4)[0] as i32 - 190).abs() <= 1, "{}", r.get(60, 4)[0]);
    }

    #[test]
    fn alpha通道一路不动() {
        let mut img = Rgba::new(4, 4);
        for i in 0..16 {
            img.px[i * 4..i * 4 + 4].copy_from_slice(&[i as u8 * 8, 40, 200, (i * 7) as u8]);
        }
        let mut c = Color::default();
        c.exposure.0 = 60;
        c.contrast.0 = -40;
        c.clarity.0 = 80;
        c.sharpen.0 = 50;
        let r = apply(&img, &c);
        for i in 0..16 {
            assert_eq!(r.px[i * 4 + 3], img.px[i * 4 + 3], "透明度被调色改掉了");
        }
    }

    #[test]
    fn 预设都在量程内且不是恒等() {
        for (name, _) in PRESETS {
            let c = preset(name).unwrap_or_else(|| panic!("预设 {name} 没定义"));
            assert!(!c.is_empty(), "预设 {name} 全是零，等于没预设");
            for (label, s) in [("exposure", c.exposure), ("contrast", c.contrast), ("highlights", c.highlights), ("shadows", c.shadows), ("temp", c.temp), ("tint", c.tint), ("saturation", c.saturation), ("vibrance", c.vibrance), ("clarity", c.clarity), ("sharpen", c.sharpen)] {
                assert!((-100..=100).contains(&s.0), "{name}.{label} 越界：{}", s.0);
            }
        }
        assert!(preset("不存在的名字").is_none());
        assert!(preset("").is_none());
    }

    #[test]
    fn 预设不改动透明度也不制造新像素() {
        let img = card();
        for (name, _) in PRESETS {
            let c = preset(name).unwrap();
            let r = apply(&img, &c);
            assert_eq!((r.w, r.h), (img.w, img.h));
            for i in 0..img.w * img.h {
                assert_eq!(r.px[i * 4 + 3], img.px[i * 4 + 3], "{name} 改了 alpha");
            }
            // 死黑：正端对比不该抬它；负端对比会把它提起来，那正是"褪色感"的来源，
            // 但提升量必须有限，不能把黑提成灰白
            let black = apply(&flat(0, 0, 0, 4, 4), &c).get(0, 0);
            let limit = if c.contrast.0 < 0 { 45 } else { 2 };
            assert!(black[0] <= limit && black[1] <= limit && black[2] <= limit, "{name} 把死黑提成了 {black:?}");
        }
    }

    #[test]
    fn 正对比预设不动死黑() {
        let c = preset("mono").unwrap();
        assert_eq!(apply(&flat(0, 0, 0, 4, 4), &c).get(0, 0), [0, 0, 0, 255]);
    }
}
