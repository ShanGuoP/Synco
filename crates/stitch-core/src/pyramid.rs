//! 拉普拉斯金字塔多频段融合：低频取对方的、高频留自己的，接缝处才不会露色块或露边。
//! alpha=1 处严格等于校正后的模型输出，alpha=0 处严格等于原图像素——未涂区域零漂移就是这么来的。

use crate::buffer::{Alpha, Box2, Rgba};
use crate::par::par_chunks_mut;
use crate::resize::{crop_scale_alpha, crop_scale_rgba, grow_rgba, half_alpha, half_rgba, to_u8};

/// 第 i 层上的包围盒：`Math.floor(box.x / 2^i)` 起，`Math.ceil((box.x+box.w)/2^i)` 止
fn box_at(l0: &Box2, i: usize) -> Box2 {
    let f = (1u64 << i) as f64;
    let x = (l0.x as f64 / f).floor() as usize;
    let y = (l0.y as f64 / f).floor() as usize;
    Box2::new(
        x,
        y,
        ((l0.x + l0.w) as f64 / f).ceil() as usize - x,
        ((l0.y + l0.h) as f64 / f).ceil() as usize - y,
    )
}

/// 只取 RGB 三个平面，alpha 一律不读（和 JS 的 `region` 一样）
fn region_rgba(img: &Rgba, b: &Box2) -> Vec<f32> {
    let w = b.w.min(img.w.saturating_sub(b.x));
    let h = b.h.min(img.h.saturating_sub(b.y));
    let mut f = vec![0.0f32; w * h * 3];
    par_chunks_mut(&mut f, w * 3, |blk, y| {
        for x in 0..w {
            let i = img.off(b.x + x, b.y + y);
            blk[x * 3] = img.px[i] as f32;
            blk[x * 3 + 1] = img.px[i + 1] as f32;
            blk[x * 3 + 2] = img.px[i + 2] as f32;
        }
    });
    f
}

fn region_alpha(img: &Alpha, b: &Box2) -> Vec<f32> {
    let w = b.w.min(img.w.saturating_sub(b.x));
    let h = b.h.min(img.h.saturating_sub(b.y));
    let mut f = vec![0.0f32; w * h];
    par_chunks_mut(&mut f, w, |blk, y| {
        for x in 0..w {
            blk[x] = img.v[(b.y + y) * img.w + (b.x + x)] as f32 / 255.0;
        }
    });
    f
}

/// 把平面 `rgb`（铺在画布上的 `win` 那块）写进画布，只写 `clip` 与 `win` 相交的那一段。
/// 越界值夹住、按 Uint8Clamped 取整；`a=None` 时 alpha 全 255。
/// 窗口比盒子大一圈（见 `WIN_PAD`），多算出来的一圈不该进结果——没人认领的像素就该保持原样。
fn write_region(img: &mut Rgba, win: &Box2, clip: &Box2, rgb: &[f32], a: Option<&[f32]>) {
    let w = win.w.min(img.w.saturating_sub(win.x));
    let h = win.h.min(img.h.saturating_sub(win.y));
    let (x0, y0) = (clip.x.max(win.x), clip.y.max(win.y));
    let (x1, y1) = ((clip.x + clip.w).min(win.x + w).min(img.w), (clip.y + clip.h).min(win.y + h).min(img.h));
    par_chunks_mut(&mut img.px, img.w * 4, |row, y| {
        if y < y0 || y >= y1 {
            return;
        }
        let src_row = (y - win.y) * w;
        for x in x0..x1 {
            let q = src_row + (x - win.x);
            let i = x * 4;
            row[i] = to_u8(rgb[q * 3]);
            row[i + 1] = to_u8(rgb[q * 3 + 1]);
            row[i + 2] = to_u8(rgb[q * 3 + 2]);
            row[i + 3] = match a {
                Some(av) => to_u8(av[q] * 255.0),
                None => 255,
            };
        }
    });
}

struct Lap {
    w: usize,
    o: Vec<f32>,
    u: Vec<f32>,
    a: Vec<f32>,
}

/// 一层上的"盒面"：所在层的整幅画布尺寸 + 盒面位置与夹过边界后的尺寸。
/// 放大映射必须按画布尺寸算（与各层 `grow_rgba` 同一个锚点），否则残差与累加项错开半像素，
/// 逐层相消就漏了——高频行上会露出上百个色阶的偏差。
struct Plane {
    cw: usize,
    ch: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

fn plane_at(img: &Rgba, b: &Box2) -> Plane {
    Plane {
        cw: img.w,
        ch: img.h,
        x: b.x,
        y: b.y,
        w: b.w.min(img.w.saturating_sub(b.x)),
        h: b.h.min(img.h.saturating_sub(b.y)),
    }
}

/// 把低一层的盒面放大铺到高一层的盒面上，全程留在 f32。
///
/// 手写双线性而不是走 `fast_image_resize`：那套接口只吃 u8，而带通残差是有符号的——
/// 借一张 u8 画布中转，每层都把越界的那一半夹掉一次（回归：`融合结果对整体平移不变`）。
/// 映射中心与锚点同 FIR；盒外按边缘值延拓（补零会在盒子边框留一道台阶，
/// 而那条边框正贴着权重最小的那一圈像素）。
fn grow_plane(src: &[f32], from: &Plane, to: &Plane) -> Vec<f32> {
    let mut dst = vec![0.0f32; to.w * to.h * 3];
    if from.w == 0 || from.h == 0 || to.w == 0 || to.h == 0 || src.len() < from.w * from.h * 3 {
        return dst;
    }
    let (rx, ry) = (from.cw as f32 / to.cw as f32, from.ch as f32 / to.ch as f32);
    let at = |x: usize, y: usize, c: usize| src[(y * from.w + x) * 3 + c];
    for y in 0..to.h {
        let fy = ((to.y + y) as f32 + 0.5) * ry - 0.5 - from.y as f32;
        let y0 = (fy.floor().max(0.0) as usize).min(from.h - 1);
        let y1 = (y0 + 1).min(from.h - 1);
        let ty = (fy - y0 as f32).clamp(0.0, 1.0);
        for x in 0..to.w {
            let fx = ((to.x + x) as f32 + 0.5) * rx - 0.5 - from.x as f32;
            let x0 = (fx.floor().max(0.0) as usize).min(from.w - 1);
            let x1 = (x0 + 1).min(from.w - 1);
            let tx = (fx - x0 as f32).clamp(0.0, 1.0);
            for c in 0..3 {
                let a0 = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
                let a1 = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
                dst[(y * to.w + x) * 3 + c] = a0 * (1.0 - ty) + a1 * ty;
            }
        }
    }
    dst
}

/// 返回裁切区尺寸的画布：RGB 是融合结果，alpha 是"这一块要不要贴回"的覆盖标记。
/// 本层计算窗口 = 盒子在本层的投影再向四周垫几格（夹到画布内）。
///
/// 往上一层放大时，盒子边缘那一圈要用到"盒外"的低频；只带盒子本身就得靠延拓去猜，
/// 而猜错的边缘会顺着金字塔往下渗——最高层差一格，等于原图层差 2^levels 格。
/// 垫窗口比只垫第 0 层有效，因为每一层都要有自己的余量。
const WIN_PAD: usize = 4;

fn win_at(canvas: &Rgba, b: &Box2) -> Box2 {
    let x = b.x.saturating_sub(WIN_PAD);
    let y = b.y.saturating_sub(WIN_PAD);
    let x1 = (b.x + b.w + WIN_PAD).min(canvas.w).max(x + 1);
    let y1 = (b.y + b.h + WIN_PAD).min(canvas.h).max(y + 1);
    Box2::new(x, y, x1 - x, y1 - y)
}

/// 权重为 0 的地方完全透明，原图逐像素保留；权重不为 0 的地方贴回那步原样采用这里的 RGB
/// （羽化权重已经在每一带里掺过一次，贴回再按它掺就是掺第二遍）。
pub fn pyramid_blend(orig: &Rgba, out: &Rgba, alpha: &Alpha, box0: Box2, levels: usize) -> Rgba {
    let mut l = levels;
    while l > 0 {
        let b = box_at(&box0, l);
        if b.w.max(b.h) < 8 {
            l -= 1;
        } else {
            break;
        }
    }

    let mut go = vec![orig.clone()];
    let mut gu = vec![out.clone()];
    let mut ga = vec![alpha.clone()];
    for i in 1..=l {
        go.push(half_rgba(&go[i - 1]));
        gu.push(half_rgba(&gu[i - 1]));
        ga.push(half_alpha(&ga[i - 1]));
    }
    let wins: Vec<Box2> = (0..=l).map(|i| win_at(&go[i], &box_at(&box0, i))).collect();

    let mut lap: Vec<Lap> = Vec::with_capacity(l + 1);
    for i in 0..=l {
        let b = wins[i];
        let pw = b.w.min(go[i].w.saturating_sub(b.x));
        let mut o = region_rgba(&go[i], &b);
        let mut u = region_rgba(&gu[i], &b);
        let a = region_alpha(&ga[i], &b);
        if i < l {
            // 拉普拉斯 = 本层高斯 − 上层高斯放大回来；顶层的高斯就是它的拉普拉斯
            let up_o = region_rgba(&grow_rgba(&go[i + 1], go[i].w, go[i].h), &b);
            let up_u = region_rgba(&grow_rgba(&gu[i + 1], gu[i].w, gu[i].h), &b);
            par_chunks_mut(&mut o, pw * 3, |blk, row| {
                let base = row * pw * 3;
                for k in 0..blk.len() {
                    blk[k] -= up_o[base + k];
                }
            });
            par_chunks_mut(&mut u, pw * 3, |blk, row| {
                let base = row * pw * 3;
                for k in 0..blk.len() {
                    blk[k] -= up_u[base + k];
                }
            });
        }
        lap.push(Lap { w: pw, o, u, a });
    }

    let mut acc: Option<Vec<f32>> = None;
    for i in (0..=l).rev() {
        let cur = &lap[i];
        let pw = cur.w;
        let (o, u, a) = (&cur.o, &cur.u, &cur.a);
        let mut mixv = vec![0.0f32; o.len()];
        let prev_buf = std::mem::take(&mut acc);
        let prev = prev_buf.as_deref();
        par_chunks_mut(&mut mixv, pw * 3, |blk, row| {
            let jrow = row * pw * 3;
            for x in 0..pw {
                let j = jrow + x * 3;
                let q = row * pw + x;
                let wg = a[q];
                for c in 0..3 {
                    let oo = o[j + c];
                    let mut v = oo + (u[j + c] - oo) * wg;
                    if let Some(p) = prev {
                        v += p[j + c];
                    }
                    blk[x * 3 + c] = v;
                }
            }
        });
        if i > 0 {
            // 往下的累加留在 f32 里：借一张 u8 画布中转，这一层越界的那一半就被夹掉一次
            let (from, to) = (plane_at(&go[i], &wins[i]), plane_at(&go[i - 1], &wins[i - 1]));
            acc = Some(grow_plane(&mixv, &from, &to));
        } else {
            let mut res = Rgba::new(orig.w, orig.h);
            let cov: Vec<f32> = a.iter().map(|&v| if v > 0.0 { 1.0 } else { 0.0 }).collect();
            write_region(&mut res, &wins[0], &box0, &mixv, Some(&cov));
            return res;
        }
    }
    Rgba::new(orig.w, orig.h)
}

/// 盒内变体：先把三张画布裁到"包围盒 + 卷积核支撑宽度"，再在裁出来的小图上跑同一套融合。
/// 与 `pyramid_blend` 的结果不保证逐位相同——逐层降采样的栅格锚点从画布原点变成了盒原点，
/// 差多少由 bench_两版金字塔比对 出数字。
pub fn pyramid_blend_boxed(orig: &Rgba, out: &Rgba, alpha: &Alpha, box0: Box2, levels: usize) -> Rgba {
    let m = 8usize;
    let x0 = box0.x.saturating_sub(m).min(orig.w);
    let y0 = box0.y.saturating_sub(m).min(orig.h);
    let x1 = (box0.x + box0.w + m).min(orig.w);
    let y1 = (box0.y + box0.h + m).min(orig.h);
    if x1 <= x0 || y1 <= y0 {
        return Rgba::new(orig.w, orig.h);
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let o = crop_scale_rgba(orig, x0, y0, cw, ch, cw, ch);
    let u = crop_scale_rgba(out, x0, y0, cw, ch, cw, ch);
    let a = crop_scale_alpha(alpha, x0, y0, cw, ch, cw, ch);
    let local = Box2::new(box0.x - x0, box0.y - y0, box0.w, box0.h);
    let small = pyramid_blend(&o, &u, &a, local, levels);
    let mut res = Rgba::new(orig.w, orig.h);
    par_chunks_mut(&mut res.px, orig.w * 4, |row, y| {
        if y < box0.y || y >= box0.y + box0.h {
            return;
        }
        let sy = y - box0.y + local.y;
        let n = box0.w.min(orig.w - box0.x);
        let sx0 = box0.x - x0;
        for x in 0..n {
            let si = (sy * small.w + sx0 + x) * 4;
            let di = (box0.x + x) * 4;
            row[di..di + 4].copy_from_slice(&small.px[si..si + 4]);
        }
    });
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: usize, h: usize, c: [u8; 3]) -> Rgba {
        let mut img = Rgba::new(w, h);
        for i in 0..w * h {
            img.px[i * 4..i * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
        img
    }

    fn full_alpha(w: usize, h: usize, v: u8) -> Alpha {
        let mut a = Alpha::new(w, h);
        a.v.iter_mut().for_each(|x| *x = v);
        a
    }

    #[test]
    fn 权重满时等于模型输出_权重零时逐像素透明() {
        let orig = flat(64, 64, [10, 20, 30]);
        let out = flat(64, 64, [200, 210, 220]);
        let b = Box2::new(0, 0, 64, 64);

        let r = pyramid_blend(&orig, &out, &full_alpha(64, 64, 255), b, 4);
        assert_eq!(&r.px[0..3], &[200, 210, 220]);
        assert_eq!(r.px[3], 255);

        let r = pyramid_blend(&orig, &out, &full_alpha(64, 64, 0), b, 4);
        // alpha=0 的像素完全透明，贴回时原图逐像素保留
        assert_eq!(r.px[3], 0);
        assert_eq!(&r.px[0..3], &[10, 20, 30]);
    }

    #[test]
    fn 包围盒外不写入() {
        let orig = flat(64, 64, [10, 20, 30]);
        let out = flat(64, 64, [200, 210, 220]);
        let r = pyramid_blend(&orig, &out, &full_alpha(64, 64, 255), Box2::new(8, 8, 16, 16), 3);
        assert_eq!(r.get(7, 8), [0, 0, 0, 0]);
        assert_eq!(r.get(24, 24), [0, 0, 0, 0]);
        // 盒内深处应当就是模型输出；紧贴盒边的像素会掺到框外的透明黑，留给对拍去量
        let p = r.get(12, 12);
        assert_eq!(p[3], 255);
        for c in 0..3 {
            assert!((p[c] as i32 - out.get(12, 12)[c] as i32).abs() <= 2, "盒内 {p:?} 偏离模型输出");
        }
    }

    #[test]
    fn 非原点包围盒的权重与像素对齐() {
        // 回归：region_alpha 曾漏加 b.x/b.y，整块权重错位
        let mut orig = Rgba::new(48, 48);
        let mut out = Rgba::new(48, 48);
        for y in 0..48 {
            for x in 0..48 {
                orig.set(x, y, [(x * 5) as u8, 0, 0, 255]);
                out.set(x, y, [0, 0, 200, 255]);
            }
        }
        let mut a = Alpha::new(48, 48);
        for y in 10..20 {
            for x in 10..20 {
                a.v[y * 48 + x] = 255;
            }
        }
        let r = pyramid_blend(&orig, &out, &a, Box2::new(8, 8, 32, 32), 3);
        for &(x, y) in &[(36, 36), (8, 36), (36, 8), (9, 9)] {
            assert_eq!(r.get(x, y)[3], 0, "盒内 ({x},{y}) 权重应为 0");
        }
        // 小色块 + 4 层金字塔时，深层权重已被摊薄，结果不会严格等于模型输出——
        // 这里只要求它落在两端之间且偏向模型侧，严格的等于下面那个全覆盖场景。
        let p = r.get(15, 15);
        assert_eq!(p[3], 255);
        assert!(p[2] > 120 && p[2] <= 200, "蓝通道 {p:?} 应明显偏向模型输出 200");

        let mut full = Alpha::new(48, 48);
        for y in 8..40 {
            for x in 8..40 {
                full.v[y * 48 + x] = 255;
            }
        }
        let r2 = pyramid_blend(&orig, &out, &full, Box2::new(8, 8, 32, 32), 3);
        let q = r2.get(20, 20);
        assert_eq!(q[3], 255);
        for c in 0..3 {
            assert!(
                (q[c] as i32 - out.get(20, 20)[c] as i32).abs() <= 2,
                "整盒满权重处应等于模型输出，实得 {q:?}"
            );
        }
    }

    #[test]
    fn 小包围盒自动降层() {
        // 4x4 的盒子在第 2 层只剩 1px，<8 会一路降到 0 层，仍要出结果
        let orig = flat(32, 32, [0, 0, 0]);
        let out = flat(32, 32, [255, 255, 255]);
        let r = pyramid_blend(&orig, &out, &full_alpha(32, 32, 255), Box2::new(4, 4, 4, 4), 4);
        assert_eq!(r.get(5, 5), [255, 255, 255, 255]);
    }

    #[test]
    fn 渐变上融合不出现硬跳变() {
        let mut orig = Rgba::new(64, 8);
        let mut a = Alpha::new(64, 8);
        for x in 0..64 {
            for y in 0..8 {
                orig.set(x, y, [x as u8 * 4, 0, 0, 255]);
                a.set(x, y, if x < 24 { 255 } else { if x >= 40 { 0 } else { (255 - (x - 24) * 15) as u8 } });
            }
        }
        let out = flat(64, 8, [0, 0, 200]);
        let r = pyramid_blend(&orig, &out, &a, Box2::new(0, 0, 64, 8), 3);
        // 权重 0 的一侧必须仍是原图（透明，贴回后逐像素等于原图）
        for x in 40..64 {
            assert_eq!(r.get(x, 0)[3], 0, "x={x} 不该带权重");
        }
        // 过渡带内不得出现比两端都亮/都暗的过冲
        let mut prev = r.get(20, 0)[0] as i32;
        let mut max_jump = 0i32;
        for x in 21..40 {
            let v = r.get(x, 0)[0] as i32;
            max_jump = max_jump.max((v - prev).abs());
            prev = v;
        }
        assert!(max_jump < 40, "过渡带跳变 {} 太硬", max_jump);
    }

    /// 羽化权重只能生效一次。金字塔内部已经按权重混过一遍，输出 alpha 若还是那个权重，
    /// `composite_over` 会再掺一次，中段就变成 a²：a=0.5 处模型只占 25%，被擦的东西整条带里透回来。
    /// 所以输出的 alpha 只能是"这一块要不要贴"的覆盖标记。
    #[test]
    fn 过渡带按权重线性掺入而不是权重的平方() {
        let (w, h) = (64usize, 8usize);
        let orig = flat(w, h, [0, 0, 0]);
        let out = flat(w, h, [200, 200, 200]);
        // 恒定权重时每一层的权重都一样，多频段重构就等于线性交叉淡入——期望值是解析的
        let a = full_alpha(w, h, 128);
        let b = pyramid_blend(&orig, &out, &a, Box2::new(0, 0, w, h), 4);
        let wg = 128.0f32 / 255.0;
        let want = wg * 200.0;
        for y in 0..h {
            for x in 0..w {
                let p = b.get(x, y);
                assert_eq!(p[3], 255, "({x},{y}) 的输出 alpha 应是覆盖标记，不是第二次权重");
                // 贴回那步按输出 alpha 走一次 source-over：这里必须正好是掺了 wg 的结果
                let after = p[0] as f32 * (p[3] as f32 / 255.0);
                assert!(
                    (after - want).abs() <= 2.0,
                    "({x},{y})：权重 {wg:.3} 应掺到 {want:.1}，实际 {after:.1}（按 a² 掺只剩 {:.1}）",
                    wg * wg * 200.0
                );
            }
        }
        // 权重为 0 的那一侧仍然逐像素不贴：覆盖标记只能是 0
        let mut half = full_alpha(w, h, 0);
        for x in 0..w / 2 {
            for y in 0..h {
                half.set(x, y, 200);
            }
        }
        let b2 = pyramid_blend(&orig, &out, &half, Box2::new(0, 0, w, h), 4);
        for y in 0..h {
            assert_eq!(b2.get(0, y)[3], 255, "有权重的一侧要贴");
            assert_eq!(b2.get(w - 1, y)[3], 0, "权重 0 的一侧不该被贴上");
        }
    }

    /// 层间累加必须在浮点里做。把中间结果写回 u8 画布，越界的那一半每层都被夹掉一次，
    /// 结果是"两张图一起提亮"不等于"融合完再提亮"——贴回区里高对比边周围系统性偏亮、纹理被压平。
    #[test]
    fn 融合结果对整体平移不变() {
        let (w, h) = (80usize, 8usize);
        let mut a = Alpha::new(w, h);
        for x in 0..w {
            for y in 0..h {
                a.set(x, y, (x * 255 / (w - 1)) as u8);
            }
        }
        // 原图近白、模型近黑的高对比棋盘：过渡带里逐带权重不同，中间值会跨过 0 与 255
        let mk = |shift: u8| -> (Rgba, Rgba) {
            let mut o = Rgba::new(w, h);
            let mut u = Rgba::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let cell = (x / 4 + y / 4) % 2 == 0;
                    o.set(x, y, [{ if cell { 200u8 } else { 60u8 } }.saturating_add(shift), 180, 100, 255]);
                    u.set(x, y, [{ if cell { 40u8 } else { 170u8 } }.saturating_add(shift), 60, 200, 255]);
                }
            }
            (o, u)
        };
        let (o0, u0) = mk(0);
        let (o1, u1) = mk(20);
        let r0 = pyramid_blend(&o0, &u0, &a, Box2::new(0, 0, w, h), 4);
        let r1 = pyramid_blend(&o1, &u1, &a, Box2::new(0, 0, w, h), 4);
        let mut worst = 0f32;
        let mut at = 0usize;
        for y in 0..h {
            for x in 0..w {
                // 只比两端都留有余量的像素：那 20 的平移本该原样穿过去
                let a0 = r0.get(x, y)[0] as i32;
                let a1 = r1.get(x, y)[0] as i32;
                if a0 > 40 && a0 < 180 && a1 > 60 && a1 < 200 {
                    let d = ((a1 - a0) as f32 - 20.0).abs();
                    if d > worst {
                        worst = d;
                        at = x;
                    }
                }
            }
        }
        assert!(worst <= 3.0, "提亮 20 之后结果位移了 {:.1}（x={at}）：中间层被夹进 0..255 了", worst);
    }
}
