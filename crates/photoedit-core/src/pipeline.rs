//! 链的组装：几何 → 变形 → LUT → 调色 → 美颜。
//!
//! 顺序是有理由的，不是随手排的：
//! - 变形在调色之前，插值出来的像素才不会再被色彩算子二次改动；
//! - 美颜在最后，它的蒙版坐标约定在"变形之后"的那个域里，前端涂在哪儿就磨在哪儿；
//! - LUT 在滑杆之前，预设曲线打底、滑杆做微调，反过来会让滑杆失去锚点。
//!
//! 恒等参数原图直出，一个字节都不动——"没调过的图"与 0.2.1 逐字段一致这件事靠这里保证。

use crate::beauty;
use crate::color;
use crate::geometry;
use crate::lut;
use crate::ops::EditOps;
use crate::warp::{self, FaceShape};
use stitch_core::{Alpha, Rgba};

/// 链的可选输入。都是引用：内核不认领内存，也不去磁盘找东西。
#[derive(Default)]
pub struct Chain<'a> {
    /// 美颜蒙版，尺寸必须等于**几何+变形之后**的图
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
    let mut cur = geometry::apply(img, &ops.geometry);
    cur = warp::apply(&cur, &ops.warp, chain.shape);
    if let (Some(l), Some(table)) = (ops.lut.as_ref(), chain.lut) {
        cur = lut::apply(&cur, table, l.strength.pos().min(1.0));
    }
    cur = color::apply(&cur, &ops.color);
    let mut mask = chain.mask;
    if mask.map(|m| m.w != cur.w || m.h != cur.h).unwrap_or(false) {
        // 尺寸对不上宁可不加限定：蒙版是用户画的，静默挪位比"全图生效"更难解释
        mask = None;
    }
    beauty::apply(&cur, &ops.beauty, mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{self, Color, Geometry, Slider, Stroke};
    use stitch_core::Rgba;

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
        let (ops, _) = EditOps::parse(ops::DEFAULT_JSON).unwrap();
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

    #[test]
    fn 几何在前美颜在后() {
        // 转 90° 之后画幅换了边长，蒙版必须按换过边长的图给
        let img = ramp(48, 24);
        let mut o = EditOps::default();
        o.geometry = Geometry { crop: None, rotate_deg: 90.0, flip_h: false, flip_v: false, fill: ops::Fill::Edge };
        o.beauty.smooth = Slider(80);
        let out = apply_chain(&img, &o, &Chain::default());
        assert_eq!((out.w, out.h), (24, 48), "链把几何顺序丢了");
        // 给错尺寸的蒙版：不该 panic，也不该把效果歪着贴
        let wrong = Alpha::new(48, 24);
        let out2 = apply_chain(&img, &o, &Chain { mask: Some(&wrong), ..Default::default() });
        assert_eq!(out2.w, 24);
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
        let small = stitch_core::resize_rgba(&big, 80, 50);
        let a = apply_chain(&small, &o, &Chain::default());
        let b = apply_chain(&big, &o, &Chain::default());
        // 裁切之后两侧画幅不同档，先把大的缩到小的那个尺寸再比
        assert_eq!((b.w, b.h), (a.w * 4, a.h * 4), "裁切框在两档上没按同一比例落位：{}×{} vs {}×{}", b.w, b.h, a.w, a.h);
        let b_small = stitch_core::resize_rgba(&b, a.w, a.h);
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
