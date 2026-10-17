//! Rasterize bounded, source-relative independent local adjustment masks.
use crate::develop::params::{Mask, MaskKind};
use crate::develop::tone::smooth;
use px_core::{par::par_chunks_mut, Alpha};
fn segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1])
        / (d[0] * d[0] + d[1] * d[1]).max(1e-10))
    .clamp(0.0, 1.0);
    ((p[0] - a[0] - d[0] * t).powi(2) + (p[1] - a[1] - d[1] * t).powi(2)).sqrt()
}
pub fn raster(w: usize, h: usize, m: &Mask) -> Alpha {
    let mut result = Alpha {
        w,
        h,
        v: vec![0; w * h],
    };
    let feather = m.feather.pos();
    let aspect = w as f32 / h.max(1) as f32;
    par_chunks_mut(&mut result.v, (w * 16).max(1), |chunk, band| {
        for (j, value) in chunk.iter_mut().enumerate() {
            let i = band * w * 16 + j;
            let p = [
                ((i % w) as f32 + 0.5) / w as f32,
                ((i / w) as f32 + 0.5) / h as f32,
            ];
            let a = match m.kind {
                MaskKind::Radial => {
                    let d = (((p[0] - m.center[0]) / m.radius[0].max(0.005)).powi(2)
                        + ((p[1] - m.center[1]) / m.radius[1].max(0.005)).powi(2))
                    .sqrt();
                    1.0 - smooth((1.0 - feather).min(0.999), 1.0, d)
                }
                MaskKind::Linear => {
                    let d = [m.end[0] - m.center[0], m.end[1] - m.center[1]];
                    let t = ((p[0] - m.center[0]) * d[0] + (p[1] - m.center[1]) * d[1])
                        / (d[0] * d[0] + d[1] * d[1]).max(1e-8);
                    smooth(0.5 - feather * 0.5 - 0.001, 0.5 + feather * 0.5 + 0.001, t)
                }
                MaskKind::Brush => 0.0,
            };
            *value = (a * 255.0).round() as u8;
        }
    });
    // Rasterize each segment only inside its brush bounds; cost does not scale as pixels × all points.
    for s in &m.strokes {
        if s.points.is_empty() {
            continue;
        }
        let r = s.radius * aspect.sqrt();
        let rx = r / aspect;
        let ry = r;
        let low = s
            .points
            .iter()
            .fold([1.0f32; 2], |a, p| [a[0].min(p[0]), a[1].min(p[1])]);
        let high = s
            .points
            .iter()
            .fold([0.0f32; 2], |a, p| [a[0].max(p[0]), a[1].max(p[1])]);
        let x0 = ((low[0] - rx).max(0.0) * w as f32).floor() as usize;
        let y0 = ((low[1] - ry).max(0.0) * h as f32).floor() as usize;
        let x1 = (((high[0] + rx) * w as f32).ceil() as usize).min(w);
        let y1 = (((high[1] + ry) * h as f32).ceil() as usize).min(h);
        let bw = x1.saturating_sub(x0);
        let bh = y1.saturating_sub(y0);
        if bw == 0 || bh == 0 {
            continue;
        }
        let mut ink = vec![0u8; bw * bh];
        for i in 0..s.points.len().saturating_sub(1).max(1) {
            let a = s.points[i];
            let b = s.points[(i + 1).min(s.points.len() - 1)];
            let sx = ((a[0].min(b[0]) - rx).max(0.0) * w as f32).floor() as usize;
            let sy = ((a[1].min(b[1]) - ry).max(0.0) * h as f32).floor() as usize;
            let ex = (((a[0].max(b[0]) + rx) * w as f32).ceil() as usize).min(x1);
            let ey = (((a[1].max(b[1]) + ry) * h as f32).ceil() as usize).min(y1);
            for y in sy.max(y0)..ey {
                for x in sx.max(x0)..ex {
                    let p = [
                        (x as f32 + 0.5) / w as f32 * aspect,
                        (y as f32 + 0.5) / h as f32,
                    ];
                    let d = segment(p, [a[0] * aspect, a[1]], [b[0] * aspect, b[1]]);
                    let v = ((1.0 - smooth(r * (1.0 - feather).min(0.999), r, d)) * 255.0).round()
                        as u8;
                    let k = (y - y0) * bw + x - x0;
                    ink[k] = ink[k].max(v);
                }
            }
        }
        for y in y0..y1 {
            for x in x0..x1 {
                let a = &mut result.v[y * w + x];
                let v = ink[(y - y0) * bw + x - x0] as u32;
                *a = if s.erase {
                    ((*a as u32 * (255 - v) + 127) / 255) as u8
                } else {
                    (*a as u32 + ((255 - *a as u32) * v + 127) / 255) as u8
                };
            }
        }
    }
    if m.invert {
        for a in &mut result.v {
            *a = 255 - *a;
        }
    }
    result
}
