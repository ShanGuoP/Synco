//! 色彩统计校正：把重绘区的均值/方差贴回原图，接缝才不会一眼看出是补的。
//! 只在"外扩带"（模型该原样画回来、但用户没涂）上统计，逐通道对齐 mean/σ。

use px_core::buffer::{Alpha, Rgba};
use px_core::par::{par_chunks_mut, par_reduce};
use px_core::resize::to_u8;

#[derive(Default, Clone, Copy)]
struct Acc {
    sum: [f64; 3],
    sq: [f64; 3],
    rsum: [f64; 3],
    rsq: [f64; 3],
    n: usize,
}

impl Acc {
    fn merge(mut self, o: Self) -> Self {
        for c in 0..3 {
            self.sum[c] += o.sum[c];
            self.sq[c] += o.sq[c];
            self.rsum[c] += o.rsum[c];
            self.rsq[c] += o.rsq[c];
        }
        self.n += o.n;
        self
    }
}

/// `send.a > 128 && user.a < 8` 的像素进统计；样本不足 256 个退化为不校。
/// σ 比夹在 0.8~1.25，避免把噪声放大。校正作用于整张裁切区，不只统计带。
pub fn color_match(out: &mut Rgba, orig: &Rgba, send: &Alpha, user: &Alpha) {
    let n_px = out.w * out.h;
    if n_px == 0 || !out.same_size(orig) || send.w != out.w || send.h != out.h || user.w != out.w || user.h != out.h {
        return;
    }
    let acc = par_reduce(
        n_px,
        Acc::default(),
        |a, b| {
            let mut t = Acc::default();
            for i in a..b {
                if send.v[i] > 128 && user.v[i] < 8 {
                    t.n += 1;
                    for c in 0..3 {
                        let v = out.px[i * 4 + c] as f64;
                        let r = orig.px[i * 4 + c] as f64;
                        t.sum[c] += v;
                        t.sq[c] += v * v;
                        t.rsum[c] += r;
                        t.rsq[c] += r * r;
                    }
                }
            }
            t
        },
        |x, y| x.merge(y),
    );
    let n = acc.n;
    if n < 256 {
        return;
    }
    let nf = n as f64;
    let mut mo = [0.0f64; 3];
    let mut mr = [0.0f64; 3];
    let mut gain = [0.0f64; 3];
    for c in 0..3 {
        mo[c] = acc.sum[c] / nf;
        mr[c] = acc.rsum[c] / nf;
        let so = ((acc.sq[c] / nf - mo[c] * mo[c]).max(1.0)).sqrt();
        let sr = ((acc.rsq[c] / nf - mr[c] * mr[c]).max(1.0)).sqrt();
        gain[c] = (sr / so).clamp(0.8, 1.25);
    }
    par_chunks_mut(&mut out.px, out.w * 4, |blk, _row| {
        for byte in (0..blk.len()).step_by(4) {
            for c in 0..3 {
                let v = blk[byte + c] as f64;
                blk[byte + c] = to_u8(((v - mo[c]) * gain[c] + mr[c]) as f32);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: usize, h: usize, c: [u8; 3]) -> Rgba {
        let mut img = Rgba::new(w, h);
        for i in 0..w * h {
            img.px[i * 4..i * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
        img
    }

    fn band(w: usize, h: usize, ink: usize, user_no_ink: usize) -> (Alpha, Alpha) {
        // send 前 ink 个像素外扩有墨，user 前 user_no_ink 个像素被涂
        let mut send = Alpha::new(w, h);
        let mut user = Alpha::new(w, h);
        for i in 0..ink {
            send.v[i] = 255;
        }
        for i in 0..user_no_ink {
            user.v[i] = 255;
        }
        (send, user)
    }

    #[test]
    fn 样本不足不校正() {
        let mut out = flat(40, 1, [10, 20, 30]);
        let orig = flat(40, 1, [200, 200, 200]);
        let (send, user) = band(40, 1, 20, 0);
        let before = out.px.clone();
        color_match(&mut out, &orig, &send, &user);
        assert_eq!(out.px, before);
    }

    #[test]
    fn 统计带外的像素不动_带内被对齐() {
        let w = 600;
        // 前 400 是外扩带（send 有墨、user 没涂），后 200 是用户涂抹区
        let mut out = flat(w, 1, [100, 100, 100]);
        let mut orig = flat(w, 1, [150, 150, 150]);
        for i in 400..w {
            orig.px[i * 4] = 20;
        }
        let (send, mut user) = band(w, 1, w, 0);
        for i in 400..w {
            user.v[i] = 255;
        }
        color_match(&mut out, &orig, &send, &user);
        // 目标均值 150、σ 完全一致 → k=1，整张抬到 150
        assert_eq!(out.px[0], 150);
        assert_eq!(out.px[4], 150);
    }

    #[test]
    fn 标准差比被夹在_0_8_到_1_25() {
        let w = 600;
        let mut out = Rgba::new(w, 1);
        let mut orig = Rgba::new(w, 1);
        // out 的 σ=10、orig 的 σ=50，比值 5 → 必须被夹到 1.25，否则噪声会被放大 5 倍
        for i in 0..w {
            let v = if i % 2 == 0 { 90 } else { 110 };
            let r = if i % 2 == 0 { 50 } else { 150 };
            for c in 0..3 {
                out.px[i * 4 + c] = v;
                orig.px[i * 4 + c] = r;
            }
            out.px[i * 4 + 3] = 255;
            orig.px[i * 4 + 3] = 255;
        }
        let mut send = Alpha::new(w, 1);
        send.v.iter_mut().for_each(|x| *x = 255);
        let user = Alpha::new(w, 1);
        color_match(&mut out, &orig, &send, &user);
        // 均值抬到 100，幅度只按 1.25 放大：90→87.5→88、110→112.5→112（tie-to-even）
        assert_eq!(out.px[0], 88);
        assert_eq!(out.px[4], 112);
        assert_eq!(out.px[3], 255, "alpha 不该被动");
    }
}
