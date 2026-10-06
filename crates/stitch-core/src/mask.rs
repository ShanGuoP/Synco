//! 蒙版运算：包围盒、外扩、贴回权重。全部只读 alpha。
//! 外扩在浏览器画布上是 12 向平移盖戳取 max，这里用 imageproc 的形态学膨胀（欧氏距离变换 + 阈值）——
//! 半径语义一致，边缘不逐像素相同（方案 §5 Phase 1 已认可这一点）。

use crate::buffer::{Alpha, Box2};
use crate::par::par_chunks_mut;
use crate::resize::to_u8;
use imageproc::distance_transform::Norm;
use imageproc::image::GrayImage;
use imageproc::morphology::dilate as ip_dilate;

/// 蒙版非空像素的包围盒（`alpha > thr`），返回闭开区间；无笔迹返回 None
pub fn ink_bbox(a: &Alpha, thr: u8) -> Option<Box2> {
    let mut x0 = usize::MAX;
    let mut y0 = usize::MAX;
    let mut x1 = -1isize;
    let mut y1 = -1isize;
    for y in 0..a.h {
        for x in 0..a.w {
            if a.v[y * a.w + x] > thr {
                if x < x0 {
                    x0 = x;
                }
                if x as isize > x1 {
                    x1 = x as isize;
                }
                if y < y0 {
                    y0 = y;
                }
                if y as isize > y1 {
                    y1 = y as isize;
                }
            }
        }
    }
    if x1 < 0 {
        return None;
    }
    Some(Box2 {
        x: x0,
        y: y0,
        w: (x1 - x0 as isize + 1) as usize,
        h: (y1 - y0 as isize + 1) as usize,
    })
}

/// 向半径 r 的圆盘膨胀（imageproc 的形态学膨胀，内部就是 FH 距离变换 + 阈值）。
/// `r <= 0.5` 原样返回，与 JS 的 `!(r > 0.5)` 一致；半径向下取整成整数（imageproc 只吃 u8），
/// 与 JS 的浮半径盖戳最多差 1px，这条由对拍 harness 出数字。
/// 只在"笔迹包围盒 + r"这块面积上跑：24MP 画布上的涂抹通常只占千分之几。
pub fn dilate(src: &Alpha, r: f64) -> Alpha {
    if !(r > 0.5) {
        return src.clone();
    }
    let (w, h) = (src.w, src.h);
    let mut out = Alpha::new(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    let bb = match ink_bbox(src, 0) {
        Some(b) => b,
        None => return out,
    };
    let k = r.floor().clamp(0.0, 255.0) as u8;
    let margin = k as usize + 2;
    let x0 = bb.x.saturating_sub(margin);
    let y0 = bb.y.saturating_sub(margin);
    let x1 = (bb.x + bb.w + margin).min(w);
    let y1 = (bb.y + bb.h + margin).min(h);
    let (aw, ah) = (x1 - x0, y1 - y0);
    if aw == 0 || ah == 0 {
        return out;
    }

    // imageproc 把"非零"当前景，正好是 alpha>0 的笔迹
    let mut bytes = Vec::with_capacity(aw * ah);
    for y in y0..y1 {
        let row = y * w + x0;
        bytes.extend_from_slice(&src.v[row..row + aw]);
    }
    let sub = GrayImage::from_vec(aw as u32, ah as u32, bytes).expect("子图缓冲与尺寸不符");
    // Norm::L2 = 欧氏距离（imageproc 用"上取整的整数距离"表达，阈值判据与精确圆盘一致）
    let grown = ip_dilate(&sub, Norm::L2, k).into_vec();
    for (k_row, y) in (y0..y1).enumerate() {
        let row = y * w + x0;
        out.v[row..row + aw].copy_from_slice(&grown[k_row * aw..(k_row + 1) * aw]);
    }
    out
}

/// 横向箱式模糊。窗口越界时**复制边缘**，不是零填充：
/// 涂到画布边的遮罩很常见（删边缘的路人、压边的天空），零填充会把那一圈的贴回权重
/// 拉低到一半以下，原图就从顶边/右边透出来，看着像"角落没改掉"。
fn box_pass_x(src: &[f32], w: usize, r: usize, dst: &mut [f32]) {
    let denom = (2 * r + 1) as f32;
    let last = w - 1;
    par_chunks_mut(dst, w, |blk, y| {
        let base = y * w;
        let at = |i: isize| src[base + i.clamp(0, last as isize) as usize];
        let mut acc = 0.0f32;
        for i in -(r as isize)..=r as isize {
            acc += at(i);
        }
        for x in 0..w {
            blk[x] = acc / denom;
            acc += at(x as isize + r as isize + 1) - at(x as isize - r as isize);
        }
    });
}

/// 纵向箱式模糊：按行条带分核。每个条带先把窗口内 2r+1 行的列和累出来，再在条带内滑动；
/// 条带首行的重叠是 O(w·r) 的冗余，比让三趟纵向保持单核便宜得多。越界同样复制边缘。
fn box_pass_y(src: &[f32], w: usize, h: usize, r: usize, dst: &mut [f32]) {
    let denom = (2 * r + 1) as f32;
    let strip = (h.div_ceil(8)).clamp(8, 64);
    let last = h - 1;
    par_chunks_mut(dst, w * strip, |blk, s| {
        let y0 = s * strip;
        if y0 >= h {
            return;
        }
        let y1 = (y0 + strip).min(h);
        let row = |i: isize| (i.clamp(0, last as isize) as usize) * w;
        let mut acc = vec![0.0f32; w];
        for i in (y0 as isize - r as isize)..=(y0 as isize + r as isize) {
            let base = row(i);
            for x in 0..w {
                acc[x] += src[base + x];
            }
        }
        for y in y0..y1 {
            let out_row = (y - y0) * w;
            for x in 0..w {
                blk[out_row + x] = acc[x] / denom;
            }
            if y + 1 < y1 {
                let add = row(y as isize + r as isize + 1);
                let sub = row(y as isize - r as isize);
                for x in 0..w {
                    acc[x] += src[add + x] - src[sub + x];
                }
            }
        }
    });
}

/// 三次箱式模糊近似一次高斯（每轴总方差 3·(w²−1)/12 = σ²，与 CSS blur 同口径）。
/// 唯一偏离是边缘：CSS 在画布边界零填充，这里复制边缘，见 box_pass_x。
fn blur_alpha(src: &Alpha, sigma: f64) -> Alpha {
    let width = ((4.0 * sigma * sigma + 1.0).sqrt().round() as usize).max(3);
    let width = if width % 2 == 0 { width + 1 } else { width };
    let r = (width - 1) / 2;
    let n = src.w * src.h;
    let mut a: Vec<f32> = src.v.iter().map(|&v| v as f32).collect();
    let mut b = vec![0.0f32; n];
    // 三趟之间保持浮点：每趟截断成 u8 会让峰值系统性偏低
    for _ in 0..3 {
        box_pass_x(&a, src.w, r, &mut b);
        std::mem::swap(&mut a, &mut b);
        box_pass_y(&a, src.w, src.h, r, &mut b);
        std::mem::swap(&mut a, &mut b);
    }
    let v = a.iter().map(|&x| to_u8(x)).collect();
    Alpha { w: src.w, h: src.h, v }
}

/// 贴回 alpha：涂抹区外扩 `inner` 再模糊 `blur`，峰值归一。
/// 归一条件 `mx > 8 && mx < 250` 抄 JS——满幅涂抹模糊后峰值仍接近 255，此时不该整体提亮。
pub fn paste_alpha(user: &Alpha, inner: f64, blur: f64) -> Alpha {
    let grown = dilate(user, inner);
    if !(blur > 0.0) {
        return grown;
    }
    let mut out = blur_alpha(&grown, blur);
    let mx = out.max();
    if mx > 8 && mx < 250 {
        let s = 255.0 / mx as f32;
        for v in out.v.iter_mut() {
            *v = to_u8(*v as f32 * s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> Alpha {
        let mut a = Alpha::new(w, h);
        for y in y0..y1 {
            for x in x0..x1 {
                a.v[y * w + x] = 255;
            }
        }
        a
    }

    #[test]
    fn ink_bbox_闭开区间与阈值() {
        let a = blob(20, 20, 4, 6, 9, 12);
        let b = ink_bbox(&a, 8).unwrap();
        assert_eq!((b.x, b.y, b.w, b.h), (4, 6, 5, 6));
        assert_eq!(ink_bbox(&a, 255), None);
        assert_eq!(ink_bbox(&Alpha::new(8, 8), 8), None);
    }

    #[test]
    fn dilate_半径语义_方块长出圆盘角() {
        let a = blob(64, 64, 30, 30, 34, 34);
        let d = dilate(&a, 6.0);
        // 正上方 6px 处应在膨胀区内，7px 处不在
        assert_eq!(d.get(32, 30 - 6), 255);
        assert_eq!(d.get(32, 30 - 7), 0);
        // 对角 6px 的欧氏距离是 6/√2·2 ≈ 8.49 > 6，所以角上不进圆盘（12 向盖戳也类似）
        assert_eq!(d.get(32 + 5, 30 - 5), 0);
        // 原笔迹一定被覆盖
        assert_eq!(d.get(32, 32), 255);
    }

    #[test]
    fn dilate_半径不足一像素原样返回() {
        let a = blob(8, 8, 2, 2, 4, 4);
        assert_eq!(dilate(&a, 0.5), a);
    }

    #[test]
    fn blur_峰值单调下降且不产生满值平台() {
        // 6×6 的墨块比模糊窗小，中心不该仍是满值
        let a = blob(64, 64, 29, 29, 35, 35);
        let b = blur_alpha(&a, 4.0);
        assert!(b.max() < 255, "峰值 {}", b.max());
        assert!(b.get(32, 32) > b.get(32, 20));
        assert_eq!(b.get(0, 0), 0);
    }

    #[test]
    fn paste_alpha_细笔画被归一() {
        let a = blob(64, 64, 30, 4, 33, 60);
        let p = paste_alpha(&a, 8.0 * 0.4, 8.0);
        // 细线条模糊后峰值远低于 255，归一后应重新接近满值
        assert!(p.max() >= 250, "峰值 {}", p.max());
    }

    /// 涂到画布边的遮罩，边上必须仍是满权重：零填充的模糊窗会把贴边那一圈拉到一半以下，
    /// 原图就从画布边缘透出来，用户看到的就是"角落没改掉"（他 2026-10-06 那张删路人的图）
    #[test]
    fn 贴住画布边的遮罩在边上仍是满权重() {
        // 右半边整条涂满：贴边的点在图内各个方向都有墨，权重必须是满值
        let a = blob(64, 64, 40, 0, 64, 64);
        let p = paste_alpha(&a, 3.2, 8.0);
        for (x, y) in [(63, 0), (63, 32), (63, 63), (56, 0), (56, 63)] {
            assert!(p.get(x, y) >= 250, "({x},{y}) 权重 {}", p.get(x, y));
        }
        // 复制边缘不该把权重漏进没涂到的地方：膨胀后墨从 x=37 起，箱窗总支撑 ±24
        assert_eq!(p.get(0, 32), 0);
        assert_eq!(p.get(0, 0), 0);
    }
}

