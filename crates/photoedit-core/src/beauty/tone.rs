//! 美白、匀肤和去油光的亮度/色度调节。
use crate::px::Plane;
use super::{m_at, EVEN_TONE_RADIUS, EVEN_TONE_MAX, SHINE_FLOOR, SHINE_KEEP};
/// 美白：肤色域软掩码 × gamma 提亮。非肤色（蓝天、灰墙）几乎不动，避免整张过曝。
pub(super) fn brighten_luma(y: &mut [f32], cb: &[f32], cr: &[f32], msk: &Option<Vec<f32>>, strength: f32) {
    for i in 0..y.len() {
        let k = m_at(msk, i) * skin_weight(cb[i], cr[i]) * strength;
        if k <= 1e-3 {
            continue;
        }
        let v = y[i] / 255.0;
        // gamma 从 1 走到 0.72：满档时中灰提到约 186
        let g = 1.0 - 0.28 * k;
        let lifted = v.powf(g) * 255.0;
        y[i] += (lifted - y[i]) * k;
    }
}

/// 肤色隶属度：Cb/Cr 的经典盒式范围 + 软边，范围外平滑到 0。
/// `to_ycbcr` 给出的是 ±128 那套量纲，所以要加回 128 才是常见的 Cb/Cr 数值。
pub(super) fn skin_weight(cb: f32, cr: f32) -> f32 {
    let b = cb + 128.0;
    let r = cr + 128.0;
    let soft = |v: f32, lo: f32, hi: f32| {
        if v <= lo || v >= hi {
            0.0
        } else {
            ((v - lo) / 12.0).min(1.0) * ((hi - v) / 12.0).min(1.0)
        }
    };
    soft(b, 77.0, 127.0) * soft(r, 133.0, 173.0)
}

/// 匀肤：把 Cb/Cr 往大尺度那一层搬，亮度与细节都不动。
/// 只在肤色域里做——蓝天与白墙的色度也照搬就会整片发灰。
pub(super) fn even_tone_chroma(w: usize, h: usize, cb: &mut [f32], cr: &mut [f32], msk: &Option<Vec<f32>>, strength: f32) {
    let low_b = Plane { w, h, v: cb.to_vec() }.into_box_blur_passes(EVEN_TONE_RADIUS, 3);
    let low_r = Plane { w, h, v: cr.to_vec() }.into_box_blur_passes(EVEN_TONE_RADIUS, 3);
    for i in 0..cb.len() {
        // 门限读搬动前的色度：先取值再写，不然第二格用到的就是被第一格改过的
        let (b0, r0) = (cb[i], cr[i]);
        let k = skin_weight(b0, r0) * m_at(msk, i) * strength * EVEN_TONE_MAX;
        if k <= 1e-3 {
            continue;
        }
        cb[i] = b0 + (low_b.v[i] - b0) * k;
        cr[i] = r0 + (low_r.v[i] - r0) * k;
    }
}

/// 去油光：肤色域里超过 `SHINE_FLOOR` 的那一截高光按比例压回线上，线以下不动。
/// 压的是"超出量"而不是整块调暗，所以 T 区的油光掉了而鼻梁的立体感还在。
pub(super) fn de_shine_luma(y: &mut [f32], cb: &[f32], cr: &[f32], msk: &Option<Vec<f32>>, strength: f32) {
    for i in 0..y.len() {
        let over = y[i] - SHINE_FLOOR;
        if over <= 0.0 {
            continue;
        }
        let k = skin_weight(cb[i], cr[i]) * m_at(msk, i) * strength;
        if k <= 1e-3 {
            continue;
        }
        let target = SHINE_FLOOR + over * SHINE_KEEP;
        y[i] += (target - y[i]) * k;
    }
}
