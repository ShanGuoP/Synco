//! Separate luma/chroma denoising, manual lens correction, seeded grain and vignette.
use crate::develop::params::{Detail, Effects, Lens};
use crate::develop::tone::smooth;
use crate::px::Plane;
use px_core::{par::par_chunks_mut, Rgba};

pub fn denoise(img: &Rgba, d: &Detail) -> Rgba {
    if d.luma.is_zero() && d.chroma.is_zero() {
        return img.clone();
    }
    let n = img.w * img.h;
    let mut y = Plane::new(img.w, img.h);
    let mut u = Plane::new(img.w, img.h);
    let mut v = Plane::new(img.w, img.h);
    for i in 0..n {
        let p = &img.px[i * 4..];
        let l = p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722;
        y.v[i] = l;
        u.v[i] = p[2] as f32 - l;
        v.v[i] = p[0] as f32 - l;
    }
    let radius = (img.w.min(img.h) as f32 / 700.0).round().clamp(1.0, 6.0) as usize;
    let yl = y.box_blur_passes(radius, 2);
    let ul = u.box_blur_passes(radius * 2, 2);
    let vl = v.box_blur_passes(radius * 2, 2);
    let mut out = img.clone();
    par_chunks_mut(&mut out.px, (img.w * 16).max(1) * 4, |chunk, band| {
        for (j, p) in chunk.chunks_exact_mut(4).enumerate() {
            let i = band * img.w * 16 + j;
            let diff = (y.v[i] - yl.v[i]).abs();
            // Strong edges retain their original luma; chroma smoothing is independently weighted.
            let a = d.luma.pos() * (1.0 - smooth(3.0, 12.0 + d.luma.pos() * 18.0, diff));
            let b = d.chroma.pos() * (1.0 - smooth(8.0, 40.0, diff));
            let yy = y.v[i] + (yl.v[i] - y.v[i]) * a;
            let uu = u.v[i] + (ul.v[i] - u.v[i]) * b;
            let vv = v.v[i] + (vl.v[i] - v.v[i]) * b;
            let r = yy + vv;
            let blue = yy + uu;
            let g = (yy - 0.2126 * r - 0.0722 * blue) / 0.7152;
            for (k, c) in [r, g, blue].iter().enumerate() {
                p[k] = c.round().clamp(0.0, 255.0) as u8;
            }
        }
    });
    out
}
fn bilinear(img: &Rgba, x: f32, y: f32, k: usize) -> f32 {
    let x = x.clamp(0.0, (img.w - 1) as f32);
    let y = y.clamp(0.0, (img.h - 1) as f32);
    let ix = x as usize;
    let iy = y as usize;
    let xx = (ix + 1).min(img.w - 1);
    let yy = (iy + 1).min(img.h - 1);
    let a = img.px[(iy * img.w + ix) * 4 + k] as f32;
    let b = img.px[(iy * img.w + xx) * 4 + k] as f32;
    let c = img.px[(yy * img.w + ix) * 4 + k] as f32;
    let d = img.px[(yy * img.w + xx) * 4 + k] as f32;
    (a + (b - a) * (x - ix as f32)) * (1.0 - (y - iy as f32))
        + (c + (d - c) * (x - ix as f32)) * (y - iy as f32)
}
pub fn lens(img: &Rgba, l: &Lens) -> Rgba {
    if *l == Lens::default() || img.w == 0 || img.h == 0 {
        return img.clone();
    }
    let mut out = img.clone();
    let (cx, cy) = ((img.w - 1) as f32 * 0.5, (img.h - 1) as f32 * 0.5);
    let scale = cx.max(cy).max(1.0);
    par_chunks_mut(&mut out.px, (img.w * 16).max(1) * 4, |chunk, band| {
        for (j, p) in chunk.chunks_exact_mut(4).enumerate() {
            let i = band * img.w * 16 + j;
            let dx = (i % img.w) as f32 - cx;
            let dy = (i / img.w) as f32 - cy;
            let r2 = (dx * dx + dy * dy) / (scale * scale);
            let f = 1.0 + l.distortion.norm() * 0.25 * r2;
            for (k, value) in p.iter_mut().enumerate() {
                let ca = match k {
                    0 => l.chromatic.norm() * 0.008,
                    2 => -l.chromatic.norm() * 0.008,
                    _ => 0.0,
                };
                let f = f + ca * r2;
                *value = bilinear(img, cx + dx * f, cy + dy * f, k).round() as u8;
            }
        }
    });
    out
}
fn noise(x: u32, y: u32, seed: u32) -> f32 {
    let mut n =
        x.wrapping_mul(374761393) ^ y.wrapping_mul(668265263) ^ seed.wrapping_mul(2246822519);
    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
    ((n ^ (n >> 16)) as f32 / u32::MAX as f32) * 2.0 - 1.0
}
pub fn effects(img: &Rgba, e: &Effects) -> Rgba {
    if e.grain.is_zero() && e.vignette.is_zero() && e.film.is_zero() {
        return img.clone();
    }
    let mut out = img.clone();
    let reference = img.w.min(img.h).max(1) as f32;
    let cell = (0.6 + e.size.pos() * 4.0) * reference / 1600.0;
    par_chunks_mut(&mut out.px, (img.w * 16).max(1) * 4, |chunk, band| {
        for (j, p) in chunk.chunks_exact_mut(4).enumerate() {
            let i = band * img.w * 16 + j;
            let x = (i % img.w) as f32;
            let y = (i / img.w) as f32;
            let radius = (((x + 0.5) / img.w as f32 - 0.5).powi(2)
                + ((y + 0.5) / img.h as f32 - 0.5).powi(2))
            .sqrt()
                * 1.414214;
            let start = 0.1 + e.midpoint.pos() * 0.65;
            let vig = smooth(
                start,
                (start + 0.05 + e.feather.pos() * 0.65).min(1.3),
                radius,
            );
            let f = noise(
                (x / cell.max(0.5)) as u32,
                (y / cell.max(0.5)) as u32,
                e.seed,
            );
            let f = f * (1.0 - e.roughness.pos() * 0.5)
                + noise(
                    (x / cell.max(0.5) * 2.7) as u32,
                    (y / cell.max(0.5) * 2.7) as u32,
                    e.seed ^ 0xa132,
                ) * e.roughness.pos()
                    * 0.5;
            for value in p.iter_mut().take(3) {
                let mut c = *value as f32 / 255.0;
                let film = e.film.pos();
                c = c * (1.0 - film * 0.12) + film * 0.035;
                c += film * 0.12 * (c - 0.5) * c * (1.0 - c);
                c += e.vignette.norm() * vig * 0.55 * c.max(0.1);
                c += f * e.grain.pos() * 0.12 * (0.35 + 0.65 * (1.0 - (c - 0.5).abs() * 2.0));
                *value = (c * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    });
    out
}
