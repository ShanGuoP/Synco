//! 人脸控制点与 MLS 网格变形；坐标和保护蒙版同处图像域。
use crate::ops::Auto;
use crate::px::{sample, Pad};
use moving_least_squares::deform_similarity;
use px_core::Rgba;
use super::{MLS_GRID, MLS_SCOPE};

/// Fine control near zero, full manual range at the ends; presets stay gentle.
pub(crate) fn shape_strength(value: f32) -> f32 {
    let v=value.clamp(-1.0,1.0);
    v.signum()*(v.abs()*0.35+v*v*0.65)
}
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

/// 由 `Auto` 滑杆与关键点生成 MLS 控制点对（像素坐标）；大眼由独立径向场处理。
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
    let slim = shape_strength(auto.face_slim.norm()) * 0.075;
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
            // Keep the chin stable; slimming belongs mainly to the cheeks.
            let t = i as f32 / (n - 1) as f32;
            let taper = 0.25 + 0.75 * (2.0 * t - 1.0).abs();
            q.push((x + nx * slim * face_w * taper, y + ny * slim * face_w * taper));
        }
    }

    // 瘦鼻：两翼朝鼻梁中心内收
    let nose = shape_strength(auto.nose_slim.norm()) * 0.30;
    if nose.abs() > 1e-4 {
        let (lx, ly) = px(shape.nose_wing_l);
        let (rx, ry) = px(shape.nose_wing_r);
        let (mx, my) = ((lx + rx) * 0.5, (ly + ry) * 0.5);
        for (x, y) in [(lx, ly), (rx, ry), ((lx + mx) * 0.5, (ly + my) * 0.5), ((rx + mx) * 0.5, (ry + my) * 0.5)] {
            let (dx, dy) = (mx - x, my - y);
            p.push((x, y));
            q.push((x + dx * nose, y + dy * nose));
        }
    }

    // 下巴：沿"鼻中点 → 颏点"这条轴前后推
    let chin = shape_strength(auto.chin.norm()) * 0.065;
    if chin.abs() > 1e-4 {
        let (cx, cy) = px(shape.chin);
        let (mx, my) = px(shape.nose_wing_l);
        let (nx, ny) = px(shape.nose_wing_r);
        let (dx, dy) = (cx - (mx + nx) * 0.5, cy - (my + ny) * 0.5);
        let l = (dx * dx + dy * dy).sqrt().max(1e-6);
        let step = chin * face_w;
        for t in [0.0f32, 0.5, 1.0] {
            p.push((cx - dx * t, cy - dy * t));
            q.push((cx - dx * t + dx / l * step * (1.0-t), cy - dy * t + dy / l * step * (1.0-t)));
        }
    }
    if !p.is_empty() {
        // Unedited features anchor the local field instead of drifting along
        // with a neighbouring cheek, nose or eye adjustment.
        let mut fixed = vec![shape.iris_l, shape.iris_r];
        fixed.extend(&shape.eye_l); fixed.extend(&shape.eye_r);
        if auto.nose_slim.0 == 0 { fixed.extend([shape.nose_wing_l, shape.nose_wing_r]); }
        for pt in fixed {
            let a=px(pt);
            if !p.iter().any(|v| (v.0-a.0).abs()<0.1 && (v.1-a.1).abs()<0.1) { p.push(a); q.push(a); }
        }
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

/// 一键塑形：局部 MLS 与大眼径向场合成，只重采样一次，域外恒等。
/// 全图 dense 在 24MP 上是分钟级，局部域 + 网格把问题缩到万级节点。
pub fn mls(img: &Rgba, auto: &Auto, shape: &FaceShape) -> Rgba {
    if auto.is_empty() || shape.box_norm[2] <= 0.0 || shape.box_norm[3] <= 0.0 {
        return img.clone();
    }
    let (p, q) = control_pairs(auto, shape, img.w, img.h);
    mls_pairs(img, shape, &p, &q, auto.eye_big.norm(), None)
}

/// 精细塑形复用同一个 MLS 实现。保护图是源图域的位移权重，零权重处逐位保留。
pub(crate) fn mls_pairs(img: &Rgba, shape: &FaceShape, p: &[(f32, f32)], q: &[(f32, f32)], eye_strength: f32, protection: Option<&px_core::Alpha>) -> Rgba {
    let mut out = img.clone();
    let (w, h) = (img.w, img.h);
    if w==0 || h==0 || shape.box_norm[2]<=0.0 || shape.box_norm[3]<=0.0 || !shape.box_norm.iter().all(|v|v.is_finite()) {return out;}
    let eyes=eye_fields(shape,w,h,eye_strength);
    if p.len() < 2 && eyes.is_empty() {
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
    // Small preview faces need more than one or two nodes across an eyelid.
    let grid = (shape.box_norm[2]*w as f32*0.015).clamp(2.0, MLS_GRID as f32) as usize;
    let gw = (x1 - x0) / grid + 2;
    let gh = (y1 - y0) / grid + 2;
    // 节点位移场
    let mut field = vec![[0f32; 2]; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let x = x0 as f32 + gx as f32 * grid as f32;
            let y = y0 as f32 + gy as f32 * grid as f32;
            // Destination -> source MLS. Subtracting a forward field evaluated
            // at the destination is only a small-displacement approximation.
            let (dx, dy) = if p.len()>=2 {mls_at(q, p, x, y)} else {(0.0,0.0)};
            field[gy * gw + gx] = [dx, dy];
        }
    }
    let src = &img.px;
    for y in y0..=y1 {
        for x in x0..=x1 {
            // 位移按网格双线性插值：形变本身已经是平滑的，插值不会额外糊
            let fx = (x as f32 - x0 as f32) / grid as f32;
            let fy = (y as f32 - y0 as f32) / grid as f32;
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
            let mut source=(x as f32+dx,y as f32+dy);
            // Compose source-space radial eye scaling with the inverse MLS.
            // Iris, eyelid and surrounding texture follow one continuous field.
            for eye in &eyes {source=eye.source_at(source);}
            let v = sample(src, w, h, x as f32+(source.0-x as f32)*fade, y as f32+(source.1-y as f32)*fade, Pad::Edge);
            let i = (y * w + x) * 4;
            out.px[i] = v[0].round().clamp(0.0, 255.0) as u8;
            out.px[i + 1] = v[1].round().clamp(0.0, 255.0) as u8;
            out.px[i + 2] = v[2].round().clamp(0.0, 255.0) as u8;
            out.px[i + 3] = v[3].round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

// Local radial inverse mapping, informed by GPUPixel / CainCamera. Radii use
// pixel-space anatomy; the squared falloff has zero slope at the support edge.
struct EyeField { center:(f32,f32), radius:f32, strength:f32 }
impl EyeField {
    fn source_at(&self, point:(f32,f32))->(f32,f32) {
        let (dx,dy)=(point.0-self.center.0,point.1-self.center.1);
        let r2=(dx*dx+dy*dy)/(self.radius*self.radius);
        if r2>=1.0 {return point;}
        let scale=1.0-self.strength*(1.0-r2).powi(2);
        (self.center.0+dx*scale,self.center.1+dy*scale)
    }
}
fn eye_fields(shape:&FaceShape,w:usize,h:usize,value:f32)->Vec<EyeField> {
    let strength=shape_strength(value)*0.32;
    if strength.abs()<1e-5 {return Vec::new();}
    let px=|v:[f32;2]| (v[0]*w as f32,v[1]*h as f32);
    let (l,r)=(px(shape.iris_l),px(shape.iris_r));
    let separation=((l.0-r.0).powi(2)+(l.1-r.1).powi(2)).sqrt();
    [(&shape.eye_l,l),(&shape.eye_r,r)].into_iter().filter_map(|(ring,center)| {
        let extent=ring.iter().map(|v| {let v=px(*v);((v.0-center.0).powi(2)+(v.1-center.1).powi(2)).sqrt()}).fold(0.0f32,f32::max);
        let radius=(extent*2.4).min(separation*0.48);
        (radius>=1.0 && radius.is_finite()).then_some(EyeField{center,radius,strength})
    }).collect()
}

#[cfg(test)]
mod radial_tests {
    use super::{EyeField,shape_strength};
    #[test]
    fn radial_mapping_is_monotone_with_fixed_center_and_boundary() {
        for strength in [-0.32,0.32] {
            let eye=EyeField{center:(80.0,90.0),radius:40.0,strength};
            assert_eq!(eye.source_at(eye.center),eye.center);
            assert_eq!(eye.source_at((120.0,90.0)),(120.0,90.0));
            assert_eq!(eye.source_at((121.0,90.0)),(121.0,90.0));
            let mut previous=80.0;
            for i in 1..=400 {
                let input=80.0+i as f32*0.1;
                let mapped=eye.source_at((input,90.0)).0;
                assert!(mapped>previous,"radial fold at {input}");previous=mapped;
            }
            let mapped=eye.source_at((90.0,90.0)).0;
            assert_eq!(mapped<90.0,strength>0.0);
        }
    }
    #[test]
    fn slider_response_is_symmetric_monotone_and_has_full_range() {
        assert_eq!(shape_strength(0.0),0.0);
        assert_eq!(shape_strength(1.0),1.0);
        assert_eq!(shape_strength(-1.0),-1.0);
        let mut previous=0.0;
        for i in 1..=100 {
            let t=i as f32/100.0;let v=shape_strength(t);
            assert!(v>previous);assert_eq!(shape_strength(-t),-v);previous=v;
        }
        assert!(shape_strength(0.2)<0.12);
        assert!(shape_strength(0.8)>0.65);
    }
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
