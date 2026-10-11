//! 链的组装：几何 → 变形 → LUT → 调色 → 美颜。
//!
//! 顺序是有理由的，不是随手排的：
//! - 变形在调色之前，插值出来的像素才不会再被色彩算子二次改动；
//! - 美颜在最后，蒙版按源图域收、跟着几何段一起转，前端涂在哪儿就磨在哪儿（转过 90° 也一样）；
//! - LUT 在滑杆之前，预设曲线打底、滑杆做微调，反过来会让滑杆失去锚点。
//!
//! 恒等参数原图直出，一个字节都不动——"没调过的图"与 0.2.1 逐字段一致这件事靠这里保证。

use crate::beauty;
use crate::color;
use crate::geometry;
use crate::lut;
use crate::ops::EditOps;
use crate::warp::{self, FaceShape};
use px_core::{Alpha, Rgba};
use std::borrow::Cow;

/// 链的可选输入。都是引用：内核不认领内存，也不去磁盘找东西。
#[derive(Default)]
pub struct Chain<'a> {
    /// 美颜蒙版，尺寸等于**传进来的那张图**（源图域）：几何段会带着它一起走，
    /// 所以带裁切/旋转时它落在对的位置上
    pub mask: Option<&'a Alpha>,
    /// 一键塑形要的关键点组
    pub shape: Option<&'a FaceShape>,
    /// 已解析好的 LUT（读文件是调用方的事）
    pub lut: Option<&'a lut::Lut>,
}

pub fn apply_chain(img: &Rgba, ops: &EditOps, chain: &Chain) -> Rgba {
    if ops.is_identity() {
        return img.clone();
    }
    let geometric = geometry::needs_pass(&ops.geometry);
    let mut cur = if geometric {
        Cow::Owned(geometry::apply(img, &ops.geometry))
    } else {
        Cow::Borrowed(img)
    };
    // 笔迹活在源图域，几何段换了坐标系就得跟着转一遍；转完还是对不上就是调用方给错了档，
    // 宁可不加限定——静默挪位比"全图生效"更难解释
    let mask = match chain.mask {
        Some(m) if !ops.beauty.is_empty() && m.w == img.w && m.h == img.h => {
            Some(if geometric { Cow::Owned(geometry::apply_alpha(m, &ops.geometry)) } else { Cow::Borrowed(m) })
        }
        _ => None,
    };
    if ops.warp.strokes.iter().any(|s| !s.points.is_empty())
        || (!ops.warp.auto.is_empty() && chain.shape.is_some())
    {
        cur = Cow::Owned(warp::apply(&cur, &ops.warp, chain.shape));
    }
    if let (Some(l), Some(table)) = (ops.lut.as_ref(), chain.lut) {
        let strength = l.strength.pos().min(1.0);
        if strength > 0.0 {
            cur = Cow::Owned(lut::apply(&cur, table, strength));
        }
    }
    if !ops.color.is_empty() {
        cur = Cow::Owned(color::apply(&cur, &ops.color));
    }
    if !ops.beauty.is_empty() {
        cur = Cow::Owned(beauty::apply(&cur, &ops.beauty, mask.as_deref()));
    }
    cur.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{self, Color, Geometry, Slider, Stroke};
    use px_core::Rgba;

    fn ramp(w: usize, h: usize) -> Rgba {
        let mut img = Rgba::new(w, h);
        for i in 0..w * h {
            let v = (i % 256) as u8;
            img.px[i * 4..i * 4 + 4].copy_from_slice(&[v, 255u8.wrapping_sub(v), (i / 7) as u8, 255]);
        }
        img
    }

    #[test]
    fn 恒等参数逐位直出() {
        let img = ramp(64, 33);
        let (ops, _) = EditOps::parse(ops::default_json()).unwrap();
        assert_eq!(apply_chain(&img, &ops, &Chain::default()), img);
        // 一条空笔画也不该触发变形段
        let mut o = ops.clone();
        o.warp.strokes.push(Stroke::default());
        assert_eq!(apply_chain(&img, &o, &Chain::default()), img);
    }

    #[test]
    fn 只开调色时等价于直接调色() {
        let img = ramp(32, 32);
        let mut o = EditOps::default();
        o.color = Color { exposure: Slider(30), contrast: Slider(-20), ..Default::default() };
        let a = apply_chain(&img, &o, &Chain::default());
        let b = color::apply(&img, &o.color);
        assert_eq!(a, b, "链上多跑的空段不该改变结果");
    }

    /// 保留优化前的阶段调用顺序作为参考，覆盖空段、缺少可选输入与混合算子。
    fn staged_reference(img: &Rgba, ops: &EditOps, chain: &Chain) -> Rgba {
        if ops.is_identity() {
            return img.clone();
        }
        let mut cur = geometry::apply(img, &ops.geometry);
        let mask = match chain.mask {
            Some(m) if m.w == img.w && m.h == img.h => Some(geometry::apply_alpha(m, &ops.geometry)),
            _ => None,
        };
        cur = warp::apply(&cur, &ops.warp, chain.shape);
        if let (Some(l), Some(table)) = (ops.lut.as_ref(), chain.lut) {
            cur = lut::apply(&cur, table, l.strength.pos().min(1.0));
        }
        cur = color::apply(&cur, &ops.color);
        beauty::apply(&cur, &ops.beauty, mask.as_ref())
    }

    #[test]
    fn 跳过空阶段与原链逐像素一致() {
        let mut img = ramp(24, 16);
        for (i, p) in img.px.chunks_exact_mut(4).enumerate() {
            p[3] = (i * 37 % 256) as u8;
        }
        let mask = Alpha::from_vec(24, 16, (0..384).map(|i| (i * 19 % 256) as u8).collect());
        let wrong = Alpha::new(16, 24);
        let table = lut::parse_cube("LUT_1D_SIZE 2\n1 0 0\n0 1 1\n").unwrap();
        for mode in 0..4 {
            for active in 0..16 {
                let mut o = EditOps::default();
                match mode {
                    1 => o.geometry.rotate_deg = 90.0,
                    2 => { o.geometry.flip_h = true; o.geometry.crop = Some([0.1, 0.1, 0.8, 0.8]); }
                    3 => { o.geometry.rotate_deg = 7.0; o.warp.auto.eye_big = Slider(30); }
                    _ => {}
                }
                if active & 1 != 0 { o.color.exposure = Slider(25); o.color.sharpen = Slider(10); }
                if active & 2 != 0 {
                    o.beauty.smooth = Slider(40);
                    o.beauty.blemish = Slider(50);
                    o.beauty.even_tone = Slider(25);
                }
                if active & 4 != 0 {
                    o.warp.strokes.push(Stroke { points: vec![[0.3, 0.5], [0.6, 0.5]],
                        radius: 0.1, strength: Slider(35), ..Default::default() });
                } else { o.warp.strokes.push(Stroke::default()); }
                // 同时覆盖强度零与缺失 LUT 表：它们都应该保持恒等。
                o.lut = Some(ops::Lut { name: "test.cube".into(),
                    strength: Slider(if active & 8 != 0 { 70 } else { 0 }) });
                for m in [None, Some(&mask), Some(&wrong)] {
                    for l in [None, Some(&table)] {
                        let chain = Chain { mask: m, lut: l, shape: None };
                        assert_eq!(apply_chain(&img, &o, &chain), staged_reference(&img, &o, &chain),
                            "mode={mode}, active={active}, mask={}, lut={}", m.is_some(), l.is_some());
                    }
                }
            }
        }
    }

    /// 同一进程比较原链与跳过空阶段的链，纯内核计时，不包含解码与 IO。
    #[test]
    #[ignore]
    fn bench_二十四mp只开调色() {
        use std::{hint::black_box, time::Instant};
        let img = ramp(6000, 4000);
        let mut o = EditOps::default();
        o.color.exposure = Slider(20);
        o.color.contrast = Slider(15);
        let chain = Chain::default();
        for _ in 0..3 {
            let t = Instant::now();
            let old = black_box(staged_reference(&img, &o, &chain));
            let old_ms = t.elapsed().as_secs_f64() * 1000.0;
            let t = Instant::now();
            let new = black_box(apply_chain(&img, &o, &chain));
            let new_ms = t.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(old, new);
            println!("只开调色 6000×4000：原链 {old_ms:.1}ms / 跳过空段 {new_ms:.1}ms");
        }
    }

    #[test]
    fn 几何在前美颜在后() {
        // 转 90° 之后画幅换了边长，但蒙版按**源图**那对尺寸给（链子带着它一起转）
        let img = ramp(48, 24);
        let mut o = EditOps::default();
        o.geometry = Geometry { crop: None, rotate_deg: 90.0, flip_h: false, flip_v: false, fill: ops::Fill::Edge };
        o.beauty.smooth = Slider(80);
        let out = apply_chain(&img, &o, &Chain::default());
        assert_eq!((out.w, out.h), (24, 48), "链把几何顺序丢了");
        // 尺寸对不上的蒙版（比如拿转过的档来给）：不该 panic，也不该把效果歪着贴
        let wrong = Alpha::new(24, 48);
        let out2 = apply_chain(&img, &o, &Chain { mask: Some(&wrong), ..Default::default() });
        assert_eq!(out2.w, 24);
        assert_eq!(out2.px, out.px, "对不上尺寸的蒙版被将就着用了，效果应该整段不加");
    }

    #[test]
    fn 旋转之后美颜仍然落在涂过的那一块() {
        // 源图 48×24，笔迹只涂左半边；右转 90° 之后是 24×48，那半边的笔迹落在**上半**
        let img = ramp(48, 24);
        let mut o = EditOps::default();
        o.geometry = Geometry { crop: None, rotate_deg: 90.0, flip_h: false, flip_v: false, fill: ops::Fill::Edge };
        o.beauty.smooth = Slider(100);
        let mut m = Alpha::new(48, 24);
        for y in 0..24 {
            for x in 0..48 {
                m.set(x, y, if x < 24 { 255 } else { 0 });
            }
        }
        let base = apply_chain(&img, &EditOps { geometry: o.geometry.clone(), ..Default::default() }, &Chain::default());
        let out = apply_chain(&img, &o, &Chain { mask: Some(&m), ..Default::default() });
        assert_eq!((out.w, out.h), (24, 48));
        fn row(img: &Rgba, y: usize) -> &[u8] {
            let s = y * img.w * 4;
            &img.px[s..s + img.w * 4]
        }
        assert_ne!(row(&out, 0), row(&base, 0), "笔迹转过去的那半边没吃到美颜");
        assert_eq!(row(&out, 40), row(&base, 40), "没涂的下半边被一起磨了：遮罩没跟着几何段走");
    }

    #[test]
    fn 蒙版限定只在涂过的地方生效() {
        let img = ramp(64, 16);
        let mut o = EditOps::default();
        o.beauty.brighten = Slider(100);
        let mut m = Alpha::new(64, 16);
        for y in 0..16 {
            for x in 0..64 {
                m.set(x, y, if x < 16 { 255 } else { 0 });
            }
        }
        let out = apply_chain(&img, &o, &Chain { mask: Some(&m), ..Default::default() });
        for y in 0..16 {
            for x in 20..64 {
                assert_eq!(out.get(x, y), img.get(x, y), "没涂的地方被动过：({x},{y})");
            }
        }
    }

    /// 平滑渐变图：`ramp` 那种逐像素锯齿的测试图会被缩放核放大成假差异，
    /// 档位一致性要的是照片那种局部平滑
    fn gradient(w: usize, h: usize) -> Rgba {
        let mut img = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / w.max(1)) as u8;
                let g = (y * 255 / h.max(1)) as u8;
                let b = ((x + y) * 255 / (w + h).max(1)) as u8;
                img.set(x, y, [r, g, b, 255]);
            }
        }
        img
    }

    #[test]
    fn 点算子在三档上落在同一相对位置() {
        // 参数归一化的意义：同一份参数在缩略档与原图档上作用于同一个相对区域。
        // 逐像素比对要先重采样，两侧都要过一遍卷积，所以只要求"平均意义上对得上"。
        let mut o = EditOps::default();
        o.geometry.crop = Some([0.1, 0.1, 0.6, 0.6]);
        // 只挑对色彩是"仿射"的两根滑杆：这样"先缩再调"与"先调再缩"理论上等价，
        // 差异就只剩量化，能钉得紧。曝光走线性域增益、磨皮走邻域卷积，都与缩放不可交换，
        // 它们的档位一致性由各自的算子测试与真实预览路径负责。
        o.color = Color { contrast: Slider(-20), saturation: Slider(40), ..Default::default() };
        let big = gradient(320, 200);
        let small = px_core::resize_rgba(&big, 80, 50);
        let a = apply_chain(&small, &o, &Chain::default());
        let b = apply_chain(&big, &o, &Chain::default());
        // 裁切之后两侧画幅不同档，先把大的缩到小的那个尺寸再比
        assert_eq!((b.w, b.h), (a.w * 4, a.h * 4), "裁切框在两档上没按同一比例落位：{}×{} vs {}×{}", b.w, b.h, a.w, a.h);
        let b_small = px_core::resize_rgba(&b, a.w, a.h);
        let mut diff = 0f64;
        for i in 0..a.w * a.h {
            for c in 0..3 {
                diff += (a.px[i * 4 + c] as f64 - b_small.px[i * 4 + c] as f64).abs();
            }
        }
        let mean = diff / (a.w * a.h * 3) as f64;
        assert!(mean < 1.2, "小档与原图档平均差 {mean:.3}，参数与画幅脱钩了");
    }

    /// 24MP 全链计时。机器相关，不进常规回归，跑法是
    /// `cargo test -p photoedit-core --release -- --ignored --nocapture bench`
    #[test]
    #[ignore]
    fn bench_全链二十四mp() {
        use std::time::Instant;
        let (w, h) = (6000, 4000);
        let mut img = Rgba::new(w, h);
        for i in 0..w * h {
            let v = (i % 256) as u8;
            img.px[i * 4..i * 4 + 4].copy_from_slice(&[v, v / 2, 255 - v, 255]);
        }
        let mut o = EditOps::default();
        o.geometry.crop = Some([0.05, 0.05, 0.9, 0.9]);
        o.color = Color { exposure: Slider(20), contrast: Slider(15), clarity: Slider(30), sharpen: Slider(25), ..Default::default() };
        o.beauty.smooth = Slider(60);
        let t = Instant::now();
        let out = apply_chain(&img, &o, &Chain::default());
        println!("全链 {}×{}：{:?}（含裁切/调色/清晰度/磨皮）", out.w, out.h, t.elapsed());
        let mut o2 = EditOps::default();
        o2.warp.strokes.push(Stroke { tool: ops::Tool::Push, points: (0..40).map(|i| [0.3 + i as f32 * 0.005, 0.5]).collect(), radius: 0.05, strength: Slider(70) });
        let t = Instant::now();
        crate::warp::apply(&img, &o2.warp, None);
        println!("40 点一笔的液化：{:?}", t.elapsed());
    }

    #[test]
    fn lut在滑杆之前() {
        // 反色 LUT + 饱和度 -100：先反色再变灰，结果应与同样顺序的手工组合一致
        let img = ramp(16, 8);
        let cube = {
            let mut t = String::from("LUT_3D_SIZE 2\n");
            for b in (0..2).rev() {
                for g in (0..2).rev() {
                    for r in (0..2).rev() {
                        t.push_str(&format!("{r} {g} {b}\n"));
                    }
                }
            }
            t
        };
        let l = lut::parse_cube(&cube).unwrap();
        let mut o = EditOps::default();
        o.lut = Some(ops::Lut { name: "neg".into(), strength: Slider(100) });
        o.color.saturation = Slider(-100);
        let out = apply_chain(&img, &o, &Chain { lut: Some(&l), ..Default::default() });
        let want = color::apply(&lut::apply(&img, &l, 1.0), &o.color);
        assert_eq!(out, want, "LUT 与滑杆的先后次序漂了");
    }
}
