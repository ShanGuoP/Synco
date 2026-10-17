//! Encoded-sRGB histograms and bounded clipping maps from authoritative working pixels.
use px_core::Rgba;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Histogram {
    pub version: u8,
    pub red: Vec<u64>,
    pub green: Vec<u64>,
    pub blue: Vec<u64>,
    pub luminance: Vec<u64>,
    pub pixels: u64,
    pub shadows: u64,
    pub highlights: u64,
    pub width: usize,
    pub height: usize,
    pub map_width: usize,
    pub map_height: usize,
    pub clipping: Vec<u8>,
}
impl Histogram {
    pub fn valid(&self) -> bool {
        self.version == 1
            && [&self.red, &self.green, &self.blue, &self.luminance]
                .iter()
                .all(|v| v.len() == 256)
            && (1..=320).contains(&self.map_width)
            && (1..=320).contains(&self.map_height)
            && self.clipping.len() == self.map_width * self.map_height
    }
}
pub fn compute(img: &Rgba) -> Histogram {
    let scale = (320.0 / img.w.max(img.h).max(1) as f32).min(1.0);
    let mw = ((img.w as f32 * scale).round() as usize).max(1);
    let mh = ((img.h as f32 * scale).round() as usize).max(1);
    let mut out = Histogram {
        version: 1,
        red: vec![0; 256],
        green: vec![0; 256],
        blue: vec![0; 256],
        luminance: vec![0; 256],
        pixels: 0,
        shadows: 0,
        highlights: 0,
        width: img.w,
        height: img.h,
        map_width: mw,
        map_height: mh,
        clipping: vec![0; mw * mh],
    };
    for (i, p) in img.px.chunks_exact(4).enumerate() {
        if p[3] == 0 {
            continue;
        }
        out.pixels += 1;
        out.red[p[0] as usize] += 1;
        out.green[p[1] as usize] += 1;
        out.blue[p[2] as usize] += 1;
        let y = crate::px::luma(p[0] as f32, p[1] as f32, p[2] as f32)
            .round()
            .clamp(0.0, 255.0) as usize;
        out.luminance[y] += 1;
        let shadow = p[..3].iter().all(|v| *v == 0);
        let highlight = p[..3].contains(&255);
        out.shadows += shadow as u64;
        out.highlights += highlight as u64;
        if shadow || highlight {
            let x = (i % img.w) * mw / img.w;
            let y = (i / img.w) * mh / img.h;
            out.clipping[y * mw + x] |= shadow as u8 | ((highlight as u8) << 1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::compute;
    use px_core::Rgba;
    #[test]
    fn bins_clipping_and_alpha_are_literal() {
        let mut img = Rgba::new(4, 1);
        img.px = vec![
            0, 0, 0, 255, 255, 30, 80, 255, 40, 90, 120, 0, 255, 255, 255, 128,
        ];
        let h = compute(&img);
        assert!(h.valid());
        assert_eq!(h.pixels, 3);
        assert_eq!(h.shadows, 1);
        assert_eq!(h.highlights, 2);
        assert_eq!(h.red[255], 2);
        assert_eq!(h.blue[120], 0);
        assert_eq!(h.clipping, vec![1, 2, 0, 2]);
        for b in [&h.red, &h.green, &h.blue, &h.luminance] {
            assert_eq!(b.iter().sum::<u64>(), 3);
        }
    }
    #[test]
    fn clipping_map_preserves_sparse_extremes_and_is_bounded() {
        let mut img = Rgba::new(2400, 1600);
        for p in img.px.chunks_exact_mut(4) {
            p.copy_from_slice(&[110, 90, 80, 255]);
        }
        img.set(1234, 777, [255, 80, 30, 255]);
        let h = compute(&img);
        assert_eq!(h.highlights, 1);
        assert_eq!(h.map_width, 320);
        assert!(h.clipping.contains(&2));
        assert!(h.valid());
        let empty = compute(&Rgba::new(0, 0));
        assert_eq!(empty.pixels, 0);
        assert!(empty.valid());
    }
}
