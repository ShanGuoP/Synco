//! 笔画简化、局部位移场与采样合成。
use crate::ops::{Stroke, Tool, Warp};
use crate::px::{sample, Pad};
use px_core::{par::par_chunks_mut, Rgba};
use super::STROKE_MAX_SHIFT;
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
    for s in &warp.strokes {
        apply_stroke(img, &mut cur, s);
    }
    cur
}

/// 一笔的位移场每像素存四个累加量：`[最大权重, 平面 X, 平面 Y, 权重和]`。
/// 平面 X/Y 按笔种装不同东西——推挤装加权单位切向，其余装加权投影点。
/// 逐段累加的下标顺序与"逐像素扫完全部线段"的标量版一致，浮点求和因此逐位相同。
const FIELD: usize = 4;

/// 行带高度：带内自带一份位移场，带与带之间不共享写入，所以能并行。
/// 一笔盖不满两个带时 `par_chunks_mut` 自己退回串行，小笔不为起线程付钱。
const BAND_ROWS: usize = 8;

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
    let (sw, sh) = (span.x1.saturating_sub(span.x0), span.y1.saturating_sub(span.y0));
    if sw == 0 || sh == 0 {
        return;
    }
    let k = s.strength.norm().clamp(-1.0, 1.0); // 负压力 = 反方向，前端不给但参数域允许
    let shift = STROKE_MAX_SHIFT * k;
    // 单点笔当一条零长线段：投影恒落在该点上，与标量版的单点分支同一个算式
    let segs: Vec<([f32; 2], [f32; 2])> = if pts.len() == 1 {
        vec![(pts[0], pts[0])]
    } else {
        pts.windows(2).map(|d| (d[0], d[1])).collect()
    };
    // 快照照旧留整幅：位移上限 0.45r 落在带 r 外扩的 span 内，取样范围用不到全图，
    // 但 Pad::Edge 的夹边是按整幅算的——按 span 裁会让靠边那一圈取到不同的值
    let src = cur.px.clone();
    let base_px = &base.px;
    let (tool, r2) = (s.tool, r * r);
    par_chunks_mut(
        &mut cur.px[span.y0 * w * 4..span.y1 * w * 4],
        BAND_ROWS * w * 4,
        |blk, bi| {
            let gy0 = span.y0 + bi * BAND_ROWS;
            let rows = blk.len() / (w * 4);
            let mut field = vec![0f32; sw * rows * FIELD];
            // 1) 场：每条线段只走自己的外接胶囊，且只落在本带那几行
            for &(a, b) in &segs {
                let cx0 = (a[0].min(b[0]) - r).max(span.x0 as f32).floor() as usize;
                let cx1 = (a[0].max(b[0]) + r)
                    .max(span.x0 as f32)
                    .ceil()
                    .clamp(span.x0 as f32, span.x1 as f32) as usize;
                if cx0 >= cx1 {
                    continue;
                }
                let ry0 = (a[1].min(b[1]) - r).max(gy0 as f32).floor() as usize;
                let ry1 = (a[1].max(b[1]) + r)
                    .max(gy0 as f32)
                    .ceil()
                    .clamp(gy0 as f32, (gy0 + rows) as f32) as usize;
                if ry0 >= ry1 {
                    continue;
                }
                for y in ry0..ry1 {
                    let row_off = (y - gy0) * sw;
                    for x in cx0..cx1 {
                        let o = (row_off + x - span.x0) * FIELD;
                        contribute(&mut field[o..o + FIELD], &a, &b, r2, x as f32, y as f32, tool);
                    }
                }
            }
            // 2) 反向映射：目标像素按场去源图取色
            for dy in 0..rows {
                let y = gy0 + dy;
                let row = &mut blk[dy * w * 4..(dy + 1) * w * 4];
                for fx in 0..sw {
                    let o = (dy * sw + fx) * FIELD;
                    let (x, yy) = ((span.x0 + fx) as f32, y as f32);
                    let (d, wgt) = disp(&field[o..o + FIELD], x, yy, r, tool, shift);
                    if wgt <= 0.0 && tool != Tool::Restore {
                        continue;
                    }
                    let i = (span.x0 + fx) * 4;
                    match tool {
                        Tool::Restore => {
                            // 恢复 = 朝链起点插值，权重就是衰减盘；越靠盘心回得越多
                            let t = wgt.min(1.0);
                            let g = (y * w + span.x0 + fx) * 4;
                            for c in 0..4 {
                                row[i + c] = (src[g + c] as f32 * (1.0 - t) + base_px[g + c] as f32 * t)
                                    .round()
                                    .clamp(0.0, 255.0) as u8;
                            }
                        }
                        _ => {
                            let v = sample(&src, w, h, x - d.0, yy - d.1, Pad::Edge);
                            row[i] = v[0].round().clamp(0.0, 255.0) as u8;
                            row[i + 1] = v[1].round().clamp(0.0, 255.0) as u8;
                            row[i + 2] = v[2].round().clamp(0.0, 255.0) as u8;
                            // alpha 不参与位移（照片这里是满值），但仍按取样结果写回，保证同一条路径
                            row[i + 3] = v[3].round().clamp(0.0, 255.0) as u8;
                        }
                    }
                }
            }
        },
    );
}

/// 一条线段对一格像素的贡献。`acc = [最大权重, 平面 X, 平面 Y, 权重和]`。
fn contribute(acc: &mut [f32], a: &[f32; 2], b: &[f32; 2], r2: f32, x: f32, y: f32, tool: Tool) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 < 1e-9 {
        0.0
    } else {
        (((x - a[0]) * dx + (y - a[1]) * dy) / l2).clamp(0.0, 1.0)
    };
    let (px, py) = (a[0] + t * dx, a[1] + t * dy);
    let d2 = (x - px).powi(2) + (y - py).powi(2);
    if d2 >= r2 {
        return;
    }
    let w = falloff(d2, r2);
    if w > acc[0] {
        acc[0] = w;
    }
    acc[3] += w;
    if tool == Tool::Push {
        let len = l2.sqrt();
        if len > 1e-6 {
            acc[1] += dx / len * w;
            acc[2] += dy / len * w;
        }
    } else {
        acc[1] += px * w;
        acc[2] += py * w;
    }
}

/// 盘内一格的位移与衰减权重。两条纪律，都是早先那版漏掉的：
///
/// - **连续**：方向取各段单位向量的加权平均，盘心取各投影点的加权平均——两者都是像素位置的连续函数。
///   早先按"最近的那一段"硬选一段：相邻两段一切换，方向与盘心就跳一档，而切换线正好沿笔画走，
///   拖一道笔就留下一串裂缝（液化"把图片弄断裂"就是它）。
/// - **有界**：位移上限 = 半径 × `STROKE_MAX_SHIFT` × 权重，与线段被抽稀成多长无关。
///   早先推挤用的是整段向量，而直线轨迹入库前会被 Douglas–Peucker 压成一两个点——
///   一段几百像素的位移直接超出盘外，采样咬到自己刚写的那一圈像素。
fn disp(acc: &[f32], x: f32, y: f32, r: f32, tool: Tool, shift: f32) -> ((f32, f32), f32) {
    let (wmax, wsum) = (acc[0], acc[3]);
    if wmax <= 0.0 || wsum <= 0.0 {
        return ((0.0, 0.0), 0.0);
    }
    let reach = r * shift * wmax;
    match tool {
        // 推挤：沿加权切向，长度只由半径与压力说了算
        Tool::Push => {
            let n = (acc[1] * acc[1] + acc[2] * acc[2]).sqrt();
            if n < 1e-6 {
                return ((0.0, 0.0), wmax);
            }
            ((acc[1] / n * reach, acc[2] / n * reach), wmax)
        }
        // 收缩 / 膨胀 / 恢复：位移 = (p − 盘心) × 系数。盘心是折线上各投影点的加权平均（连续），
        // 所以盘心那一点不动、盘缘由衰减收回 0，最大量落在 0.29r 那一圈，全程没有奇点。
        t => {
            let (qx, qy) = (acc[1] / wsum, acc[2] / wsum);
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

/// 标量参考实现：单像素扫完全部线段。生产路径走 `apply_stroke` 的按段累积，
/// 两边共用 `contribute` 与 `disp`，所以这里同时是那套场的定义。
#[cfg(test)]
pub(super) fn field_at(pts: &[[f32; 2]], r: f32, x: f32, y: f32, tool: Tool, shift: f32) -> ((f32, f32), f32) {
    let (mut acc, r2) = ([0f32; FIELD], r * r);
    if pts.len() == 1 {
        contribute(&mut acc, &pts[0], &pts[0], r2, x, y, tool);
    } else {
        for j in 0..pts.len() - 1 {
            contribute(&mut acc, &pts[j], &pts[j + 1], r2, x, y, tool);
        }
    }
    disp(&acc, x, y, r, tool, shift)
}
