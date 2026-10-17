//! Conservative histogram-based automatic tone and neutral-patch white balance.
use crate::develop::params::Basic;
use crate::ops::{Color, Slider};
use px_core::Rgba;
pub fn analyze(img: &Rgba, point: Option<[f32; 2]>) -> (Color, Option<Basic>) {
    let mut color = Color::default();
    if let Some([x, y]) = point {
        let cx = (x.clamp(0.0, 1.0) * (img.w.saturating_sub(1)) as f32) as usize;
        let cy = (y.clamp(0.0, 1.0) * (img.h.saturating_sub(1)) as f32) as usize;
        let mut sum = [0.0; 3];
        let mut n = 0.0;
        for yy in cy.saturating_sub(3)..=(cy + 3).min(img.h.saturating_sub(1)) {
            for xx in cx.saturating_sub(3)..=(cx + 3).min(img.w.saturating_sub(1)) {
                let p = &img.px[(yy * img.w + xx) * 4..];
                if p[3] > 0 && p[..3].iter().all(|v| *v > 5 && *v < 250) {
                    for k in 0..3 {
                        sum[k] += p[k] as f32;
                    }
                    n += 1.0;
                }
            }
        }
        if n > 0.0 {
            // Search the actual legacy temperature/tint gains, rather than a second WB model.
            let mut best = f32::INFINITY;
            for temp in (-100..=100).step_by(2) {
                for tint in (-100..=100).step_by(2) {
                    let r = sum[0] * (1.0 + 0.12 * temp as f32 / 100.0);
                    let g = sum[1] * (1.0 - 0.10 * tint as f32 / 100.0);
                    let b = sum[2]
                        * (1.0 - 0.12 * temp as f32 / 100.0)
                        * (1.0 + 0.05 * tint as f32 / 100.0);
                    let score = (r - g).powi(2) + (g - b).powi(2) + (b - r).powi(2);
                    if score < best {
                        best = score;
                        color.temp = Slider(temp);
                        color.tint = Slider(tint);
                    }
                }
            }
        }
        return (color, None);
    }
    let mut hist = [0usize; 256];
    let mut count = 0usize;
    for p in img.px.chunks_exact(4).filter(|p| p[3] > 0) {
        let y =
            (p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722).round() as usize;
        hist[y] += 1;
        count += 1;
    }
    if count == 0 {
        return (color, Some(Basic::default()));
    }
    let quantile = |q: f32| {
        let target = (count as f32 * q) as usize;
        let mut n = 0;
        for (i, v) in hist.iter().enumerate() {
            n += v;
            if n > target {
                return i as f32;
            }
        }
        255.0
    };
    let median = quantile(0.5);
    let lo = quantile(0.02);
    let hi = quantile(0.98);
    color.exposure = Slider(
        (((112.0 - median) / 112.0) * 35.0)
            .round()
            .clamp(-35.0, 35.0) as i32,
    );
    color.contrast = Slider(((180.0 - (hi - lo)) * 0.12).round().clamp(-15.0, 20.0) as i32);
    color.highlights = Slider(if hi > 235.0 { -20 } else { 0 });
    color.shadows = Slider(if lo < 25.0 { 18 } else { 0 });
    let basic = Basic {
        whites: Slider(((235.0 - hi) * 0.2).round().clamp(-10.0, 15.0) as i32),
        blacks: Slider(((12.0 - lo) * 0.2).round().clamp(-12.0, 8.0) as i32),
        brightness: Slider(0),
    };
    (color, Some(basic))
}
