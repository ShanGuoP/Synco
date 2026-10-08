//! 几何段：翻转、旋转、裁切。链上的位置是**变形之前**。
//!
//! 顺序是 翻转 → 旋转 → 裁切：裁切框落在"旋转之后的那个画面"里，而不是源图里。
//! 这样前端的裁切 overlay 画在服务端刚渲染完那张档上，用户拖的框和他看到的画面
//! 是同一个坐标系——反过来排（先裁后转）就得让客户端自己算旋转后的外接框，
//! 两套取整迟早对不上。

use crate::ops::{Fill, Geometry};
use crate::px::{corner_avg, sample, Pad};
use stitch_core::{par::par_chunks_mut, Alpha, Rgba};

/// 按 `Geometry` 走一遍：翻转 → 旋转（90° 步进无损，余下小角度双线性）→ 裁切
pub fn apply(img: &Rgba, g: &Geometry) -> Rgba {
    let mut cur = img.clone();
    if g.flip_h || g.flip_v {
        flip_in_place(&mut cur, g.flip_h, g.flip_v);
    }
    let quad = (g.rotate_deg / 90.0).round() as i32;
    let fine = g.rotate_deg - quad as f32 * 90.0;
    let quad = quad.rem_euclid(4) as usize;
    for _ in 0..quad {
        cur = rot90_cw(&cur);
    }
    if fine.abs() > 1e-4 {
        cur = rot_fine(&cur, fine, g.fill);
    }
    if let Some(rect) = g.crop {
        cur = crop_norm(&cur, rect);
    }
    cur
}

/// 把单通道遮罩也过一遍几何段：笔迹活在 alpha 里，RGB 留 0，转完再取回 alpha。
///
/// 为什么要有这一步：库里的笔迹**永远画在源图坐标系**（带着裁切/旋转时编辑器锁住画笔），
/// 而下游吃的都是几何段之后那张。中间没人转，`k = 图宽 / 遮罩宽` 就会把"左上角"按横宽的比例
/// 硬贴到转过的竖幅上——发出去的重绘区在错的位置，且一路落到 done，一声不响。
/// 补边那几格跟着 `g.fill` 的同一套规则走：那一块既然进了画面，遮罩就不该当它不存在。
pub fn apply_alpha(a: &Alpha, g: &Geometry) -> Alpha {
    if !needs_pass(g) {
        return a.clone();
    }
    let mut img = Rgba::new(a.w, a.h);
    for (i, v) in a.v.iter().enumerate() {
        img.px[i * 4 + 3] = *v;
    }
    apply(&img, g).alpha()
}

/// 几何段这一趟到底动没动坐标系：全默认时上游原样退回，一个字节都不必重算
pub fn needs_pass(g: &Geometry) -> bool {
    g.flip_h || g.flip_v || g.crop.is_some() || g.rotate_deg.abs() > 1e-4
}

/// 归一化框换成像素框：与 [`crop_norm`] 同一套取整，两处共用才不会算出两个尺寸。
fn crop_rect(w: usize, h: usize, rect: [f32; 4]) -> (usize, usize, usize, usize) {
    let [x, y, cw, ch] = rect;
    let x0 = (x.clamp(0.0, 1.0) * w as f32).round() as usize;
    let y0 = (y.clamp(0.0, 1.0) * h as f32).round() as usize;
    let x1 = (((x + cw).clamp(0.0, 1.0)) * w as f32).round() as usize;
    let y1 = (((y + ch).clamp(0.0, 1.0)) * h as f32).round() as usize;
    let x1 = x1.clamp(x0 + 1, w);
    let y1 = y1.clamp(y0 + 1, h);
    (x0, y0, x1 - x0, y1 - y0)
}

/// 走完几何段之后这张图会是多大。**与 apply 同一套取整、同一套顺序**（翻转不改尺寸 →
/// 旋转 → 裁切）：服务端要在真正渲染之前就知道新图的宽高（另存为新图时它直接进库），
/// 两边各算一份早晚对不上。
pub fn out_size(w: usize, h: usize, g: &Geometry) -> (usize, usize) {
    let (mut cw, mut ch) = (w, h);
    let quad = (g.rotate_deg / 90.0).round() as i32;
    let fine = g.rotate_deg - quad as f32 * 90.0;
    for _ in 0..quad.rem_euclid(4) {
        std::mem::swap(&mut cw, &mut ch);
    }
    if fine.abs() > 1e-4 {
        let th = fine.to_radians();
        let (s, c) = (th.sin().abs(), th.cos().abs());
        let (a, b) = (cw, ch);
        cw = ((a as f32 * c + b as f32 * s).ceil()).max(1.0) as usize;
        ch = ((a as f32 * s + b as f32 * c).ceil()).max(1.0) as usize;
    }
    if let Some(rect) = g.crop {
        if !whole_rect(&rect) {
            let (_, _, a, b) = crop_rect(cw, ch, rect);
            cw = a;
            ch = b;
        }
    }
    (cw.max(1), ch.max(1))
}

/// 整框（等价于不裁）：`crop_norm` 与 `out_size` 都靠它走 fast path
fn whole_rect(rect: &[f32; 4]) -> bool {
    rect[0] <= 0.0 && rect[1] <= 0.0 && rect[2] >= 1.0 - 1e-6 && rect[3] >= 1.0 - 1e-6
}

/// 归一化框 → 子图。整框不越界且不小于 1px 才动手；退化框按恒等处理，
/// 免得一个 `[0,0,0,0]` 把成图变成空图。
pub fn crop_norm(img: &Rgba, rect: [f32; 4]) -> Rgba {
    if whole_rect(&rect) {
        return img.clone();
    }
    let (x0, y0, w0, h0) = crop_rect(img.w, img.h, rect);
    crop(img, x0, y0, w0, h0)
}

/// 像素级子图
pub fn crop(img: &Rgba, x: usize, y: usize, w: usize, h: usize) -> Rgba {
    let x = x.min(img.w);
    let y = y.min(img.h);
    let w = w.max(1).min(img.w - x);
    let h = h.max(1).min(img.h - y);
    let mut out = Rgba::new(w, h);
    for row in 0..h {
        let s = ((y + row) * img.w + x) * 4;
        out.px[row * w * 4..(row + 1) * w * 4].copy_from_slice(&img.px[s..s + w * 4]);
    }
    out
}

fn flip_in_place(img: &mut Rgba, h: bool, v: bool) {
    if h {
        for y in 0..img.h {
            let row = &mut img.px[y * img.w * 4..(y + 1) * img.w * 4];
            for k in 0..img.w / 2 {
                let a = k * 4;
                let b = (img.w - 1 - k) * 4;
                for c in 0..4 {
                    row.swap(a + c, b + c);
                }
            }
        }
    }
    if v {
        let n = img.w * 4;
        for y in 0..img.h / 2 {
            let a = y * n;
            let b = (img.h - 1 - y) * n;
            for k in 0..n {
                img.px.swap(a + k, b + k);
            }
        }
    }
}

/// 顺时针 90°：新图 (x,y) 取源图 `(y, h-1-x)`，无插值
pub fn rot90_cw(img: &Rgba) -> Rgba {
    let mut out = Rgba::new(img.h, img.w);
    for y in 0..img.h {
        for x in 0..img.w {
            let s = (y * img.w + x) * 4;
            let dx = img.h - 1 - y;
            let dy = x;
            let d = (dy * out.w + dx) * 4;
            out.px[d..d + 4].copy_from_slice(&img.px[s..s + 4]);
        }
    }
    out
}

/// 小角度矫正：输出取旋转后的外接框，越界按补边策略取
fn rot_fine(img: &Rgba, deg: f32, fill: Fill) -> Rgba {
    let th = deg.to_radians();
    let (s, c) = (th.sin().abs(), th.cos().abs());
    let nw = ((img.w as f32 * c + img.h as f32 * s).ceil()).max(1.0) as usize;
    let nh = ((img.w as f32 * s + img.h as f32 * c).ceil()).max(1.0) as usize;
    let pad = match fill {
        Fill::Edge => Pad::Edge,
        Fill::Avg => Pad::Fixed(corner_avg(&img.px, img.w, img.h)),
    };
    // 屏幕坐标里 +deg 是顺时针，反变换就是同一角度逆时针
    let (cs, sn) = (th.cos(), th.sin());
    let (scx, scy) = ((img.w as f32 - 1.0) * 0.5, (img.h as f32 - 1.0) * 0.5);
    let (dcx, dcy) = ((nw as f32 - 1.0) * 0.5, (nh as f32 - 1.0) * 0.5);
    let rows = band_rows(nh);
    let mut out = Rgba::new(nw, nh);
    let src = &img.px;
    let (sw, sh) = (img.w, img.h);
    par_chunks_mut(&mut out.px, rows * nw * 4, |blk, band| {
        // blk 是连续 rows 行，块内第 i 个像素的全图行号是 band*rows + i/nw
        let y0 = band * rows;
        for (i, px) in blk.chunks_exact_mut(4).enumerate() {
            let g = y0 * nw + i;
            let dx = (g % nw) as f32 - dcx;
            let dy = (g / nw) as f32 - dcy;
            let v = sample(src, sw, sh, cs * dx + sn * dy + scx, -sn * dx + cs * dy + scy, pad);
            px.copy_from_slice(&v.map(|f| f.clamp(0.0, 255.0) as u8));
        }
    });
    out
}

/// 并行分块的最少行数：块太碎会让线程数超过行数
fn band_rows(h: usize) -> usize {
    h.div_ceil(16).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stitch_core::Rgba;

    /// 非对称测试图：每个像素的 R 通道编码它的 x，G 编码 y，一眼能看出映射对不对
    fn grid(w: usize, h: usize) -> Rgba {
        let mut img = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set(x, y, [x as u8, y as u8, 0, 255]);
            }
        }
        img
    }

    fn g(crop: Option<[f32; 4]>, rot: f32, fh: bool, fv: bool) -> Geometry {
        Geometry { crop, rotate_deg: rot, flip_h: fh, flip_v: fv, fill: Fill::Edge }
    }

    #[test]
    fn 默认几何不动一个像素() {
        let img = grid(7, 5);
        assert_eq!(apply(&img, &Geometry::default()), img);
    }

    #[test]
    fn 四次步进旋转回到原图() {
        let img = grid(6, 4);
        let r = apply(&img, &g(None, 90.0, false, false));
        assert_eq!((r.w, r.h), (4, 6), "转 90° 应该换边长");
        let r = apply(&r, &g(None, 90.0, false, false));
        let r = apply(&r, &g(None, 90.0, false, false));
        let r = apply(&r, &g(None, 90.0, false, false));
        assert_eq!(r, img, "四次 90° 必须逐像素恒等");
    }

    #[test]
    fn 顺时针九十度的映射点() {
        let img = grid(2, 3);
        let r = rot90_cw(&img);
        assert_eq!((r.w, r.h), (3, 2));
        // 顺时针：源图左下角 → 新图左上角
        assert_eq!(r.get(0, 0), img.get(0, 2));
        assert_eq!(r.get(2, 0), img.get(0, 0));
        assert_eq!(r.get(0, 1), img.get(1, 2));
    }

    #[test]
    fn 翻转是对合() {
        let img = grid(5, 3);
        let fh = apply(&img, &g(None, 0.0, true, false));
        assert_eq!(fh.get(0, 0), img.get(4, 0));
        assert_eq!(apply(&fh, &g(None, 0.0, true, false)), img);
        let fv = apply(&img, &g(None, 0.0, false, true));
        assert_eq!(fv.get(0, 0), img.get(0, 2));
        assert_eq!(apply(&fv, &g(None, 0.0, false, true)), img);
    }

    #[test]
    fn 裁切框按归一化取子图() {
        let img = grid(8, 8);
        let r = apply(&img, &g(Some([0.25, 0.25, 0.5, 0.5]), 0.0, false, false));
        assert_eq!((r.w, r.h), (4, 4));
        assert_eq!(r.get(0, 0), [2, 2, 0, 255]);
        assert_eq!(r.get(3, 3), [5, 5, 0, 255]);
    }

    #[test]
    fn 退化框不落空图() {
        let img = grid(6, 6);
        let r = apply(&img, &g(Some([0.5, 0.5, 0.0, 0.0]), 0.0, false, false));
        assert_eq!((r.w, r.h), (1, 1), "零宽高至少留一颗像素");
        let r = apply(&img, &g(Some([0.9, 0.9, 0.5, 0.5]), 0.0, false, false));
        assert_eq!((r.w, r.h), (1, 1), "右下角越界夹回边内");
    }

    #[test]
    fn 微调旋转外接框变大且中心不变() {
        let img = grid(40, 40);
        let r = apply(&img, &g(None, 5.0, false, false));
        assert!(r.w > 40 && r.h > 40, "转 5° 画布该变大：{}×{}", r.w, r.h);
        // 中心区域仍是中心那个像素（旋转围绕图像中心）：外接框尺寸是奇偶不定的，
        // 所以取最近的那个点，允许插值带来的一档误差
        let near = r.get(r.w / 2, r.h / 2);
        for c in 0..3 {
            assert!((near[c] as i32 - img.get(20, 20)[c] as i32).abs() <= 1, "中心漂了：{near:?}");
        }
        // 角度为 0 时小角度分支不该被触发（apply 里按 1e-4 跳过）
        assert_eq!(apply(&img, &g(None, 0.0, false, false)), img);
    }

    #[test]
    fn 小角度旋转两种补边各得其所() {
        let mut img = Rgba::new(21, 21);
        for i in 0..21 * 21 {
            img.px[i * 4..i * 4 + 4].copy_from_slice(&[10, 20, 30, 255]);
        }
        img.set(0, 0, [200, 0, 0, 255]);
        img.set(20, 0, [0, 200, 0, 255]);
        img.set(0, 20, [0, 0, 200, 255]);
        img.set(20, 20, [200, 200, 200, 255]);
        let mut gg = g(None, 12.0, false, false);
        let edge = apply(&img, &gg);
        gg.fill = Fill::Avg;
        let avg = apply(&img, &gg);
        assert!(edge.w > img.w && avg.w > img.w);
        // 角上那一块：边缘延伸拿到的是最外圈的颜色，均值补边拿到的是四角均值
        assert!(avg.get(0, 0)[0] > 0 && avg.get(0, 0)[0] < 255);
        // 两种补边都不该出现全黑
        assert!(edge.get(0, 0) != [0, 0, 0, 0]);
    }

    #[test]
    fn 预告尺寸与实际产出逐档对得上() {
        // 服务端在渲染之前就要知道新图的宽高（另存为新图直接进库），
        // 这两条路共用取整逻辑，所以拿几个边界形状钉住它们不会分叉
        for (w, h) in [(97usize, 53usize), (64, 64), (200, 7)] {
            for g in [
                g(Some([0.0, 0.0, 1.0, 1.0]), 0.0, false, false),
                g(Some([0.13, 0.21, 0.47, 0.33]), 0.0, false, false),
                g(None, 90.0, false, false),
                g(Some([0.0, 0.1, 0.5, 0.8]), 270.0, true, false),
                g(None, -7.5, false, true),
                g(Some([0.25, 0.25, 0.5, 0.5]), 180.0, false, false),
            ] {
                let img = grid(w, h);
                let got = apply(&img, &g);
                let want = out_size(w, h, &g);
                assert_eq!((got.w, got.h), want, "{w}×{h} 在 {g:?} 下对不上");
            }
        }
    }

    #[test]
    fn 一百八十度等价于两次九十() {
        let img = grid(9, 6);
        let a = apply(&img, &g(None, 180.0, false, false));
        let b = apply(&apply(&img, &g(None, 90.0, false, false)), &g(None, 90.0, false, false));
        assert_eq!(a, b);
        assert_eq!(a.get(0, 0), img.get(8, 5));
    }

    #[test]
    fn 遮罩跟着几何段一起换坐标系() {
        // 一张 8×4 的遮罩，笔迹在**左上角**（源图域）
        let mut a = Alpha::new(8, 4);
        for y in 0..2 {
            for x in 0..4 {
                a.v[y * 8 + x] = 255;
            }
        }
        // 没几何段就该原样退回，一次都不重算
        assert_eq!(apply_alpha(&a, &g(None, 0.0, false, false)), a, "默认几何也把遮罩动过了");
        // 右转 90°：8×4 → 4×8，源图左上角那一坨落到**右上角**（src(x,y) → dst(h-1-y, x)）
        let r = apply_alpha(&a, &g(None, 90.0, false, false));
        assert_eq!((r.w, r.h), (4, 8), "遮罩没跟着换边长，下游那个比例就算定了");
        assert_eq!(&r.v[0..4], &[0, 0, 255, 255], "第一行该是右侧两格有笔迹：{:?}", &r.v[0..4]);
        assert!(r.v[4 * r.w..].iter().all(|&v| v == 0), "下面四行不该有笔迹");
        let ink: usize = r.v.iter().filter(|&&v| v > 0).count();
        assert_eq!(ink, 8, "笔迹面积在转的时候被吃掉了：{ink} 格");
    }
}
