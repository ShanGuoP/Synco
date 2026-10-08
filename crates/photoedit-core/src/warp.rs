//! 变形段：手动液化笔画 + 关键点驱动的一键塑形（MLS）。
//!
//! 两种变形都走**反向映射**：目标像素去源图取色，而不是把源像素推出去。
//! 前者每个目标像素必定有值、不会出现洞；后者要靠散布加权，还要处理重叠。
//! 代价是"推"的方向得反着算——位移场 `d(p)` 表示 p 该被搬去哪，取样就取 `p − d(p)`。

use crate::ops::{Auto, Stroke, Tool, Warp};
use crate::px::{sample, Pad};
use moving_least_squares::deform_similarity;
use stitch_core::Rgba;

/// 滑杆满档时，一个笔画步长最多搬掉半径的多少比例。
/// 超过这个量就会把采样点推出盘外，等于自己咬自己的输出。
pub const STROKE_MAX_SHIFT: f32 = 0.45;
/// MLS 稀疏网格步长（像素）。网格越密越准也越慢，8px 是样张上看不出台阶的那一档
pub const MLS_GRID: usize = 8;
/// 一键塑形的局部域 = 人脸框外扩倍数，域外恒等
pub const MLS_SCOPE: f32 = 1.2;

/// 一笔的位移场作用框（像素坐标，闭开区间）
struct Span {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

/// 轨迹抽稀（Douglas–Peucker）。前端按 rAF 采点，一笔几百个点很正常，
/// 入库前用 1px 容差压一遍，参数体积与重放耗时都靠它兜住。
pub fn simplify(points: &[[f32; 2]], tol: f32) -> Vec<[f32; 2]> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (ax, ay) = (points[a][0], points[a][1]);
        let (bx, by) = (points[b][0], points[b][1]);
        let (dx, dy) = (bx - ax, by - ay);
        let len = (dx * dx + dy * dy).sqrt();
        let mut worst = 0f32;
        let mut idx = a;
        for i in a + 1..b {
            let d = if len < 1e-6 {
                ((points[i][0] - ax).powi(2) + (points[i][1] - ay).powi(2)).sqrt()
            } else {
                (((points[i][0] - ax) * dy - (points[i][1] - ay) * dx).abs()) / len
            };
            if d > worst {
                worst = d;
                idx = i;
            }
        }
        if worst > tol {
            keep[idx] = true;
            stack.push((a, idx));
            stack.push((idx, b));
        }
    }
    points.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

/// 手动液化：逐笔顺序应用（后一笔在前一笔的结果上），恢复笔刷朝链起点回拉。
/// `base` 是进入变形段的那张图——恢复笔刷的"原图"就是它，不是某个中间结果。
pub fn strokes(img: &Rgba, warp: &Warp) -> Rgba {
    let mut cur = img.clone();
    if warp.strokes.is_empty() {
        return cur;
    }
    let base = img.clone();
    for s in &warp.strokes {
        apply_stroke(&base, &mut cur, s);
    }
    cur
}

fn apply_stroke(base: &Rgba, cur: &mut Rgba, s: &Stroke) {
    if s.points.is_empty() {
        return;
    }
    let (w, h) = (cur.w, cur.h);
    // 归一化 → 像素：位置按各轴、半径按几何均边（三档同宽高比时盘是正圆）
    let unit = ((w as f32 * h as f32).sqrt()).max(1.0);
    let r = (s.radius * unit).clamp(1.0, unit);
    let pts: Vec<[f32; 2]> = s.points.iter().map(|p| [p[0] * w as f32, p[1] * h as f32]).collect();
    let span = bbox(&pts, r, w, h);
    let k = s.strength.norm().clamp(-1.0, 1.0); // 负压力 = 反方向，前端不给但参数域允许
    let shift = STROKE_MAX_SHIFT * k;
    let src = cur.px.clone();
    let base_px = &base.px;
    for y in span.y0..span.y1 {
        for x in span.x0..span.x1 {
            let (d, wgt) = field_at(&pts, r, x as f32, y as f32, s.tool, shift);
            if wgt <= 0.0 && s.tool != Tool::Restore {
                continue;
            }
            let i = (y * w + x) * 4;
            match s.tool {
                Tool::Restore => {
                    // 恢复 = 朝链起点插值，权重就是衰减盘；越靠盘心回得越多
                    let t = wgt.min(1.0);
                    for c in 0..4 {
                        cur.px[i + c] = (src[i + c] as f32 * (1.0 - t) + base_px[i + c] as f32 * t).round().clamp(0.0, 255.0) as u8;
                    }
                }
                _ => {
                    let v = sample(&src, w, h, x as f32 - d.0, y as f32 - d.1, Pad::Edge);
                    cur.px[i] = v[0].round().clamp(0.0, 255.0) as u8;
                    cur.px[i + 1] = v[1].round().clamp(0.0, 255.0) as u8;
                    cur.px[i + 2] = v[2].round().clamp(0.0, 255.0) as u8;
                    // alpha 不参与位移（照片这里是满值），但仍按取样结果写回，保证同一条路径
                    cur.px[i + 3] = v[3].round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

/// 盘内某点的位移与衰减权重。两条纪律，都是早先那版漏掉的：
///
/// - **连续**：方向取各段单位向量的加权平均，盘心取各投影点的加权平均——两者都是像素位置的连续函数。
///   早先按"最近的那一段"硬选一段：相邻两段一切换，方向与盘心就跳一档，而切换线正好沿笔画走，
///   拖一道笔就留下一串裂缝（液化"把图片弄断裂"就是它）。
/// - **有界**：位移上限 = 半径 × `STROKE_MAX_SHIFT` × 权重，与线段被抽稀成多长无关。
///   早先推挤用的是整段向量，而直线轨迹入库前会被 Douglas–Peucker 压成一两个点——
///   一段几百像素的位移直接超出盘外，采样咬到自己刚写的那一圈像素。
fn field_at(pts: &[[f32; 2]], r: f32, x: f32, y: f32, tool: Tool, shift: f32) -> ((f32, f32), f32) {
    let r2 = r * r;
    let (mut wmax, mut acc, mut qsum, mut wsum) = (0f32, (0f32, 0f32), (0f32, 0f32), 0f32);
    if pts.len() == 1 {
        let (dx, dy) = (x - pts[0][0], y - pts[0][1]);
        let d2 = dx * dx + dy * dy;
        if d2 >= r2 {
            return ((0.0, 0.0), 0.0);
        }
        wmax = falloff(d2, r2);
        (qsum, wsum) = ((pts[0][0] * wmax, pts[0][1] * wmax), wmax);
    } else {
        for j in 0..pts.len() - 1 {
            let (a, b) = (pts[j], pts[j + 1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l2 = dx * dx + dy * dy;
            let t = if l2 < 1e-9 { 0.0 } else { (((x - a[0]) * dx + (y - a[1]) * dy) / l2).clamp(0.0, 1.0) };
            let (px, py) = (a[0] + t * dx, a[1] + t * dy);
            let d2 = (x - px).powi(2) + (y - py).powi(2);
            if d2 >= r2 {
                continue;
            }
            let w = falloff(d2, r2);
            wmax = wmax.max(w);
            wsum += w;
            qsum = (qsum.0 + px * w, qsum.1 + py * w);
            let len = l2.sqrt();
            if len > 1e-6 {
                acc = (acc.0 + dx / len * w, acc.1 + dy / len * w);
            }
        }
    }
    if wmax <= 0.0 || wsum <= 0.0 {
        return ((0.0, 0.0), 0.0);
    }
    let (qx, qy) = (qsum.0 / wsum, qsum.1 / wsum);
    let reach = r * shift * wmax;
    match tool {
        // 推挤：沿加权切向，长度只由半径与压力说了算
        Tool::Push => {
            let n = (acc.0 * acc.0 + acc.1 * acc.1).sqrt();
            if n < 1e-6 {
                return ((0.0, 0.0), wmax);
            }
            ((acc.0 / n * reach, acc.1 / n * reach), wmax)
        }
        // 收缩 / 膨胀 / 恢复：位移 = (p − 盘心) × 系数。盘心是折线上各投影点的加权平均（连续），
        // 所以盘心那一点不动、盘缘由衰减收回 0，最大量落在 0.29r 那一圈，全程没有奇点。
        t => {
            let k = if matches!(t, Tool::Pucker | Tool::Restore) { -2.0 } else { 2.0 };
            let s = k * shift * wmax;
            (((x - qx) * s, (y - qy) * s), wmax)
        }
    }
}

/// 抛物线衰减 `w = (1 − (d/r)²)²`：盘缘处函数值与导数一起归零，边缘才接得上原图
fn falloff(d2: f32, r2: f32) -> f32 {
    let t = 1.0 - d2 / r2;
    t * t
}

fn bbox(pts: &[[f32; 2]], r: f32, w: usize, h: usize) -> Span {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in pts {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    Span {
        x0: (x0 - r).max(0.0) as usize,
        y0: (y0 - r).max(0.0) as usize,
        x1: (x1 + r).ceil().min(w as f32) as usize,
        y1: (y1 + r).ceil().min(h as f32) as usize,
    }
}

/// 一键塑形需要的关键点组（归一化坐标）。由人脸关键点链路准备好交进来，
/// 内核不认识模型，只认识这些点。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FaceShape {
    /// 人脸框 `[x, y, w, h]`
    pub box_norm: [f32; 4],
    /// 下颌轮廓，从一侧到另一侧有序（外轮廓法线方向由相邻点差推出）
    pub jaw: Vec<[f32; 2]>,
    pub eye_l: Vec<[f32; 2]>,
    pub eye_r: Vec<[f32; 2]>,
    /// 虹膜中心，大眼的锚点
    pub iris_l: [f32; 2],
    pub iris_r: [f32; 2],
    /// 鼻翼两点
    pub nose_wing_l: [f32; 2],
    pub nose_wing_r: [f32; 2],
    /// 颏点
    pub chin: [f32; 2],
}

/// 由 `Auto` 滑杆与关键点生成控制点对 `(p, q)`（像素坐标）。
/// 拆成独立函数是为了能单测"滑杆 → 位移"这一步，不用先跑一遍形变。
pub fn control_pairs(auto: &Auto, shape: &FaceShape, w: usize, h: usize) -> (Vec<(f32, f32)>, Vec<(f32, f32)>) {
    let mut p = Vec::new();
    let mut q = Vec::new();
    let px = |pt: [f32; 2]| (pt[0] * w as f32, pt[1] * h as f32);
    let face_w = (shape.box_norm[2] * w as f32).max(1.0);

    // 瘦脸：下颌线上的点沿"外轮廓法线"内收
    let slim = auto.face_slim.norm() * 0.16;
    if slim.abs() > 1e-4 && shape.jaw.len() >= 3 {
        let n = shape.jaw.len();
        for i in 0..n {
            let (a, b, c) = (shape.jaw[i.saturating_sub(1)], shape.jaw[i], shape.jaw[(i + 1).min(n - 1)]);
            let (tx, ty) = (c[0] - a[0], c[1] - a[1]);
            let l = (tx * tx + ty * ty).sqrt().max(1e-6);
            // 法线取 (+ty, −tx)：下颌线自左向右时它指向脸外
            let (nx, ny) = (ty / l, -tx / l);
            let (x, y) = px(b);
            p.push((x, y));
            q.push((x + nx * slim * face_w, y + ny * slim * face_w));
        }
    }

    // 大眼：眼圈相对虹膜中心径向扩张
    let big = auto.eye_big.norm() * 0.12;
    if big.abs() > 1e-4 {
        for (ring, iris) in [(&shape.eye_l, shape.iris_l), (&shape.eye_r, shape.iris_r)] {
            let (cx, cy) = px(iris);
            for pt in ring {
                let (x, y) = px(*pt);
                let (dx, dy) = (x - cx, y - cy);
                let l = (dx * dx + dy * dy).sqrt().max(1e-6);
                let step = big * face_w * 0.5;
                p.push((x, y));
                q.push((x + dx / l * step, y + dy / l * step));
            }
        }
    }

    // 瘦鼻：两翼朝鼻梁中心内收
    let nose = auto.nose_slim.norm() * 0.10;
    if nose.abs() > 1e-4 {
        let (lx, ly) = px(shape.nose_wing_l);
        let (rx, ry) = px(shape.nose_wing_r);
        let (mx, my) = ((lx + rx) * 0.5, (ly + ry) * 0.5);
        for (x, y) in [(lx, ly), (rx, ry), ((lx + mx) * 0.5, (ly + my) * 0.5), ((rx + mx) * 0.5, (ry + my) * 0.5)] {
            let (dx, dy) = (mx - x, my - y);
            let l = (dx * dx + dy * dy).sqrt().max(1e-6);
            let step = nose * face_w;
            p.push((x, y));
            q.push((x + dx / l * step, y + dy / l * step));
        }
    }

    // 下巴：沿"鼻中点 → 颏点"这条轴前后推
    let chin = auto.chin.norm() * 0.12;
    if chin.abs() > 1e-4 {
        let (cx, cy) = px(shape.chin);
        let (mx, my) = px(shape.nose_wing_l);
        let (nx, ny) = px(shape.nose_wing_r);
        let (dx, dy) = (cx - (mx + nx) * 0.5, cy - (my + ny) * 0.5);
        let l = (dx * dx + dy * dy).sqrt().max(1e-6);
        let step = chin * face_w;
        for t in [0.0f32, 0.5, 1.0] {
            p.push((cx - dx * t, cy - dy * t));
            q.push((cx - dx * t + dx / l * step, cy - dy * t + dy / l * step));
        }
    }
    (p, q)
}

/// 一键塑形：在人脸框外扩 1.2 倍的局部域里跑稀疏网格 MLS，域外恒等。
/// 全图 dense 在 24MP 上是分钟级，局部域 + 网格把问题缩到万级节点。
pub fn mls(img: &Rgba, auto: &Auto, shape: &FaceShape) -> Rgba {
    let mut out = img.clone();
    if auto.is_empty() || shape.box_norm[2] <= 0.0 || shape.box_norm[3] <= 0.0 {
        return out;
    }
    let (w, h) = (img.w, img.h);
    let (p, q) = control_pairs(auto, shape, w, h);
    if p.len() < 2 {
        return out;
    }
    // 局部域
    let grow = (MLS_SCOPE - 1.0) * 0.5;
    let bx = shape.box_norm[0] - shape.box_norm[2] * grow;
    let by = shape.box_norm[1] - shape.box_norm[3] * grow;
    let bw = shape.box_norm[2] * (1.0 + 2.0 * grow);
    let bh = shape.box_norm[3] * (1.0 + 2.0 * grow);
    let x0 = (bx * w as f32).max(0.0) as usize;
    let y0 = (by * h as f32).max(0.0) as usize;
    let x1 = ((bx + bw) * w as f32).ceil().min(w as f32 - 1.0) as usize;
    let y1 = ((by + bh) * h as f32).ceil().min(h as f32 - 1.0) as usize;
    if x1 <= x0 || y1 <= y0 {
        return out;
    }
    let gw = (x1 - x0) / MLS_GRID + 2;
    let gh = (y1 - y0) / MLS_GRID + 2;
    // 节点位移场
    let mut field = vec![[0f32; 2]; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let x = x0 as f32 + gx as f32 * MLS_GRID as f32;
            let y = y0 as f32 + gy as f32 * MLS_GRID as f32;
            let (dx, dy) = mls_at(&p, &q, x, y);
            field[gy * gw + gx] = [dx, dy];
        }
    }
    let src = &img.px;
    for y in y0..=y1 {
        for x in x0..=x1 {
            // 位移按网格双线性插值：形变本身已经是平滑的，插值不会额外糊
            let fx = (x as f32 - x0 as f32) / MLS_GRID as f32;
            let fy = (y as f32 - y0 as f32) / MLS_GRID as f32;
            let (gx, gy) = (fx.floor().max(0.0) as usize, fy.floor().max(0.0) as usize);
            let (gx1, gy1) = ((gx + 1).min(gw - 1), (gy + 1).min(gh - 1));
            let (tx, ty) = (fx - gx as f32, fy - gy as f32);
            let a = field[gy * gw + gx];
            let b = field[gy * gw + gx1];
            let c = field[gy1 * gw + gx];
            let d = field[gy1 * gw + gx1];
            let dx = (a[0] + (b[0] - a[0]) * tx) * (1.0 - ty) + (c[0] + (d[0] - c[0]) * tx) * ty;
            let dy = (a[1] + (b[1] - a[1]) * tx) * (1.0 - ty) + (c[1] + (d[1] - c[1]) * tx) * ty;
            let v = sample(src, w, h, x as f32 - dx, y as f32 - dy, Pad::Edge);
            let i = (y * w + x) * 4;
            out.px[i] = v[0].round().clamp(0.0, 255.0) as u8;
            out.px[i + 1] = v[1].round().clamp(0.0, 255.0) as u8;
            out.px[i + 2] = v[2].round().clamp(0.0, 255.0) as u8;
            out.px[i + 3] = v[3].round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// 单个点的 similarity MLS。控制点恰好落在查询点上时权重会发散，
/// 这时直接取该点的目标位置——`deform_similarity` 内部也这么处理，
/// 但位移要自己算，所以这里同样先判一次。
fn mls_at(p: &[(f32, f32)], q: &[(f32, f32)], x: f32, y: f32) -> (f32, f32) {
    for (i, (px, py)) in p.iter().enumerate() {
        if (px - x).abs() < 0.5 && (py - y).abs() < 0.5 {
            return (q[i].0 - x, q[i].1 - y);
        }
    }
    let (nx, ny) = deform_similarity(p, q, (x, y));
    (nx - x, ny - y)
}

/// 变形段总入口：先笔画后 MLS（笔画是用户手工意图，放在前面不被自动结果覆盖）
pub fn apply(img: &Rgba, warp: &Warp, shape: Option<&FaceShape>) -> Rgba {
    let mut cur = strokes(img, warp);
    if !warp.auto.is_empty() {
        if let Some(s) = shape {
            cur = mls(&cur, &warp.auto, s);
        }
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{Auto, Stroke, Tool, Warp};
    use stitch_core::Rgba;

    /// 画一张有唯一特征的图：中央一条竖亮带，位置可量
    fn striped(w: usize, h: usize) -> Rgba {
        let mut img = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if (w / 2..w / 2 + 6).contains(&x) { 240u8 } else { 20u8 };
                img.set(x, y, [v, v, v, 255]);
            }
        }
        img
    }

    fn stroke(tool: Tool, pts: &[[f32; 2]], radius: f32, strength: i32) -> Stroke {
        Stroke { tool, points: pts.to_vec(), radius, strength: crate::ops::Slider(strength) }
    }

    fn hash(img: &Rgba) -> u64 {
        let mut h: u64 = 14695981039346656037;
        for b in &img.px {
            h ^= *b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        h
    }

    #[test]
    fn 空笔画就是恒等() {
        let img = striped(64, 32);
        assert_eq!(strokes(&img, &Warp::default()), img);
        assert_eq!(apply(&img, &Warp::default(), None), img);
    }

    #[test]
    fn 推挤把亮带拖向拖动方向() {
        let img = striped(64, 32);
        let mut wp = Warp::default();
        // 在亮带左侧向右拖一条，亮带应该被推向右
        wp.strokes.push(stroke(Tool::Push, &[[0.30, 0.5], [0.42, 0.5]], 0.12, 100));
        let out = strokes(&img, &wp);
        let center_of_mass = |im: &Rgba| -> f32 {
            let (mut s, mut n) = (0f32, 0f32);
            for y in 0..im.h {
                for x in 0..im.w {
                    let v = im.get(x, y)[0] as f32;
                    s += v * x as f32;
                    n += v;
                }
            }
            s / n
        };
        assert!(center_of_mass(&out) > center_of_mass(&img), "亮带要往右移");
        // 离笔画很远的左边仍然没动
        assert_eq!(out.get(0, 16), img.get(0, 16));
    }

    #[test]
    fn 收缩收窄亮带膨胀把它撑开() {
        // 盘心压在亮带正中：径向位移在带两侧方向相反，于是带的宽度就是判据
        let img = banded(80, 20);
        let frac = |im: &Rgba| {
            let (a, b) = (im.w * 3 / 10, im.w * 7 / 10);
            let mut n = 0;
            for y in 0..im.h {
                for x in a..b {
                    if im.get(x, y)[0] > 128 {
                        n += 1;
                    }
                }
            }
            n as f32 / ((b - a) * im.h) as f32
        };
        let mk = |t: Tool| {
            let mut wp = Warp::default();
            wp.strokes.push(stroke(t, &[[0.5, 0.5], [0.5, 0.5]], 0.16, 100));
            strokes(&img, &wp)
        };
        let (base, pucker, bloat) = (frac(&img), frac(&mk(Tool::Pucker)), frac(&mk(Tool::Bloat)));
        assert!(pucker < base, "收缩要把带子收窄：{base} → {pucker}");
        assert!(bloat > base, "膨胀要把带子撑开：{base} → {bloat}");
    }

    #[test]
    fn 恢复笔刷把笔画抹回去() {
        let img = striped(64, 32);
        let mut wp = Warp::default();
        let push = stroke(Tool::Push, &[[0.30, 0.5], [0.45, 0.5]], 0.14, 100);
        wp.strokes.push(push.clone());
        let warped = strokes(&img, &wp);
        assert_ne!(warped, img, "先确认推挤有效果");
        // 同一条轨迹上补一笔满压力恢复：盘心应当回到链起点
        wp.strokes.push(stroke(Tool::Restore, &[[0.30, 0.5], [0.45, 0.5]], 0.14, 100));
        let restored = strokes(&img, &wp);
        let mut diff = 0usize;
        for i in 0..32 * 64 {
            if restored.px[i * 4] != img.px[i * 4] {
                diff += 1;
            }
        }
        assert!(diff < 32, "恢复笔刷后仍差 {diff} 个像素（盘缘的过渡带允许少量残留）");
    }

    #[test]
    fn 同参数重放逐位一致() {
        let img = striped(96, 48);
        let mut wp = Warp::default();
        wp.strokes.push(stroke(Tool::Push, &[[0.2, 0.3], [0.35, 0.42], [0.5, 0.36]], 0.1, 80));
        wp.strokes.push(stroke(Tool::Bloat, &[[0.6, 0.7], [0.72, 0.66]], 0.08, 60));
        wp.strokes.push(stroke(Tool::Pucker, &[[0.8, 0.2], [0.86, 0.3]], 0.06, 55));
        let a = hash(&strokes(&img, &wp));
        for _ in 0..3 {
            assert_eq!(a, hash(&strokes(&img, &wp)), "重放不一致");
        }
    }

    /// 亮带宽度按画幅等比，这样才能跨尺寸比较相对效果
    fn banded(w: usize, h: usize) -> Rgba {
        let mut img = Rgba::new(w, h);
        let a = w / 2 - w / 20;
        let b = w / 2 + w / 20;
        for y in 0..h {
            for x in 0..w {
                let v = if (a..b).contains(&x) { 240u8 } else { 20u8 };
                img.set(x, y, [v, v, v, 255]);
            }
        }
        img
    }

    #[test]
    fn 参数与画幅成比例() {
        // 归一化参数的意义：小图上手感对了，大图上必须落在同一相对位置
        let frac = |im: &Rgba| {
            let (a, b) = (im.w / 2 - im.w / 20, im.w / 2 + im.w / 20);
            let mut n = 0f32;
            for y in 0..im.h {
                for x in a..b {
                    n += im.get(x, y)[0] as f32;
                }
            }
            n / ((b - a) * im.h) as f32 / 240.0
        };
        let mut wp = Warp::default();
        wp.strokes.push(stroke(Tool::Push, &[[0.35, 0.5], [0.55, 0.5]], 0.12, 100));
        let s = strokes(&banded(80, 40), &wp);
        let b = strokes(&banded(160, 80), &wp);
        // 把亮带推走之后，同一相对窗口的平均亮度应当按同一比例下降
        let (before, after_s, after_b) = (frac(&banded(80, 40)), frac(&s), frac(&b));
        assert!(before > 0.9, "基准窗口该基本被亮带填满：{before}");
        let drop_s = before - after_s;
        let drop_b = frac(&banded(160, 80)) - after_b;
        assert!(drop_s > 0.05 && drop_b > 0.05, "两边都要被推开：{drop_s} / {drop_b}");
        assert!((drop_s - drop_b).abs() < 0.25, "相对位移随画幅漂了：{drop_s} vs {drop_b}");
    }

    #[test]
    fn 抽稀压掉冗余点但留住拐角() {
        // 一条几乎直线上的一百个点 → 应该只剩两端
        let line: Vec<[f32; 2]> = (0..100).map(|i| [i as f32 / 100.0, i as f32 / 100.0]).collect();
        let s = simplify(&line, 0.001);
        assert_eq!(s.len(), 2, "直线被留成 {} 个点", s.len());
        // 带拐角的：拐角必须活下来
        let mut v = line.clone();
        v.push([0.9, 0.1]);
        let s = simplify(&v, 0.001);
        assert!(s.len() >= 3, "拐角丢了");
        assert_eq!(s.first(), v.first());
        assert_eq!(s.last(), v.last());
        assert_eq!(simplify(&[[0.1, 0.2]], 0.001).len(), 1);
    }

    fn shape() -> FaceShape {
        FaceShape {
            box_norm: [0.3, 0.2, 0.4, 0.5],
            jaw: (0..9).map(|i| [0.35 + i as f32 * 0.03, 0.55 + (i as f32 * 0.02 - 0.08).abs()]).collect(),
            eye_l: (0..6).map(|i| [0.40 + i as f32 * 0.01, 0.35]).collect(),
            eye_r: (0..6).map(|i| [0.55 + i as f32 * 0.01, 0.35]).collect(),
            iris_l: [0.425, 0.35],
            iris_r: [0.575, 0.35],
            nose_wing_l: [0.47, 0.48],
            nose_wing_r: [0.53, 0.48],
            chin: [0.5, 0.66],
        }
    }

    #[test]
    fn 零滑杆不生成控制点() {
        let (p, q) = control_pairs(&Auto::default(), &shape(), 200, 300);
        assert!(p.is_empty() && q.is_empty());
        let img = striped(64, 32);
        assert_eq!(mls(&img, &Auto::default(), &shape()), img);
    }

    #[test]
    fn 瘦脸的控制点朝脸内收() {
        let s = shape();
        let mut a = Auto::default();
        a.face_slim.0 = 100;
        let (p, q) = control_pairs(&a, &s, 1000, 1000);
        assert_eq!(p.len(), q.len());
        assert!(!p.is_empty());
        // 下颌在脸框下半部：内收意味着朝框的中心线靠
        for (i, (px, py)) in p.iter().enumerate() {
            let (qx, qy) = q[i];
            let d = ((qx - px).powi(2) + (qy - py).powi(2)).sqrt();
            assert!(d > 0.0, "第 {i} 组控制点没位移");
        }
    }

    #[test]
    fn mls域外恒等且重放一致() {
        let img = striped(120, 120);
        let mut a = Auto::default();
        a.face_slim.0 = 60;
        a.eye_big.0 = 40;
        let s = shape();
        let out1 = mls(&img, &a, &s);
        let out2 = mls(&img, &a, &s);
        assert_eq!(out1, out2, "MLS 必须可重放");
        // 局部框外（左上角一大块）不动
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(out1.get(x, y), img.get(x, y), "域外被改了：({x},{y})");
            }
        }
    }

    #[test]
    fn 没有关键点时一键塑形不生效() {
        let img = striped(64, 32);
        let mut wp = Warp::default();
        wp.auto.face_slim.0 = 100;
        assert_eq!(apply(&img, &wp, None), img, "拿不到关键点就不该乱动");
    }

    /// 轨迹被抽稀成两端点之后的长直线：段长 380px。
    /// 早先推挤直接拿整段向量当位移（0.45 × 380 ≈ 171px，远超盘半径），采样咬到自己刚写的像素，
    /// 屏幕上就是一道沿路的裂缝。
    #[test]
    fn 推挤的位移以半径的比例为上限() {
        let pts = vec![[20.0, 50.0], [400.0, 50.0]];
        let (r, shift) = (60.0f32, STROKE_MAX_SHIFT);
        let (d, w) = field_at(&pts, r, 200.0, 50.0, Tool::Push, shift);
        let len = (d.0 * d.0 + d.1 * d.1).sqrt();
        assert!(w > 0.99, "盘心权重该接近 1，实有 {w}");
        assert!(len <= r * shift + 1e-3, "位移 {len} 越过了半径 {r} 的 45%");
        assert!(d.0 > 1.0 && d.1.abs() < 1e-6, "该沿轨迹方向推，实有 {d:?}");
        // 盘外一格都不动
        let (out, wo) = field_at(&pts, r, 200.0, 50.0 + r + 1.0, Tool::Push, shift);
        assert_eq!((wo, out), (0.0, (0.0, 0.0)), "盘缘外还在推");
    }

    /// 拐角两侧的场必须连续：三种刷子都扫一条穿过顶点的横线，逐像素比邻两格的差。
    /// 早先"取最近那一段"在这里跳档，一跳就是几十像素——那就是用户看到的"液化把图弄断裂"。
    #[test]
    fn 拐角两侧的场连续() {
        let pts = vec![[40.0, 60.0], [200.0, 60.0], [200.0, 220.0]];
        let (r, shift) = (60.0f32, 0.7f32);
        for tool in [Tool::Push, Tool::Pucker, Tool::Bloat] {
            let mut worst = 0f32;
            let mut prev: Option<(f32, f32)> = None;
            for k in 0..=160 {
                let x = 120.0 + k as f32;
                let (d, _) = field_at(&pts, r, x, 60.0, tool, shift);
                if let Some(p) = prev {
                    worst = worst.max((d.0 - p.0).abs().max((d.1 - p.1).abs()));
                }
                prev = Some(d);
            }
            assert!(worst < 2.0, "{tool:?} 在拐角处一格跳 {worst} 像素：场不连续，沿路会裂");
        }
    }

    /// 盘心那一点不该被甩飞（收缩的奇点）：越靠盘心位移越小，且方向指向盘心
    #[test]
    fn 收缩在盘心不甩像素() {
        let pts = vec![[100.0, 100.0], [180.0, 100.0]];
        let (r, shift) = (60.0f32, STROKE_MAX_SHIFT);
        let (c, wc) = field_at(&pts, r, 140.0, 100.0, Tool::Pucker, shift);
        assert!(wc > 0.99);
        assert!(c.0.abs() < 1.0 && c.1.abs() < 1.0, "盘心位移该趋 0，实有 {c:?}");
        // 盘心正上方一格：位移应朝盘心（y 分量向下）
        let (u, _) = field_at(&pts, r, 140.0, 70.0, Tool::Pucker, shift);
        assert!(u.1 > 0.5, "收缩没把像素往盘心拉：{u:?}");
        let (v, _) = field_at(&pts, r, 140.0, 70.0, Tool::Bloat, shift);
        assert!(v.1 < -0.5, "膨胀方向反了：{v:?}");
    }
}
