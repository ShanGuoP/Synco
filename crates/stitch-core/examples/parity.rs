//! M1-6 对拍产出器：把 stitch-core 的结果按案例写成 PNG，交给浏览器里的 JS 参照实现逐像素比。
//!
//! 用法：`cargo run --release -p stitch-core --example parity -- "<隔离实例的 DATA>/parity"`
//! 场景完全由整数算术生成，JS 侧用同一套公式复现，不传像素只传 PNG。

use stitch_core::*;

/// 每个案例：底图尺寸 + 涂抹层相对底图的比例因子
struct Case {
    name: &'static str,
    w: usize,
    h: usize,
    /// 涂抹层 → 原图 的坐标比例 k（1 = 涂抹层和原图同尺寸；2 = 涂抹层是半分辨率）
    k: usize,
    /// true = 涂抹铺到近整幅（裁切框退化成整图那条路）
    wide: bool,
    expand: f64,
    context: f64,
    crop_edge: u32,
    feather: f64,
    levels: usize,
}

const CASES: [Case; 4] = [
    Case { name: "c1_square_k1", w: 640, h: 480, k: 1, wide: false, expand: 96.0, context: 0.35, crop_edge: 1024, feather: 48.0, levels: 4 },
    Case { name: "c2_portrait_k2", w: 900, h: 1200, k: 2, wide: false, expand: 96.0, context: 0.35, crop_edge: 1024, feather: 48.0, levels: 4 },
    Case { name: "c3_wide_small_expand", w: 1280, h: 720, k: 1, wide: false, expand: 40.0, context: 0.2, crop_edge: 1536, feather: 24.0, levels: 3 },
    Case { name: "c4_near_full_frame_paint", w: 800, h: 800, k: 1, wide: true, expand: 120.0, context: 0.35, crop_edge: 1024, feather: 60.0, levels: 4 },
];

/// 底图像素：全整数，JS 侧 buildScene 必须逐字一致
fn base_pixel(x: usize, y: usize) -> [u8; 4] {
    [
        ((x * 7 + y * 3) & 255) as u8,
        ((x ^ (y * 5)) & 255) as u8,
        (((x + y) >> 1) & 255) as u8,
        255,
    ]
}

fn ink_at(fx: f64, fy: f64, wide: bool) -> bool {
    // 相对坐标下的两块笔迹：一个矩形 + 一个圆斑；wide 时矩形铺到近整幅
    let (x0, y0, x1, y1) = if wide { (0.05, 0.05, 0.95, 0.95) } else { (0.34, 0.30, 0.52, 0.46) };
    if fx >= x0 && fx < x1 && fy >= y0 && fy < y1 {
        return true;
    }
    let cx = 0.62;
    let cy = 0.60;
    let r = 0.05;
    ((fx - cx) * (fx - cx) + (fy - cy) * (fy - cy)) <= r * r
}

fn scene(c: &Case) -> (Rgba, Alpha) {
    let (w, h) = (c.w, c.h);
    let mut base = Rgba::new(w, h);
    for y in 0..h {
        for x in 0..w {
            base.set(x, y, base_pixel(x, y));
        }
    }
    let (mw, mh) = (w / c.k, h / c.k);
    let mut mask = Alpha::new(mw, mh);
    for y in 0..mh {
        for x in 0..mw {
            if ink_at(x as f64 / mw as f64, y as f64 / mh as f64, c.wide) {
                mask.v[y * mw + x] = 255;
            }
        }
    }
    (base, mask)
}

/// 假云端成图：从载荷图做确定性变形，两侧用同一个公式，保证 stitch 的输入一致
fn model_from(payload: &Rgba) -> Rgba {
    let mut out = payload.clone();
    for i in 0..out.w * out.h {
        let r = out.px[i * 4] as u32;
        let g = out.px[i * 4 + 1] as u32;
        let b = out.px[i * 4 + 2] as u32;
        out.px[i * 4] = (r + 40).min(255) as u8;
        out.px[i * 4 + 1] = (g / 2) as u8;
        out.px[i * 4 + 2] = (255 - b) as u8;
    }
    out
}

fn write_png(dir: &str, name: &str, img: &Rgba) -> std::io::Result<()> {
    let path = format!("{dir}/{name}");
    imageproc::image::save_buffer_with_format(
        &path,
        &img.px,
        img.w as u32,
        img.h as u32,
        imageproc::image::ColorType::Rgba8,
        imageproc::image::ImageFormat::Png,
    )
    .map_err(|e| std::io::Error::other(format!("{path}: {e}")))?;
    Ok(())
}

fn main() -> std::io::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let mut meta = String::from("{\n");
    for (i, c) in CASES.iter().enumerate() {
        let (base, mask) = scene(c);
        let p = StitchParams {
            expand: c.expand,
            context: c.context,
            crop_edge: c.crop_edge,
            feather: c.feather,
            levels: c.levels,
            invert: false,       // 对拍基准是正向那套 JS，反向没有参照物
        };
        let Build::Payload(pl) = build_crop_payload(&base, &mask, &p) else {
            return Err(std::io::Error::other(format!("{} 载荷构建失败", c.name)));
        };
        let model = model_from(&pl.image);
        let final_img = stitch_crop(&base, &mask, pl.crop, &model, &p);
        let weights = paste_weights(&base, &mask, pl.crop, &p);

        write_png(&dir, &format!("{}.model.png", c.name), &model)?;
        write_png(&dir, &format!("{}.payload.png", c.name), &pl.image)?;
        write_png(&dir, &format!("{}.mask.png", c.name), &pl.mask)?;
        write_png(&dir, &format!("{}.final.png", c.name), &final_img)?;
        write_png(&dir, &format!("{}.weights.png", c.name), &Rgba::from_alpha(&weights))?;

        if i > 0 {
            meta.push_str(",\n");
        }
        meta.push_str(&format!(
            "  \"{}\": {{\"w\":{}, \"h\":{}, \"k\":{}, \"crop\":[{},{},{},{}], \"payload\":[{},{}], \"zero_weight_px\":{}, \"total_crop_px\":{}}}",
            c.name,
            c.w,
            c.h,
            c.k,
            pl.crop.x,
            pl.crop.y,
            pl.crop.w,
            pl.crop.h,
            pl.fit.w,
            pl.fit.h,
            (0..pl.crop.w * pl.crop.h).filter(|q| weights.v[*q] == 0).count(),
            pl.crop.w * pl.crop.h
        ));
    }
    meta.push_str("\n}\n");
    std::fs::write(format!("{dir}/meta.json"), &meta)?;
    println!("{meta}");
    Ok(())
}
