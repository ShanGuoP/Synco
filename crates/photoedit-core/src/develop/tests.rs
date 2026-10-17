#![cfg(test)]
use super::params::{Local, MaskKind};
use crate::{apply_chain, Chain, EditOps};
use px_core::Rgba;
fn card() -> Rgba {
    let mut img = Rgba::new(64, 48);
    for (y, row) in img.px.chunks_exact_mut(64 * 4).enumerate() {
        for (x, p) in row.chunks_exact_mut(4).enumerate() {
            p.copy_from_slice(&[(x * 4) as u8, (y * 5) as u8, 100, 210]);
        }
    }
    img
}
#[test]
fn development_defaults_preserve_legacy_and_dormant_state_roundtrips() {
    let mut o = EditOps::default();
    let img = card();
    assert_eq!(apply_chain(&img, &o, &Chain::default()), img);
    assert!(!o.to_json().contains("develop"));
    o.develop.locals.push(Local::default());
    assert!(o.is_identity());
    let parsed = EditOps::parse(&o.to_json()).unwrap().0;
    assert_eq!(o, parsed);
    assert_eq!(parsed.develop.locals.len(), 1);
}
#[test]
fn curves_are_shape_preserving_and_channel_specific() {
    let p = [[0.0, 0.0], [0.25, 0.8], [0.7, 0.85], [1.0, 1.0]];
    let mut last = 0.0;
    for i in 0..1000 {
        let v = super::tone::curve(&p, i as f32 / 999.0, -1.0);
        assert!(v >= last - 1e-6);
        last = v;
    }
    let img = card();
    let mut o = EditOps::default();
    o.develop.look.curves.red = vec![[0.0, 0.1], [1.0, 1.0]];
    let out = apply_chain(&img, &o, &Chain::default());
    assert_ne!(out, img);
    for (a, b) in img.px.chunks_exact(4).zip(out.px.chunks_exact(4)) {
        assert_eq!(a[3], b[3]);
        assert!((a[1] as i32 - b[1] as i32).abs() <= 1);
        assert!((a[2] as i32 - b[2] as i32).abs() <= 1);
    }
}
#[test]
fn local_masks_preserve_exterior_and_follow_geometry() {
    let img = card();
    let mut o = EditOps::default();
    let mut l = Local::default();
    l.look.basic.brightness.0 = 70;
    l.mask.radius = [0.15, 0.15];
    o.develop.locals.push(l);
    let out = apply_chain(&img, &o, &Chain::default());
    assert_eq!(&out.px[..4], &img.px[..4]);
    assert_ne!(out, img);
    o.geometry.rotate_deg = 90.0;
    let rotated = apply_chain(&img, &o, &Chain::default());
    let expected = crate::geometry::apply(&out, &o.geometry);
    assert_eq!(rotated, expected);
    o.develop.locals[0].mask.kind = MaskKind::Brush;
    let blank = apply_chain(&img, &o, &Chain::default());
    assert_eq!(blank, crate::geometry::apply(&img, &o.geometry));
}
#[test]
fn all_development_stages_change_pixels_and_preserve_alpha() {
    let img = card();
    for stage in 0..10 {
        let mut o = EditOps::default();
        let d = &mut o.develop;
        match stage {
            0 => d.look.hsl[0].hue.0 = 80,
            1 => d.look.curves.hue_sat = vec![[0.0, 0.8], [0.5, 0.3], [1.0, 0.8]],
            2 => {
                d.look.grading.shadows.hue = 220;
                d.look.grading.shadows.saturation.0 = 70;
            }
            3 => d.detail.luma.0 = 80,
            4 => d.detail.chroma.0 = 80,
            5 => d.lens.distortion.0 = 50,
            6 => d.lens.chromatic.0 = 80,
            7 => d.effects.vignette.0 = -80,
            8 => d.effects.grain.0 = 80,
            _ => d.effects.film.0 = 80,
        }
        let out = apply_chain(&img, &o, &Chain::default());
        assert_ne!(out, img, "stage {stage}");
        assert_eq!(out, apply_chain(&img, &o, &Chain::default()));
        assert!(out.px.chunks_exact(4).all(|p| p[3] == 210));
    }
}
#[test]
fn bounded_parameters_clamp_and_report() {
    let raw = r#"{"develop":{"look":{"basic":{"whites":900},"curves":{"rgb":[[1,0],[0,1],[0,0.4]]}},"locals":[{"mask":{"radius":[0,9],"center":[-1,2]}}]}}"#;
    let (o, r) = EditOps::parse(raw).unwrap();
    assert!(!r.is_empty());
    assert_eq!(o.develop.look.basic.whites.0, 100);
    assert_eq!(o.develop.locals[0].mask.center, [0.0, 1.0]);
    assert_eq!(o.develop.look.curves.rgb.len(), 2);
    assert!(EditOps::parse(r#"{"develop":{"locals":4}}"#).is_err());
}

#[test]
fn brush_erasing_inversion_and_disabled_layers_have_literal_masks() {
    use super::params::{Mask, MaskStroke};
    let mut m = Mask {
        kind: MaskKind::Brush,
        feather: crate::Slider(0),
        ..Default::default()
    };
    m.strokes.push(MaskStroke {
        points: vec![[0.2, 0.5], [0.8, 0.5]],
        radius: 0.1,
        erase: false,
    });
    let before = super::mask::raster(100, 100, &m);
    assert_eq!(before.v[50 * 100 + 50], 255);
    assert_eq!(before.v[0], 0);
    m.strokes.push(MaskStroke {
        points: vec![[0.5, 0.5]],
        radius: 0.04,
        erase: true,
    });
    let erased = super::mask::raster(100, 100, &m);
    assert_eq!(erased.v[50 * 100 + 50], 0);
    assert_eq!(erased.v[50 * 100 + 25], 255);
    m.invert = true;
    let inverted = super::mask::raster(100, 100, &m);
    for (a, b) in erased.v.iter().zip(inverted.v) {
        assert_eq!(*a as u16 + b as u16, 255);
    }
    let img = card();
    let mut o = EditOps::default();
    let mut local = Local::default();
    local.enabled = false;
    local.color.exposure.0 = 100;
    o.develop.locals.push(local);
    assert!(o.is_identity());
    assert_eq!(apply_chain(&img, &o, &Chain::default()), img);
}

#[test]
fn white_balance_and_automatic_tone_follow_the_sample_not_current_slider_values() {
    let mut img = Rgba::new(12, 12);
    for p in img.px.chunks_exact_mut(4) {
        p.copy_from_slice(&[145, 125, 112, 255]);
    }
    let (c, b) = super::automatic::analyze(&img, Some([0.5, 0.5]));
    assert!(b.is_none());
    assert!(c.temp.0 < 0);
    let before = (145.0f32 - 112.0).abs();
    let after = crate::color::apply(&img, &c);
    let p = &after.px[..4];
    assert!((p[0] as f32 - p[2] as f32).abs() < before);
    for p in img.px.chunks_exact_mut(4) {
        p.copy_from_slice(&[30, 30, 30, 255]);
    }
    let (c, b) = super::automatic::analyze(&img, None);
    assert!(c.exposure.0 > 0);
    assert!(b.is_some());
}

#[test]
fn tetrahedral_lut_handles_all_tetrahedra_and_nonstandard_domains() {
    let cube="LUT_3D_SIZE 2\nDOMAIN_MIN 0.2 0.2 0.2\nDOMAIN_MAX 0.8 0.8 0.8\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
    let lut = crate::parse_cube(cube).unwrap();
    for c in [
        [0.0, 0.0, 0.0],
        [255.0, 255.0, 255.0],
        [80.0, 110.0, 170.0],
        [170.0, 110.0, 80.0],
        [110.0, 170.0, 80.0],
        [80.0, 170.0, 110.0],
        [110.0, 80.0, 170.0],
        [170.0, 80.0, 110.0],
    ] {
        let linear = lut.map(c[0], c[1], c[2]);
        let tetra = lut.map_tetra(c[0], c[1], c[2]);
        for k in 0..3 {
            assert!((linear[k] - tetra[k]).abs() < 0.001);
            assert!((0.0..=255.0).contains(&linear[k]));
        }
    }
}
