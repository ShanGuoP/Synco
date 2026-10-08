//! 人脸相关的纯几何/张量函数：非极大值抑制、letterbox 坐标往返、由检出结果推建议框。
//! 模型推理本身在 server 里（tract 是有状态会话，不属于纯函数层），
//! 这里只负责"张量已经拿到手之后"的部分，因此可以脱离模型单测。

/// 一张检出的人脸：框 + 五个关键点（眉间/左眼/右眼/鼻尖/嘴角中），坐标是像素
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    pub points: [[f32; 2]; 5],
}

impl FaceBox {
    pub fn area(&self) -> f32 {
        (self.w.max(0.0) * self.h.max(0.0)).max(1.0)
    }

    /// 关键点缺失时用框中心顶替，调用方就不必处处判空
    pub fn center(&self) -> [f32; 2] {
        [self.x + self.w * 0.5, self.y + self.h * 0.5]
    }
}

fn iou(a: &FaceBox, b: &FaceBox) -> f32 {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }
    let inter = (x1 - x0) * (y1 - y0);
    inter / (a.area() + b.area() - inter).max(1e-6)
}

/// 按分数降序做非极大值抑制。阈值给 0.3 是 YuNet 系常用的那档：
/// 再松会把同一张脸的两层框留下，再紧会吃掉侧偏的框。
pub fn nms(faces: &[FaceBox], thr: f32) -> Vec<FaceBox> {
    let mut idx: Vec<usize> = (0..faces.len()).collect();
    idx.sort_by(|&i, &j| faces[j].score.total_cmp(&faces[i].score));
    let mut keep: Vec<FaceBox> = Vec::new();
    let mut suppressed = vec![false; faces.len()];
    for &i in &idx {
        if suppressed[i] {
            continue;
        }
        let f = &faces[i];
        keep.push(*f);
        for &j in &idx {
            if j != i && !suppressed[j] && iou(f, &faces[j]) > thr {
                suppressed[j] = true;
            }
        }
    }
    keep
}

/// letterbox：等比缩放后居中放进 `side×side`，返回 `(缩放, 左偏移, 上偏移)`。
/// 关键点模型都要求方形输入，直接拉伸会把脸型拽扁。
pub fn letterbox(iw: usize, ih: usize, side: usize) -> (f32, f32, f32) {
    let s = side as f32 / iw.max(ih).max(1) as f32;
    let ox = (side as f32 - iw as f32 * s) * 0.5;
    let oy = (side as f32 - ih as f32 * s) * 0.5;
    (s, ox, oy)
}

/// 归一化点 → letterbox 画布像素（模型输入的坐标约定）
pub fn to_input(p: [f32; 2], w: usize, h: usize, lb: (f32, f32, f32)) -> [f32; 2] {
    let (s, ox, oy) = lb;
    [p[0] * w as f32 * s + ox, p[1] * h as f32 * s + oy]
}

/// letterbox 画布像素 → 原图像素（反变换，越界不夹，由调用方决定怎么用）
pub fn from_input(v: [f32; 2], w: usize, h: usize, lb: (f32, f32, f32)) -> [f32; 2] {
    let (s, ox, oy) = lb;
    [(v[0] - ox) / s / w.max(1) as f32, (v[1] - oy) / s / h.max(1) as f32]
}

/// F1 的构图建议：给定画面尺寸与人脸框，给出"居中"和"三分法"两个裁切框（归一化）。
/// 目标是人像占画面高度约 0.42（证件照到头像之间的手感），并保证脸在框内。
pub fn crop_suggestion(fw: usize, fh: usize, face: &FaceBox, ratio: f32) -> [f32; 4] {
    let ratio = if ratio.is_finite() && ratio > 0.05 { ratio } else { 1.0 };
    // 以脸高反推目标框高，再按宽高比定框宽
    let th = (face.h / 0.42).min(fh as f32);
    let tw = (th * ratio).min(fw as f32);
    let th = (tw / ratio).min(fh as f32);
    let cx = face.center()[0];
    // 竖构图走三分法的下两格，横构图居中——都要求脸完整落在框内
    let (mut x, mut y) = if ratio < 1.0 { (cx - tw * 0.5, face.y + face.h * 0.55 - th * 0.5) } else { (cx - tw * 0.5, face.y + face.h * 0.5 - th * 0.5) };
    x = x.clamp(0.0, (fw as f32 - tw).max(0.0));
    y = y.clamp(0.0, (fh as f32 - th).max(0.0));
    [x / fw as f32, y / fh as f32, tw / fw as f32, th / fh as f32]
}

/// F2 的自动美颜区：脸框外扩一圈、扣掉眼与嘴附近的圆。
/// 返回与 `img` 同尺寸的 alpha 平面（255 = 参与磨皮）。
pub fn beauty_region(w: usize, h: usize, faces: &[FaceBox]) -> stitch_core::Alpha {
    let mut a = stitch_core::Alpha::new(w, h);
    for f in faces {
        let pad_x = f.w * 0.08;
        let pad_y = f.h * 0.08;
        let x0 = (f.x - pad_x).max(0.0) as usize;
        let y0 = (f.y - pad_y).max(0.0) as usize;
        let x1 = ((f.x + f.w + pad_x).min(w as f32 - 1.0)) as usize;
        let y1 = ((f.y + f.h + pad_y).min(h as f32 - 1.0)) as usize;
        // 眼/嘴的排除半径按五点间距估：没有点就退化成整框
        let ex: Vec<([f32; 2], f32)> = match (f.points[1], f.points[2], f.points[4]) {
            (l, r, m) if l[0] != 0.0 || r[0] != 0.0 || m[0] != 0.0 => {
                let d = (((r[0] - l[0]).powi(2) + (r[1] - l[1]).powi(2)).sqrt()).max(1.0);
                vec![(l, d * 0.42), (r, d * 0.42), (m, d * 0.55)]
            }
            _ => Vec::new(),
        };
        for y in y0..=y1 {
            for x in x0..=x1 {
                if ex.iter().any(|(c, rad)| ((x as f32 - c[0]).powi(2) + (y as f32 - c[1]).powi(2)) < rad * rad) {
                    continue;
                }
                a.set(x, y, 255);
            }
        }
    }
    a
}

/// 由稠密关键点折出一键塑形要的那组控制点。
///
/// 不依赖任何"第几号点是下颌"这类拓扑表——那类编号换一份导出就全错。
/// 这里只用几何：下颌线是点云每列最靠下的那些点，颏点是整团里最靠下的那个，
/// 眼圈是围绕虹膜的一圈，鼻翼是鼻尖左右两侧最远的两个。粗一点，但换模型也不塌。
///
/// `points` 与 `hint` 都用归一化坐标（按各自轴）。`hint` 是粗定位给的五点，
/// 用来认哪只是左眼、哪只是右眼、鼻尖在哪。
pub fn shape_from_landmarks(points: &[[f32; 2]], hint: &[[f32; 2]; 5]) -> Option<crate::warp::FaceShape> {
    if points.len() < 24 {
        return None; // 点太稀，边界与圈都给不准，宁可不做自动变形
    }
    if !points.iter().all(|p| p[0].is_finite() && p[1].is_finite()) {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in points {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    if !(x1 > x0) || !(y1 > y0) {
        return None;
    }
    let eye_dist = ((hint[2][0] - hint[1][0]).powi(2) + (hint[2][1] - hint[1][1]).powi(2)).sqrt();
    if !(eye_dist > 1e-3) {
        return None;
    }
    // 下颌线：横向切 24 格，每格取最靠下的那个点，再按 x 排回去
    const BINS: usize = 24;
    let mut jaw: Vec<[f32; 2]> = Vec::with_capacity(BINS);
    for b in 0..BINS {
        let a = x0 + (x1 - x0) * (b as f32 / BINS as f32);
        let z = x0 + (x1 - x0) * ((b + 1) as f32 / BINS as f32);
        let mut best: Option<[f32; 2]> = None;
        for p in points {
            if p[0] >= a && p[0] <= z && best.map(|q| p[1] > q[1]).unwrap_or(true) {
                best = Some(*p);
            }
        }
        if let Some(v) = best {
            jaw.push(v);
        }
    }
    if jaw.len() < 6 {
        return None;
    }
    let chin = *points.iter().max_by(|a, b| a[1].total_cmp(&b[1]))?;
    let ring = |c: [f32; 2]| {
        let r = eye_dist * 0.42;
        let mut v: Vec<[f32; 2]> = points
            .iter()
            .filter(|p| ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt() <= r)
            .copied()
            .collect();
        // 按极角排成一圈：控制点对要的是顺序，不是集合
        v.sort_by(|a, b| {
            let aa = (a[1] - c[1]).atan2(a[0] - c[0]);
            let bb = (b[1] - c[1]).atan2(b[0] - c[0]);
            aa.total_cmp(&bb)
        });
        thin(&v, 10)
    };
    let (eye_l, eye_r) = (ring(hint[1]), ring(hint[2]));
    if eye_l.len() < 3 || eye_r.len() < 3 {
        return None;
    }
    // 鼻翼：以鼻尖为中心，横向 0.2~0.9 个眼距、纵向 ±0.5 个眼距里最左与最右的两个
    let tip = hint[3];
    let mut l: Option<[f32; 2]> = None;
    let mut r: Option<[f32; 2]> = None;
    for p in points {
        let (dx, dy) = (p[0] - tip[0], p[1] - tip[1]);
        if dy.abs() > eye_dist * 0.5 || dx.abs() > eye_dist * 0.9 || dx.abs() < eye_dist * 0.2 {
            continue;
        }
        if l.map(|q| p[0] < q[0]).unwrap_or(true) {
            l = Some(*p);
        }
        if r.map(|q| p[0] > q[0]).unwrap_or(true) {
            r = Some(*p);
        }
    }
    let (wing_l, wing_r) = (l?, r?);
    Some(crate::warp::FaceShape {
        box_norm: [x0, y0, x1 - x0, y1 - y0],
        jaw,
        eye_l,
        eye_r,
        iris_l: hint[1],
        iris_r: hint[2],
        nose_wing_l: wing_l,
        nose_wing_r: wing_r,
        chin,
    })
}

/// 等距抽稀到不超过 `want` 个（保首尾）
fn thin(v: &[[f32; 2]], want: usize) -> Vec<[f32; 2]> {
    if v.len() <= want || v.len() < 2 {
        return v.to_vec();
    }
    let step = (v.len() - 1) as f32 / (want - 1) as f32;
    (0..want).map(|i| v[(i as f32 * step).round() as usize]).collect()
}

/// 粗定位五点的存储形状：`box` 是 `[x,y,w,h]`，`points` 是归一化的点列
pub fn face_json(box_norm: [f32; 4], points: &[[f32; 2]], score: f32) -> String {
    let o = serde_json::json!({ "box": box_norm, "points5": points, "score": score });
    o.to_string()
}

/// 把库里存的那行脸记录解回 `[x,y,w,h]` + 五点。形状不对就 None，不猜。
pub fn face_from_json(raw: &str) -> Option<([f32; 4], Vec<[f32; 2]>)> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let b: [f32; 4] = v.get("box")?.as_array()?.iter().take(4).map(|x| x.as_f64().unwrap_or(0.0) as f32).collect::<Vec<_>>().try_into().ok()?;
    let pts = v
        .get("points5")?
        .as_array()?
        .iter()
        .filter_map(|p| {
            let a = p.as_array()?;
            if a.len() < 2 {
                return None;
            }
            Some([a[0].as_f64()? as f32, a[1].as_f64()? as f32])
        })
        .collect();
    Some((b, pts))
}

/// 一张图的关键点行 → 归一化点列。行数不够直接 None（与 [`shape_from_landmarks`] 同一判据）。
pub fn points_from_rows(rows: &[Vec<[f32; 2]>]) -> Option<Vec<[f32; 2]>> {
    let n: usize = rows.iter().map(|r| r.len()).sum();
    if n < 24 {
        return None;
    }
    Some(rows.iter().flatten().copied().collect())
}

/// 关键点 JSON（一串 `[x,y]`）→ 点列
pub fn parse_points(raw: &str) -> Vec<[f32; 2]> {
    serde_json::from_str::<Vec<[f32; 2]>>(raw).unwrap_or_default()
}

/// 构图建议的候选比例（F1 的宽高比那排按钮同一个表）
pub const RATIOS: &[(&str, f32)] = &[("free", 0.0), ("1:1", 1.0), ("4:3", 4.0 / 3.0), ("3:4", 3.0 / 4.0), ("16:9", 16.0 / 9.0), ("3:2", 1.5), ("9:16", 9.0 / 16.0)];

#[cfg(test)]
mod shape_tests {
    use super::*;

    /// 合成一张"脸"：椭圆轮廓 200 点当外皮，两圈各 14 点当眼圈，鼻尖左右各几点当鼻翼，
    /// 再加一小片内部散点。真实关键点比这密得多，这里要的是几何判据而不是密度。
    fn mesh() -> (Vec<[f32; 2]>, [[f32; 2]; 5]) {
        let mut v = Vec::new();
        for i in 0..200 {
            let t = i as f32 * std::f32::consts::PI * 2.0 / 200.0;
            v.push([0.5 + 0.20 * t.cos(), 0.5 + 0.28 * t.sin()]);
        }
        for c in [[0.44f32, 0.36f32], [0.56, 0.36]] {
            for i in 0..14 {
                let t = i as f32 * std::f32::consts::PI * 2.0 / 14.0;
                v.push([c[0] + 0.03 * t.cos(), c[1] + 0.018 * t.sin()]);
            }
        }
        // 鼻翼：离鼻尖横向 0.2~0.9 个眼距（眼距 0.12 → 0.024~0.108）
        for dx in [-0.045f32, -0.038, 0.038, 0.045] {
            v.push([0.50 + dx, 0.485]);
        }
        for i in 0..60 {
            v.push([0.35 + (i as f32) * 0.005, 0.42 + ((i % 7) as f32) * 0.004]);
        }
        let hint = [[0.5, 0.30], [0.44, 0.36], [0.56, 0.36], [0.50, 0.48], [0.50, 0.62]];
        (v, hint)
    }

    #[test]
    fn 点太稀就不给形状() {
        let pts: Vec<[f32; 2]> = (0..8).map(|i| [i as f32 * 0.01, 0.5]).collect();
        assert!(shape_from_landmarks(&pts, &[[0.0, 0.0]; 5]).is_none());
    }

    #[test]
    fn 下颌线在点云底部且左右有序() {
        let (v, hint) = mesh();
        let s = shape_from_landmarks(&v, &hint).unwrap();
        assert!(s.jaw.len() >= 6);
        for w in s.jaw.windows(2) {
            assert!(w[0][0] <= w[1][0] + 1e-6, "下颌线没按 x 排：{:?} → {:?}", w[0], w[1]);
        }
        // 每格取的都是最靠下的点：椭圆下缘 y ≈ 0.5 + 0.28·sin(θ)，中心最低 0.78
        let mid = s.jaw[s.jaw.len() / 2];
        assert!(mid[1] > 0.7, "中间那格不该落在脸上部：{mid:?}");
        assert!(s.chin[1] >= mid[1] - 1e-3, "颏点应当不高于下颌线中段");
    }

    #[test]
    fn 眼圈围住虹膜() {
        let (v, hint) = mesh();
        let s = shape_from_landmarks(&v, &hint).unwrap();
        assert!(s.eye_l.len() >= 3 && s.eye_r.len() >= 3);
        for c in [s.iris_l, s.iris_r] {
            let ring = if (c[0] - hint[1][0]).abs() < 1e-3 { &s.eye_l } else { &s.eye_r };
            for p in ring {
                let d = ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt();
                assert!(d <= 0.1, "眼圈跑到外面去了：{p:?} 距 {d}");
            }
        }
    }

    #[test]
    fn 鼻翼分列鼻尖两侧() {
        let (v, hint) = mesh();
        let s = shape_from_landmarks(&v, &hint).unwrap();
        assert!(s.nose_wing_l[0] < s.nose_wing_r[0], "左右鼻翼反了：{:?} / {:?}", s.nose_wing_l, s.nose_wing_r);
    }

    #[test]
    fn 人脸框包住全部点() {
        let (v, hint) = mesh();
        let s = shape_from_landmarks(&v, &hint).unwrap();
        let [x, y, w, h] = s.box_norm;
        let box_txt = format!("{:?}", s.box_norm);
        for p in &v {
            assert!(p[0] >= x - 1e-6 && p[0] <= x + w + 1e-6 && p[1] >= y - 1e-6 && p[1] <= y + h + 1e-6, "{p:?} 出了 {box_txt}");
        }
    }

    #[test]
    fn 五点与关键点的存取的往返() {
        let raw = face_json([0.1, 0.2, 0.3, 0.4], &[[0.5, 0.5]; 5], 0.9);
        let (b, pts) = face_from_json(&raw).unwrap();
        assert_eq!(b, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(pts.len(), 5);
        assert!(face_from_json("{\"box\":[1,2]}").is_none(), "形状不对的不该硬解");
        assert_eq!(parse_points("[[0.1,0.2],[0.3,0.4]]").len(), 2);
        assert!(parse_points("坏数据").is_empty());
    }

    #[test]
    fn 抽稀保首尾() {
        let v: Vec<[f32; 2]> = (0..100).map(|i| [i as f32, 0.0]).collect();
        let t = thin(&v, 10);
        assert_eq!(t.len(), 10);
        assert_eq!(t[0], v[0]);
        assert_eq!(t[9], v[99]);
        assert_eq!(thin(&[[1.0, 2.0]], 4).len(), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fb(x: f32, y: f32, w: f32, h: f32, score: f32) -> FaceBox {
        FaceBox { x, y, w, h, score, points: [[x + w * 0.5, y + h * 0.2], [x + w * 0.35, y + h * 0.4], [x + w * 0.65, y + h * 0.4], [x + w * 0.5, y + h * 0.6], [x + w * 0.5, y + h * 0.82]] }
    }

    #[test]
    fn 抑制留下高分框() {
        let a = fb(0.0, 0.0, 100.0, 100.0, 0.9);
        let b = fb(4.0, 2.0, 100.0, 100.0, 0.6); // 和 a 高度重叠
        let c = fb(400.0, 400.0, 100.0, 100.0, 0.8);
        let kept = nms(&[b, a, c], 0.3);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].score, 0.9, "高分的要留下");
        assert_eq!(kept[1].x, 400.0);
    }

    #[test]
    fn 不重叠的框都留下() {
        let list = [fb(0.0, 0.0, 50.0, 50.0, 0.5), fb(200.0, 0.0, 50.0, 50.0, 0.4), fb(0.0, 200.0, 50.0, 50.0, 0.3)];
        assert_eq!(nms(&list, 0.3).len(), 3);
        assert!(nms(&[], 0.3).is_empty());
    }

    #[test]
    fn letterbox_往返一致() {
        let (w, h) = (640usize, 480usize);
        let lb = letterbox(w, h, 192);
        // 长边铺满、短边居中留边
        assert!((lb.0 * w.max(h) as f32 - 192.0).abs() < 1e-3);
        assert!(lb.2 > 0.0 && lb.1 < 1e-3, "横图应该上下留边：{lb:?}");
        for p in [[0.0f32, 0.0], [1.0, 1.0], [0.37, 0.62]] {
            let back = from_input(to_input(p, w, h, lb), w, h, lb);
            assert!((back[0] - p[0]).abs() < 1e-4 && (back[1] - p[1]).abs() < 1e-4, "{p:?} → {back:?}");
        }
    }

    #[test]
    fn 方形输入不移位() {
        let lb = letterbox(200, 200, 192);
        assert!(lb.1.abs() < 1e-4 && lb.2.abs() < 1e-4, "{lb:?}");
        assert!((lb.0 - 192.0 / 200.0).abs() < 1e-5);
    }

    #[test]
    fn 建议框不越界且含住脸() {
        for ratio in [1.0f32, 0.75, 1.33, 0.5625] {
            let face = fb(300.0, 200.0, 240.0, 300.0, 0.9);
            let s = crop_suggestion(1200, 1600, &face, ratio);
            assert!(s[0] >= 0.0 && s[1] >= 0.0 && s[2] > 0.0 && s[3] > 0.0, "{ratio} → {s:?}");
            assert!(s[0] + s[2] <= 1.0001 && s[1] + s[3] <= 1.0001, "{ratio} → {s:?}");
            let px = [s[0] * 1200.0, s[1] * 1600.0, s[2] * 1200.0, s[3] * 1600.0];
            assert!(px[0] <= face.x + 1.0 && px[0] + px[2] >= face.x + face.w - 1.0, "横向没含住脸：{px:?}");
            assert!(px[1] <= face.y + 1.0 && px[1] + px[3] >= face.y + face.h - 1.0, "纵向没含住脸：{px:?}");
            // 宽高比要守住
            assert!((px[2] / px[3] - ratio).abs() < 0.02 * ratio, "{ratio} → {}", px[2] / px[3]);
        }
    }

    #[test]
    fn 建议框贴边时也被夹回画面() {
        let face = fb(0.0, 0.0, 100.0, 120.0, 0.9);
        let s = crop_suggestion(200, 200, &face, 1.0);
        assert!(s[0] >= 0.0 && s[0] + s[2] <= 1.0 + 1e-6, "{s:?}");
        assert!(s[1] >= 0.0 && s[1] + s[3] <= 1.0 + 1e-6, "{s:?}");
        // 非法比例退化成正方，不能把框算成 0
        let s = crop_suggestion(200, 200, &face, 0.0);
        assert!(s[2] > 0.0 && (s[2] - s[3]).abs() < 1e-3, "{s:?}");
    }

    #[test]
    fn 自动美颜区扣掉眼嘴() {
        let face = fb(20.0, 20.0, 60.0, 80.0, 0.9);
        let a = beauty_region(200, 200, &[face]);
        // 额头（框内、远离五官）要参与
        assert_eq!(a.get(50, 24), 255);
        // 左眼位置要排除
        let eye = face.points[1];
        assert_eq!(a.get(eye[0] as usize, eye[1] as usize), 0, "眼睛没被扣掉");
        // 框外不参与
        assert_eq!(a.get(4, 4), 0);
        assert_eq!(a.get(190, 190), 0);
        // 没有脸时全空，调用方据此退回"全图"
        assert_eq!(beauty_region(40, 40, &[]).max(), 0);
    }

    #[test]
    fn 没有关键点时美颜区退化成整框() {
        let mut f = fb(20.0, 20.0, 60.0, 80.0, 0.9);
        f.points = [[0.0, 0.0]; 5];
        let a = beauty_region(200, 200, &[f]);
        let eye = [50.0f32, 52.0];
        assert_eq!(a.get(eye[0] as usize, eye[1] as usize), 255, "五点全零时不该乱扣洞");
    }
}
