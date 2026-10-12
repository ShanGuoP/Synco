#![cfg(test)]
//! 模块行为与边界回归。
use super::*;
use crate::ops::{Auto, Stroke, Tool, Warp};
use px_core::Rgba;

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
fn eye_only_shaping_is_local_and_does_not_require_mls_controls() {
    let s=shape();
    let img=striped(300,300);
    for value in [-100,100] {
        let a=Auto {eye_big:crate::Slider(value),..Default::default()};
        let (p,q)=control_pairs(&a,&s,300,300);
        assert!(p.is_empty()&&q.is_empty());
        let out=mls(&img,&a,&s);
        assert_ne!(out,img);
        assert_eq!(out.get(150,145),img.get(150,145),"nose stays outside eye support");
        assert_eq!(out.get(0,0),img.get(0,0));
        assert_eq!(out,mls(&img,&a,&s));
    }
}

#[test]
fn eye_slider_resizes_pupil_pixels_in_both_directions() {
    let s=shape();let mut img=Rgba::new(400,400);
    for y in 0..400 {for x in 0..400 {
        let pupil=[(170i32,140i32),(230,140)].into_iter().any(|(cx,cy)| (x as i32-cx).pow(2)+(y as i32-cy).pow(2)<=64);
        img.set(x,y,[if pupil {255} else {0},0,0,255]);
    }}
    let size=|img:&Rgba| (170..195).filter(|&x| img.get(x,140)[0]>=128).count();
    let small=mls(&img,&Auto{eye_big:crate::Slider(-100),..Default::default()},&s);
    let large=mls(&img,&Auto{eye_big:crate::Slider(100),..Default::default()},&s);
    assert!(size(&small)<size(&img),"negative slider must shrink eye interior");
    assert!(size(&large)>=size(&img)+2,"full slider must visibly enlarge eye interior");
    assert_eq!(large.get(170,140),img.get(170,140));
    assert_eq!(large.get(200,140),img.get(200,140),"eyes must not join across the nose");
}

#[test]
fn full_slimming_stays_bounded_and_preserves_unedited_eyes() {
    let s=shape();
    let (p,q)=control_pairs(&Auto {face_slim:crate::Slider(100),..Default::default()},&s,1000,1000);
    let width=(s.jaw.last().unwrap()[0]-s.jaw[0][0])*1000.0;
    for (a,b) in p.iter().zip(&q) {
        assert!(((b.0-a.0).powi(2)+(b.1-a.1).powi(2)).sqrt()<=width*0.075+1e-3);
    }
    for pt in s.eye_l.iter().chain(&s.eye_r) {
        let a=(pt[0]*1000.0,pt[1]*1000.0);
        let i=p.iter().position(|v| *v==a).unwrap(); assert_eq!(q[i],a);
    }
}

#[test]
fn inverse_mls_samples_source_at_displaced_control() {
    let img=striped(120,120);
    // A scale has an exact similarity inverse: q=1.25*p, so q=60 samples p=48.
    let s=FaceShape {box_norm:[0.0,0.0,1.0,1.0],..Default::default()};
    let p=[(24.0,24.0),(48.0,48.0),(72.0,24.0),(24.0,72.0)];
    let q=p.map(|v| (v.0*1.25,v.1*1.25));
    let out=super::face::mls_pairs(&img,&s,&p,&q,0.0,None);
    assert_eq!(out.get(60,60),img.get(48,48));
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
    for (i, (px, py)) in p.iter().take(s.jaw.len()).enumerate() {
        let (qx, qy) = q[i];
        let d = ((qx - px).powi(2) + (qy - py).powi(2)).sqrt();
        assert!(d > 0.0, "第 {i} 组控制点没位移");
    }
    assert!(p.iter().zip(&q).rev().take(8).all(|(a,b)| a==b), "边界锚点必须固定");
}

#[test]
fn slimming_displacements_follow_rotations_and_reflections() {
    let s=shape();
    let auto=Auto { face_slim: crate::Slider(40), ..Default::default() };
    let (p,q)=control_pairs(&auto,&s,300,180);
    let g=crate::ops::Geometry { rotate_deg:90.0,flip_h:true,..Default::default() };
    let mapped=crate::geometry::map_shape(&s,300,180,&g);
    let (rp,rq)=control_pairs(&auto,&mapped,180,300);
    for i in 0..s.jaw.len() {
        let map=|v:(f32,f32)| {
            let v=crate::geometry::map_point([v.0/300.0,v.1/180.0],300,180,&g);
            (v[0]*180.0,v[1]*300.0)
        };
        let (ep,eq)=(map(p[i]),map(q[i]));
        assert!((rp[i].0-ep.0).abs()<1e-3 && (rp[i].1-ep.1).abs()<1e-3);
        assert!((rq[i].0-eq.0).abs()<1e-3 && (rq[i].1-eq.1).abs()<1e-3);
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

/// 位移场从"逐像素扫全线段"改成"逐线段累积到场"之后，输出必须**逐位**不变：
/// 累加顺序同为线段下标递增，浮点求和才落得一样。指纹取自改写前那一版实现。
#[test]
fn 位移场改写前后逐位一致() {
    // (名称, 图, 笔画…, 改写前的输出指纹)
    let sine = |n: usize, y: f32| -> Vec<[f32; 2]> {
        (0..n)
            .map(|i| {
                let t = i as f32 / (n - 1) as f32;
                [0.04 + 0.92 * t, y + 0.012 * (t * 7.0).sin()]
            })
            .collect()
    };
    let striped = striped(320, 240);
    let banded = banded(240, 160);
    let cases: Vec<(&str, &Rgba, Vec<Stroke>, u64)> = vec![
        ("push_sine40", &striped, vec![stroke(Tool::Push, &sine(40, 0.5), 0.08, 60)], 12843966619434218842),
        ("push_sine40_r20", &striped, vec![stroke(Tool::Push, &sine(40, 0.5), 0.20, 100)], 14399672597629467676),
        ("pucker", &striped, vec![stroke(Tool::Pucker, &sine(12, 0.42), 0.05, 100)], 17018847604517427215),
        ("bloat", &striped, vec![stroke(Tool::Bloat, &sine(12, 0.42), 0.03, 40)], 7610260927988065029),
        (
            "restore_套在推挤后",
            &striped,
            vec![stroke(Tool::Push, &sine(40, 0.5), 0.08, 60), stroke(Tool::Restore, &sine(20, 0.52), 0.09, 80)],
            15527378411396461740,
        ),
        ("两点直笔_弱压", &striped, vec![stroke(Tool::Push, &[[0.1, 0.3], [0.9, 0.31]], 0.04, 5)], 8058671356134880944),
        ("单点笔", &striped, vec![stroke(Tool::Pucker, &[[0.5, 0.5]], 0.10, 90)], 14485832776867491059),
        (
            "五笔混合",
            &striped,
            vec![
                stroke(Tool::Push, &sine(30, 0.3), 0.06, 70),
                stroke(Tool::Bloat, &sine(10, 0.5), 0.04, 55),
                stroke(Tool::Pucker, &sine(10, 0.7), 0.05, 80),
                stroke(Tool::Restore, &sine(14, 0.5), 0.07, 60),
                stroke(Tool::Push, &sine(26, 0.8), 0.05, -50),
            ],
            15345631783532399588,
        ),
        ("半径顶满", &banded, vec![stroke(Tool::Push, &sine(24, 0.5), 0.5, 100)], 2040180177096295244),
        ("出界轨迹", &banded, vec![stroke(Tool::Bloat, &[[-0.2, 1.3], [0.5, 1.4], [1.4, 0.9]], 0.18, 90)], 18209119739007652645),
        ("负压力", &banded, vec![stroke(Tool::Push, &sine(18, 0.6), 0.09, -50)], 7977175818157627831),
        ("零压力", &banded, vec![stroke(Tool::Push, &sine(18, 0.6), 0.09, 0)], 18209119739007652645),
    ];
    for (name, img, ss, want) in cases {
        let out = strokes(img, &Warp { strokes: ss, auto: Auto::default() });
        assert_eq!(hash(&out), want, "{name} 的输出变了：位移场改写动了语义");
    }
}

/// 长笔要盖过好几个行带才会走并行路径；这条盯两件事：盘内确实动了、盘外一格都没被带进去。
/// （逐位不变由上一条黄金指纹钉住，这条盯的是切带之后影响范围没溢出）
#[test]
fn 行带并行后作用范围不外溢() {
    let (w, h) = (900, 700);
    let img = striped(w, h);
    let long = stroke(Tool::Push, &[[0.02, 0.5], [0.5, 0.5], [0.98, 0.5]], 0.03, 80);
    let a = strokes(&img, &Warp { strokes: vec![long.clone(), long.clone(), long], auto: Auto::default() });
    // 半径按几何均边算：0.03 × √(900×700) ≈ 24 像素 → 这一笔盖约 6 个行带
    let r = (0.03 * ((w * h) as f32).sqrt()) as i64;
    let yc = (h / 2) as i64;
    let at = |im: &Rgba, x: i64, y: i64| im.px[((y * w as i64 + x) * 4) as usize];
    let mut moved = 0;
    for y in (yc - r)..=(yc + r) {
        for x in 440..470 {
            if at(&a, x, y) != at(&img, x, y) {
                moved += 1;
            }
        }
    }
    assert!(moved > 50, "盘内那条亮带几乎没被推动：只变了 {moved} 格");
    // 盘缘外两行起必须逐格相同——切带写错范围就会在这里露出来
    for y in 0..h as i64 {
        if (y - yc).abs() <= r + 1 {
            continue;
        }
        for x in 0..w as i64 {
            assert_eq!(at(&a, x, y), at(&img, x, y), "盘外第 {x},{y} 格被改了");
        }
    }
}
