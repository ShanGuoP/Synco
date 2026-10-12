//! 人脸控制点与 MLS 网格变形；坐标和保护蒙版同处图像域。
use crate::ops::Auto;
use crate::px::{sample, Pad};
use moving_least_squares::deform_similarity;
use px_core::Rgba;
use super::{MLS_GRID, MLS_SCOPE};
/// 一键塑形需要的关键点组（归一化坐标）。由人脸关键点链路准备好交进来，
/// 内核不认识模型，只认识这些点。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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
    // Anatomical cheek width remains stable after rotations and reflections.
    let face_w = shape.jaw.first().zip(shape.jaw.last()).map(|(a,b)| {
        let (ax,ay)=px(*a); let (bx,by)=px(*b);
        ((bx-ax).powi(2)+(by-ay).powi(2)).sqrt()
    }).unwrap_or(shape.box_norm[2]*w as f32).max(1.0);

    // 瘦脸：下颌线上的点沿"外轮廓法线"内收
    let slim = auto.face_slim.norm() * 0.16;
    if slim.abs() > 1e-4 && shape.jaw.len() >= 3 {
        let n = shape.jaw.len();
        for i in 0..n {
            let (a, b, c) = (shape.jaw[i.saturating_sub(1)], shape.jaw[i], shape.jaw[(i + 1).min(n - 1)]);
            let (ax, ay) = px(a);
            let (cx, cy) = px(c);
            let (tx, ty) = (cx - ax, cy - ay);
            let l = (tx * tx + ty * ty).sqrt().max(1e-6);
            // 法线取 (+ty, −tx)：下颌线自左向右时它指向脸外
            let (mut nx, mut ny) = (ty / l, -tx / l);
            let (x, y) = px(b);
            // Reflections reverse contour winding. Choose the normal toward the
            // nose, so positive slim remains inward after a flip or rotation.
            let (mx, my) = px(shape.nose_wing_l);
            let (rx, ry) = px(shape.nose_wing_r);
            if nx * ((mx + rx) * 0.5 - x) + ny * ((my + ry) * 0.5 - y) < 0.0 {
                nx = -nx; ny = -ny;
            }
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
            // Keep the two wings from crossing even at the end of the slider.
            let step = (nose * face_w).clamp(-l*0.7, l*0.7);
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
    if !p.is_empty() {
        // Shared points (jaw tip + chin) receive both requested displacements.
        let mut merged_p: Vec<(f32,f32)> = Vec::new();
        let mut merged_q: Vec<(f32,f32)> = Vec::new();
        for (a,b) in p.iter().zip(&q) {
            if let Some(i)=merged_p.iter().position(|v| (v.0-a.0).abs()<0.1 && (v.1-a.1).abs()<0.1) {
                merged_q[i].0+=b.0-a.0; merged_q[i].1+=b.1-a.1;
            } else { merged_p.push(*a); merged_q.push(*b); }
        }
        p=merged_p; q=merged_q;
        let [x,y,bw,bh]=shape.box_norm;
        let pad=(MLS_SCOPE-1.0)*0.5;
        for pt in [[x-bw*pad,y-bh*pad],[x+bw*0.5,y-bh*pad],[x+bw*(1.0+pad),y-bh*pad],
            [x-bw*pad,y+bh*0.5],[x+bw*(1.0+pad),y+bh*0.5],
            [x-bw*pad,y+bh*(1.0+pad)],[x+bw*0.5,y+bh*(1.0+pad)],[x+bw*(1.0+pad),y+bh*(1.0+pad)]] {
            let v=px(pt); p.push(v); q.push(v);
        }
    }
    (p, q)
}

/// 一键塑形：在人脸框外扩 1.2 倍的局部域里跑稀疏网格 MLS，域外恒等。
/// 全图 dense 在 24MP 上是分钟级，局部域 + 网格把问题缩到万级节点。
pub fn mls(img: &Rgba, auto: &Auto, shape: &FaceShape) -> Rgba {
    if auto.is_empty() || shape.box_norm[2] <= 0.0 || shape.box_norm[3] <= 0.0 {
        return img.clone();
    }
    let (p, q) = control_pairs(auto, shape, img.w, img.h);
    mls_pairs(img, shape, &p, &q, None)
}

/// 精细塑形复用同一个 MLS 实现。保护图是源图域的位移权重，零权重处逐位保留。
pub(crate) fn mls_pairs(img: &Rgba, shape: &FaceShape, p: &[(f32, f32)], q: &[(f32, f32)], protection: Option<&px_core::Alpha>) -> Rgba {
    let mut out = img.clone();
    let (w, h) = (img.w, img.h);
    if w==0 || h==0 || shape.box_norm[2]<=0.0 || shape.box_norm[3]<=0.0 || !shape.box_norm.iter().all(|v|v.is_finite()) {return out;}
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
            let (dx, dy) = mls_at(p, q, x, y);
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
            let edge = (x-x0).min(x1-x).min(y-y0).min(y1-y) as f32;
            let feather = ((x1-x0).min(y1-y0) as f32 * 0.08).max(1.0);
            let fade = (edge/feather).clamp(0.0,1.0);
            let fade = fade*fade*(3.0-2.0*fade) * protection.map(|m| m.get(x,y) as f32 / 255.0).unwrap_or(1.0);
            if fade == 0.0 { continue; }
            let v = sample(src, w, h, x as f32 - dx*fade, y as f32 - dy*fade, Pad::Edge);
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
