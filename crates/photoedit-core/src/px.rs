//! 像素层的公共小工具：量化抖动、双线性取样、单通道平面。
//! 全链按方案走 f32，只在出口一次性量化到 u8，并且带抖动——
//! 滑杆两端的高光/阴影渐变最容易出 banding，抖动是把它抹平的那一步。

/// 4×4 Bayer 矩阵（值 0..15），量化前按像素坐标加 ±0.5 量级的偏置
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// 抖动偏置：把量化误差打散成高频噪声，而不是停在同一个台阶上
#[inline]
pub(crate) fn dither(x: usize, y: usize) -> f32 {
    (BAYER[y & 3][x & 3] as f32) * (1.0 / 16.0) - 0.5
}

/// f32 (0..255) → u8，带抖动与夹逼。
/// 用"四舍五入 + 偏置"而不是直接截断：偏置最大只有半个量化台阶，
/// 于是整数输入原样出去（恒等算子必须逐位不变），小数输入按相位分配到上下两档。
#[inline]
pub(crate) fn q8(v: f32, x: usize, y: usize) -> u8 {
    (v + dither(x, y)).clamp(0.0, 255.0).round() as u8
}

/// 不带抖动的量化（蒙版、平面这类"本来就是整数"的场合）
#[inline]
pub(crate) fn q8_flat(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}

#[inline]
pub(crate) fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
pub(crate) fn linear_to_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// BT.601 亮度，作用在 gamma 域（与界面/预览一致，不做线性化）
#[inline]
pub(crate) fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.299 * r + 0.587 * g + 0.114 * b
}

/// sRGB → YCbCr 的 Y（BT.601，值域 16..235 的电视域约定不适用，这里用全量程）
#[inline]
pub(crate) fn to_ycbcr(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let y = luma(r, g, b);
    let cb = -0.168736 * r - 0.331264 * g + 0.5 * b;
    let cr = 0.5 * r - 0.418688 * g - 0.081312 * b;
    (y, cb, cr)
}

/// 越界坐标怎么取：`Edge` = 钳到最外圈像素（边缘延伸），`Fixed` = 用给定颜色补
#[derive(Clone, Copy, Debug)]
pub(crate) enum Pad {
    Edge,
    Fixed([f32; 4]),
}

/// 双线性取样。坐标是"像素中心"约定：`(0,0)` 是左上像素的中心。
pub(crate) fn sample(img: &[u8], w: usize, h: usize, fx: f32, fy: f32, pad: Pad) -> [f32; 4] {
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;
    let at = |ix: i64, iy: i64| -> Option<[f32; 4]> {
        if ix < 0 || iy < 0 || ix >= w as i64 || iy >= h as i64 {
            return None;
        }
        let i = ((iy as usize * w + ix as usize) * 4) as usize;
        Some([img[i] as f32, img[i + 1] as f32, img[i + 2] as f32, img[i + 3] as f32])
    };
    let (x0i, y0i) = (x0 as i64, y0 as i64);
    match pad {
        Pad::Fixed(c) => {
            let g = |ix: i64, iy: i64| match at(ix, iy) {
                Some(v) => v,
                None => c,
            };
            // 双线性：先横向再纵向（展开写比嵌套迭代器好读，也不会有人误读成重心插值）
            let a = g(x0i, y0i);
            let b = g(x0i + 1, y0i);
            let c2 = g(x0i, y0i + 1);
            let d = g(x0i + 1, y0i + 1);
            let top = mix4(&a, &b, tx);
            let bot = mix4(&c2, &d, tx);
            mix4(&top, &bot, ty)
        }
        Pad::Edge => {
            // 边缘延伸：坐标钳位后再插值，四角自然不会缺色
            let cx = |i: i64| i.clamp(0, w as i64 - 1) as usize;
            let cy = |i: i64| i.clamp(0, h as i64 - 1) as usize;
            let g = |ix: i64, iy: i64| {
                let i = (cy(iy) * w + cx(ix)) * 4;
                [img[i] as f32, img[i + 1] as f32, img[i + 2] as f32, img[i + 3] as f32]
            };
            let a = g(x0i, y0i);
            let b = g(x0i + 1, y0i);
            let c2 = g(x0i, y0i + 1);
            let d = g(x0i + 1, y0i + 1);
            let top = mix4(&a, &b, tx);
            let bot = mix4(&c2, &d, tx);
            mix4(&top, &bot, ty)
        }
    }
}

#[inline]
fn mix4(a: &[f32; 4], b: &[f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

/// 四角均值补色：微调旋转后"留白"用的颜色，取法固定为四角平均
pub(crate) fn corner_avg(img: &[u8], w: usize, h: usize) -> [f32; 4] {
    let pts = [(0usize, 0usize), (w - 1, 0), (0, h - 1), (w - 1, h - 1)];
    let mut acc = [0.0f32; 4];
    for (x, y) in pts {
        let i = (y * w + x) * 4;
        for k in 0..4 {
            acc[k] += img[i + k] as f32;
        }
    }
    [acc[0] / 4.0, acc[1] / 4.0, acc[2] / 4.0, acc[3] / 4.0]
}

/// 单通道平面：磨皮与清晰度都只在这一层里算
pub(crate) struct Plane {
    pub w: usize,
    pub h: usize,
    pub v: Vec<f32>,
}

impl Plane {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, v: vec![0.0; w * h] }
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.v[y * self.w + x]
    }

    /// `passes` 次行+列盒式模糊。清晰度用 3 次摊出大半径低频，
    /// 锐化只要 1 次 3×3（多摊就把要保留的高频吃掉了）。成本与半径无关。
    pub fn box_blur_passes(&self, radius: usize, passes: usize) -> Plane {
        clone_plane(self).into_box_blur_passes(radius, passes)
    }

    /// 已拥有的工作平面直接复用为输出，不再先复制一份整幅 f32 缓冲。
    pub fn into_box_blur_passes(mut self, radius: usize, passes: usize) -> Plane {
        if radius == 0 || passes == 0 {
            return self;
        }
        let mut tmp = Plane::new(self.w, self.h);
        for _ in 0..passes {
            blur_rows(&self.v, self.w, self.h, radius, &mut tmp.v);
            blur_cols(&tmp.v, self.w, self.h, radius, &mut self.v);
        }
        self
    }
}

pub(crate) fn clone_plane(p: &Plane) -> Plane {
    Plane { w: p.w, h: p.h, v: p.v.clone() }
}

/// 行向盒式模糊：每行独立，滑动窗口 O(w)
fn blur_rows(src: &[f32], w: usize, h: usize, r: usize, dst: &mut [f32]) {
    let win = 2 * r + 1;
    for y in 0..h {
        let row = &src[y * w..y * w + w];
        let mut acc = 0.0f32;
        // 左端外补的是第 0 列（边缘钳位）
        for k in 0..win {
            acc += row[k.saturating_sub(r).min(w - 1)];
        }
        for x in 0..w {
            dst[y * w + x] = acc / win as f32;
            let add = row[(x + r + 1).min(w - 1)];
            let sub = row[x.saturating_sub(r)];
            acc += add - sub;
        }
    }
}

/// 列向盒式模糊：跨行不连续，按列滑窗同样 O(h)
fn blur_cols(src: &[f32], w: usize, h: usize, r: usize, dst: &mut [f32]) {
    let win = 2 * r + 1;
    for x in 0..w {
        let mut acc = 0.0f32;
        for k in 0..win {
            acc += src[k.saturating_sub(r).min(h - 1) * w + x];
        }
        for y in 0..h {
            dst[y * w + x] = acc / win as f32;
            let add = src[(y + r + 1).min(h - 1) * w + x];
            let sub = src[y.saturating_sub(r) * w + x];
            acc += add - sub;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_往返不丢值() {
        for i in 0..=255u8 {
            let back = linear_to_srgb(srgb_to_linear(i as f32 / 255.0)) * 255.0;
            assert!((back - i as f32).abs() < 0.51, "{i} → {back}");
        }
    }

    #[test]
    fn 抖动只是把误差打散() {
        // 抖动量必须小于一个量化台阶，否则等于改值
        for y in 0..8 {
            for x in 0..8 {
                let d = dither(x, y);
                assert!(d >= -0.5 && d < 0.5, "抖动偏置出界：{d}");
            }
        }
        assert_eq!(q8(0.0, 0, 0), 0);
        assert_eq!(q8(255.0, 3, 3), 255);
    }

    #[test]
    fn 恒等取样拿回原像素() {
        let px = vec![10u8, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255];
        for y in 0..2 {
            for x in 0..2 {
                let v = sample(&px, 2, 2, x as f32, y as f32, Pad::Edge);
                let i = (y * 2 + x) * 4;
                assert_eq!([v[0] as u8, v[1] as u8, v[2] as u8, v[3] as u8], [px[i], px[i + 1], px[i + 2], px[i + 3]]);
            }
        }
    }

    #[test]
    fn 中点取样是两个邻居的平均() {
        let px = vec![0u8, 0, 0, 255, 200, 200, 200, 255];
        let v = sample(&px, 2, 1, 0.5, 0.0, Pad::Edge);
        assert!((v[0] - 100.0).abs() < 1e-3, "{v:?}");
        // 越出去按边缘延伸：拿到的还是最外圈那个像素
        let e = sample(&px, 2, 1, 1.0, 0.0, Pad::Edge);
        assert!((e[0] - 200.0).abs() < 1e-3, "{e:?}");
        // 画面内那侧仍然取样到位：越过右边界的一格补 7，纵向一半处折中
        let f = sample(&px, 2, 1, 1.0, 0.5, Pad::Fixed([7.0; 4]));
        assert!((f[0] - 103.5).abs() < 1e-3, "{f:?}");
    }

    #[test]
    fn 盒式模糊压平噪声不压平台阶() {
        let mut p = Plane::new(64, 8);
        for y in 0..8 {
            for x in 0..64 {
                p.v[y * 64 + x] = if x < 32 { 10.0 } else { 200.0 };
            }
        }
        let b = p.box_blur_passes(4, 3);
        // 远离边界的地方仍然是两端值
        assert!((b.at(2, 4) - 10.0).abs() < 1e-3);
        assert!((b.at(61, 4) - 200.0).abs() < 1e-3);
        // 边界附近过渡带被摊开
        assert!(b.at(30, 4) > 10.0 && b.at(30, 4) < 200.0);
        // 常数平面模糊后还是常数
        let mut c = Plane::new(16, 16);
        for i in 0..256 {
            c.v[i] = 77.0;
        }
        let cb = c.box_blur_passes(3, 3);
        assert!(cb.v.iter().all(|v| (v - 77.0).abs() < 1e-3));
    }

    #[test]
    fn 亮度与色度权重和为一() {
        // 白与黑必须落在两端
        assert!((luma(255.0, 255.0, 255.0) - 255.0).abs() < 0.01);
        assert!(luma(0.0, 0.0, 0.0).abs() < 1e-6);
        // 纯色通道的权重就是它们的系数
        assert!((luma(255.0, 0.0, 0.0) - 0.299 * 255.0).abs() < 0.01);
    }
}
