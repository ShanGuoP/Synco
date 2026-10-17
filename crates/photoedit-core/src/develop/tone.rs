//! Curves, eight overlapping hue bands and three-way grading. Inputs are encoded sRGB.
use crate::develop::params::{Curves, Look};
use px_core::{par::par_chunks_mut, Rgba};

pub(super) fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
/// Shape-preserving cubic Hermite interpolation; no overshoot between control points.
pub fn curve(points: &[[f32; 2]], x: f32, neutral: f32) -> f32 {
    if points.is_empty() {
        return if neutral < 0.0 { x } else { neutral };
    }
    if x <= points[0][0] {
        return points[0][1];
    }
    let last = points.len() - 1;
    if x >= points[last][0] {
        return points[last][1];
    }
    let i = points
        .windows(2)
        .position(|p| x <= p[1][0])
        .unwrap_or(last - 1);
    let slope = |j: usize| {
        (points[j + 1][1] - points[j][1]) / (points[j + 1][0] - points[j][0]).max(0.00001)
    };
    let tangent = |j: usize| {
        if j == 0 {
            return slope(0);
        }
        if j == last {
            return slope(last - 1);
        }
        let a = slope(j - 1);
        let b = slope(j);
        if a * b <= 0.0 {
            0.0
        } else {
            2.0 * a * b / (a + b)
        }
    };
    let h = (points[i + 1][0] - points[i][0]).max(0.00001);
    let t = (x - points[i][0]) / h;
    let t2 = t * t;
    let t3 = t2 * t;
    ((2.0 * t3 - 3.0 * t2 + 1.0) * points[i][1]
        + (t3 - 2.0 * t2 + t) * h * tangent(i)
        + (-2.0 * t3 + 3.0 * t2) * points[i + 1][1]
        + (t3 - t2) * h * tangent(i + 1))
    .clamp(0.0, 1.0)
}
fn hue_curve(p: &[[f32; 2]], h: f32) -> f32 {
    if p.is_empty() {
        return 0.5;
    }
    // Evaluate the same endpoint at 0 and 1 so red never has a seam.
    let edge = curve(p, 0.0, 0.5);
    let mut circular = Vec::with_capacity(p.len() + 2);
    circular.push([0.0, edge]);
    circular.extend(p.iter().copied().filter(|v| v[0] > 0.0 && v[0] < 1.0));
    circular.push([1.0, edge]);
    curve(&circular, h, 0.5)
}
pub(super) fn to_hsl(c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c;
    let hi = r.max(g).max(b);
    let lo = r.min(g).min(b);
    let d = hi - lo;
    let l = (hi + lo) * 0.5;
    if d < 1e-7 {
        return [0.0, 0.0, l];
    }
    let h = if hi == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if hi == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [
        h / 6.0,
        (d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-6)).clamp(0.0, 1.0),
        l,
    ]
}
pub(super) fn from_hsl([h, s, l]: [f32; 3]) -> [f32; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = l - c * 0.5;
    let rgb = match h as u32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    rgb.map(|v| (v + m).clamp(0.0, 1.0))
}
pub(super) struct Prepared {
    rgb: Vec<f32>,
    channels: [Vec<f32>; 3],
    hue: [Vec<f32>; 3],
}
impl Prepared {
    pub(super) fn new(c: &Curves) -> Self {
        let table = |p: &[[f32; 2]], neutral: f32| {
            (0..=1024)
                .map(|i| curve(p, i as f32 / 1024.0, neutral))
                .collect()
        };
        Self {
            rgb: table(&c.rgb, -1.0),
            channels: [
                table(&c.red, -1.0),
                table(&c.green, -1.0),
                table(&c.blue, -1.0),
            ],
            hue: [&c.hue_hue, &c.hue_sat, &c.hue_lum].map(|p| {
                (0..=1024)
                    .map(|i| hue_curve(p, i as f32 / 1024.0))
                    .collect()
            }),
        }
    }
}
fn sample(t: &[f32], x: f32) -> f32 {
    let f = x.clamp(0.0, 1.0) * 1024.0;
    let i = f as usize;
    let j = (i + 1).min(1024);
    t[i] + (t[j] - t[i]) * (f - i as f32)
}
pub(super) fn pixel(mut c: [f32; 3], l: &Look, p: &Prepared) -> [f32; 3] {
    let y = c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722;
    let d = l.basic.brightness.norm() * 0.2
        + l.basic.whites.norm() * 0.25 * smooth(0.55, 1.0, y)
        + l.basic.blacks.norm() * 0.2 * (1.0 - smooth(0.0, 0.45, y));
    for k in 0..3 {
        c[k] = sample(&p.channels[k], sample(&p.rgb, (c[k] + d).clamp(0.0, 1.0)));
    }
    let [mut h, mut s, mut v] = to_hsl(c);
    let original_h = h;
    const CENTERS: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 315.0];
    let mut weights = [0.0; 8];
    let mut sum = 0.0;
    for (i, w) in weights.iter_mut().enumerate() {
        let d = (original_h * 360.0 - CENTERS[i]).abs();
        let d = d.min(360.0 - d);
        *w = 1.0 - smooth(0.0, 60.0, d);
        sum += *w;
    }
    let mut shift = [0.0; 3];
    for (b, w) in l.hsl.iter().zip(weights) {
        let w = w / sum.max(1e-6);
        shift[0] += b.hue.norm() * w;
        shift[1] += b.saturation.norm() * w;
        shift[2] += b.luminance.norm() * w;
    }
    h += (shift[0] * 30.0) / 360.0 + (sample(&p.hue[0], original_h) - 0.5) * 0.5;
    s = (s * (1.0 + shift[1] + (sample(&p.hue[1], original_h) - 0.5) * 2.0)).clamp(0.0, 1.0);
    v = (v + shift[2] * 0.25 + (sample(&p.hue[2], original_h) - 0.5) * 0.5).clamp(0.0, 1.0);
    c = from_hsl([h, s, v]);
    let g = &l.grading;
    let y = (y - g.balance.norm() * 0.2).clamp(0.0, 1.0);
    let width = 0.2 + g.blending.pos() * 0.3;
    let sh = 1.0 - smooth(0.5 - width, 0.5 + width, y);
    let hi = smooth(0.5 - width, 0.5 + width, y);
    let mid = 4.0 * y * (1.0 - y);
    let weights = [sh * (1.0 - mid * 0.5), mid, hi * (1.0 - mid * 0.5)];
    let total = weights.iter().sum::<f32>().max(1.0);
    for (zone, w) in [&g.shadows, &g.midtones, &g.highlights].iter().zip(weights) {
        let rgb = from_hsl([zone.hue as f32 / 360.0, 1.0, 0.5]);
        let lum = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        for k in 0..3 {
            c[k] += (rgb[k] - lum) * zone.saturation.pos() * 0.3 * w / total
                + zone.luminance.norm() * 0.15 * w / total;
        }
    }
    c.map(|v| v.clamp(0.0, 1.0))
}
pub fn apply(img: &Rgba, l: &Look) -> Rgba {
    if l.is_empty() {
        return img.clone();
    }
    let p = Prepared::new(&l.curves);
    let mut out = img.clone();
    par_chunks_mut(&mut out.px, (img.w * 16).max(1) * 4, |chunk, _| {
        for px in chunk.chunks_exact_mut(4) {
            let c = pixel(
                [
                    px[0] as f32 / 255.0,
                    px[1] as f32 / 255.0,
                    px[2] as f32 / 255.0,
                ],
                l,
                &p,
            );
            for k in 0..3 {
                px[k] = (c[k] * 255.0).round() as u8;
            }
        }
    });
    out
}
