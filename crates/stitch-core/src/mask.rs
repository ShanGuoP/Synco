//! 蒙版运算：包围盒、外扩、贴回权重。全部只读 alpha。
//! 外扩在浏览器画布上是 12 向平移盖戳取 max，这里用 imageproc 的形态学膨胀（邻域取 max 的灰度膨胀）：
//! 半径语义一致，边缘不逐像素相同（方案 §5 Phase 1 已认可这一点）。
//! 关键是"取 max"而不是"二值化后再膨胀"——抗锯齿边和半擦除残留的半透明值要原样往外传播，
//! 抬成全墨会让外扩比名义半径多吃一圈，贴回权重跟着偏。

use px_core::buffer::{Alpha, Box2};
use px_core::par::par_chunks_mut;
use px_core::resize::to_u8;
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

/// 灰度圆盘膨胀的等级切片。
///
/// imageproc 的形态学膨胀把"非零"当前景：半擦除残留和抗锯齿边会被抬成满墨，
/// 等于外扩在那些地方多吃一整圈半径，贴回权重与发给模型的保留区都跟着偏。
/// 这里按等级二值化后各膨胀一次，命中的最高一级即结果——相邻两级差 32，
/// 误差不超过 32，而这一段紧接着还要过羽化模糊与峰值归一，看不见这个台阶。
///
/// 8 级的成本实测（release，2048×3072 涂抹层、笔迹占 1/4 画面的病态情形）：
/// r=38/96/200 分别 161/182/239ms；正常笔迹（画面千分之几）是微秒级。
/// 云端一条请求本身要十几秒，这个量级不影响提交手感，所以换正确性。
const DILATE_LEVELS: [u8; 8] = [32, 64, 96, 128, 160, 192, 224, 255];

/// 向半径 r 的圆盘膨胀：半透明的边按原量级往外传，不会被抬成满墨。
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

    let mut sub = Vec::with_capacity(aw * ah);
    for y in y0..y1 {
        let row = y * w + x0;
        sub.extend_from_slice(&src.v[row..row + aw]);
    }
    // 相邻阈值之间没有像素时，两层的二值输入相同：只计算这组的最高一级。
    // 纯黑白蒙版因此只跑 255 层；全灰度蒙版仍沿用原来的八层与输出量级。
    let mut present = [false; 256];
    for &a in &sub {
        present[a as usize] = true;
    }
    for (i, &lv) in DILATE_LEVELS.iter().enumerate() {
        let next = DILATE_LEVELS.get(i + 1).map_or(256, |&v| v as usize);
        // imageproc 用 255 同时表示最大距离与无前景：半径钳到 255 时，
        // 空层也会命中。保留这个边界上的原八层行为，避免改变历史结果。
        if k < 255 && !present[lv as usize..next].iter().any(|&v| v) {
            continue;
        }
        let bits: Vec<u8> = sub.iter().map(|&a| if a >= lv { 255 } else { 0 }).collect();
        let img = GrayImage::from_vec(aw as u32, ah as u32, bits).expect("子图缓冲与尺寸不符");
        // Norm::L2 = 欧氏圆盘（imageproc 用"上取整的整数距离"表达，阈值判据与精确圆盘一致）
        let grown = ip_dilate(&img, Norm::L2, k).into_vec();
        for (y, row) in (y0..y1).enumerate() {
            let dst = row * w + x0;
            let band = &grown[y * aw..y * aw + aw];
            for (i, &g) in band.iter().enumerate() {
                // 等级升序遍历，所以"命中即覆盖"取到的就是命中的最高一级
                if g > 0 {
                    out.v[dst + i] = lv;
                }
            }
        }
    }
    // 原值本来就比切片等级细：满墨那一段仍是 255，其余取"原值"与"膨胀结果"里更大的那个，
    // 保证膨胀只会把墨往外推，绝不会把已经在笔迹上的值改小
    for y in y0..y1 {
        for x in x0..x1 {
            let i = y * w + x;
            if out.v[i] < src.v[i] {
                out.v[i] = src.v[i];
            }
        }
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

    /// 固定八层、整幅计算的参考算子：验证层合并不会改变任何灰度或边界像素。
    fn eight_layer_reference(src: &Alpha, r: f64) -> Alpha {
        if !(r > 0.5) || src.w == 0 || src.h == 0 || ink_bbox(src, 0).is_none() {
            return src.clone();
        }
        let k = r.floor().clamp(0.0, 255.0) as u8;
        let mut out = src.clone();
        for &lv in &DILATE_LEVELS {
            let bits = src.v.iter().map(|&v| if v >= lv { 255 } else { 0 }).collect();
            let img = GrayImage::from_vec(src.w as u32, src.h as u32, bits).unwrap();
            for (dst, &v) in out.v.iter_mut().zip(ip_dilate(&img, Norm::L2, k).as_raw()) {
                if v != 0 {
                    *dst = (*dst).max(lv);
                }
            }
        }
        out
    }

    #[test]
    fn 合并膨胀层与八层参考逐像素一致() {
        // 每个等级的两侧、稀疏半透明、黑白、空蒙版，以及超出图幅的半径。
        let edges = [0, 1, 31, 32, 33, 63, 64, 65, 95, 96, 97, 127, 128,
            129, 159, 160, 161, 191, 192, 193, 223, 224, 225, 254, 255];
        for (w, h) in [(1, 1), (1, 19), (23, 1), (37, 29)] {
            for kind in 0..5 {
                let mut src = Alpha::new(w, h);
                for (i, v) in src.v.iter_mut().enumerate() {
                    *v = match kind {
                        0 => 0,
                        1 => if i % 7 == 0 { 255 } else { 0 },
                        2 => if i % 7 == 0 { 100 } else { 0 },
                        3 => edges[i % edges.len()],
                        _ => ((i * 73 + i / 11) % 256) as u8,
                    };
                }
                for r in [0.5, 0.51, 0.99, 1.0, 3.0, 9.0, 255.0, 300.0] {
                    assert_eq!(dilate(&src, r), eight_layer_reference(&src, r),
                        "{w}×{h}, kind={kind}, r={r}");
                }
            }
        }
    }

    /// 抗锯齿边与半擦除残留是**半透明**的，膨胀只能把那个量级往外传，不能把它抬成满墨：
    /// 抬满就等于外扩在那些地方多吃一整圈半径，贴回权重与发给模型的保留区都跟着偏。
    /// 旧实现（imageproc 直接吃 alpha）在这三个点上给 255/255/0，所以这几条断言就是它的守门人。
    #[test]
    fn dilate_软边按量级传播不被抬满() {
        let (w, h) = (64usize, 16usize);
        let mut a = Alpha::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if x < 20 { 255 } else if x == 20 { 90 } else if x == 21 { 40 } else { 0 };
                a.v[y * w + x] = v;
            }
        }
        let d = dilate(&a, 6.0);
        let y = 8;
        assert_eq!(d.get(25, y), 255, "满墨那一段该照常外扩 6px");
        // 距满墨 6px 处只能拿到 90 那个量级（切片到 64），拿不到 255
        assert_eq!(d.get(26, y), 64, "半值被抬成满墨了");
        assert_eq!(d.get(27, y), 32, "更远处该只剩 40 那个量级");
        assert_eq!(d.get(28, y), 0, "超出半径还留着墨，说明外扩多吃了");
        // 任何一点都不该变暗
        for i in 0..w * h {
            assert!(d.v[i] >= a.v[i], "第 {i} 点从 {} 变成 {}", a.v[i], d.v[i]);
        }
    }

    /// 等级切片把膨胀成本放大了 8 倍，这条用来量到底值不值：
    /// `cargo test --release -p stitch-core -- --ignored --nocapture bench_dilate`
    #[test]
    #[ignore]
    fn bench_dilate_等级切片() {
        use std::time::Instant;
        // 涂抹层按 proxy 档：2048×3072，笔迹占画面四分之一（外扩后的子图就是这个量级）
        let (w, h) = (2048usize, 3072usize);
        let mut a = Alpha::new(w, h);
        for y in h / 4..h * 3 / 4 {
            for x in w / 4..w * 3 / 4 {
                let v = (((x + y) % 232) + 24) as u8;
                a.v[y * w + x] = v;
            }
        }
        for r in [38.0f64, 96.0, 200.0] {
            let t = Instant::now();
            let d = dilate(&a, r);
            println!(
                "dilate r={r:>3} on {w}x{h}  {:.1}ms  峰值 {}",
                t.elapsed().as_secs_f64() * 1000.0,
                d.max()
            );
        }
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
