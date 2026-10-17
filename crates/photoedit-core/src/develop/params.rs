//! Development parameters. Coordinates for local masks belong to the original photo.
use crate::ops::{Color, Slider};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Basic {
    pub brightness: Slider,
    pub whites: Slider,
    pub blacks: Slider,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Band {
    pub hue: Slider,
    pub saturation: Slider,
    pub luminance: Slider,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Curves {
    pub rgb: Vec<[f32; 2]>,
    pub red: Vec<[f32; 2]>,
    pub green: Vec<[f32; 2]>,
    pub blue: Vec<[f32; 2]>,
    /// Hue curves use y=0.5 as neutral; x wraps around the hue circle.
    pub hue_hue: Vec<[f32; 2]>,
    pub hue_sat: Vec<[f32; 2]>,
    pub hue_lum: Vec<[f32; 2]>,
}
impl Curves {
    pub fn is_empty(&self) -> bool {
        [&self.rgb, &self.red, &self.green, &self.blue]
            .iter()
            .all(|p| p.iter().all(|v| (v[0] - v[1]).abs() < 1e-6))
            && [&self.hue_hue, &self.hue_sat, &self.hue_lum]
                .iter()
                .all(|p| p.iter().all(|v| (v[1] - 0.5).abs() < 1e-6))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grade {
    pub hue: u16,
    pub saturation: Slider,
    pub luminance: Slider,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grading {
    pub shadows: Grade,
    pub midtones: Grade,
    pub highlights: Grade,
    pub balance: Slider,
    pub blending: Slider,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Look {
    pub basic: Basic,
    pub curves: Curves,
    pub hsl: [Band; 8],
    pub grading: Grading,
}
impl Look {
    pub fn is_empty(&self) -> bool {
        self.basic == Basic::default()
            && self.curves.is_empty()
            && self.hsl.iter().all(|b| *b == Band::default())
            && [
                &self.grading.shadows,
                &self.grading.midtones,
                &self.grading.highlights,
            ]
            .iter()
            .all(|g| g.saturation.is_zero() && g.luminance.is_zero())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detail {
    pub luma: Slider,
    pub chroma: Slider,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lens {
    pub distortion: Slider,
    pub chromatic: Slider,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Effects {
    pub vignette: Slider,
    pub midpoint: Slider,
    pub feather: Slider,
    pub grain: Slider,
    pub size: Slider,
    pub roughness: Slider,
    pub film: Slider,
    pub seed: u32,
}
impl Default for Effects {
    fn default() -> Self {
        Self {
            vignette: Slider(0),
            midpoint: Slider(50),
            feather: Slider(70),
            grain: Slider(0),
            size: Slider(25),
            roughness: Slider(50),
            film: Slider(0),
            seed: 1,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaskKind {
    #[default]
    Radial,
    Linear,
    Brush,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MaskStroke {
    pub points: Vec<[f32; 2]>,
    pub radius: f32,
    pub erase: bool,
}
impl Default for MaskStroke {
    fn default() -> Self {
        Self {
            points: Vec::new(),
            radius: 0.05,
            erase: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mask {
    pub kind: MaskKind,
    pub center: [f32; 2],
    pub end: [f32; 2],
    pub radius: [f32; 2],
    pub feather: Slider,
    pub invert: bool,
    pub strokes: Vec<MaskStroke>,
}
impl Default for Mask {
    fn default() -> Self {
        Self {
            kind: MaskKind::Radial,
            center: [0.5, 0.5],
            end: [0.75, 0.5],
            radius: [0.3, 0.3],
            feather: Slider(70),
            invert: false,
            strokes: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Local {
    pub enabled: bool,
    pub mask: Mask,
    pub color: Color,
    pub look: Look,
}
impl Default for Local {
    fn default() -> Self {
        Self {
            enabled: true,
            mask: Mask::default(),
            color: Color::default(),
            look: Look::default(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Develop {
    pub look: Look,
    pub detail: Detail,
    pub lens: Lens,
    pub effects: Effects,
    pub locals: Vec<Local>,
    pub lut_tetra: bool,
}
impl Develop {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub fn is_empty(&self) -> bool {
        self.look.is_empty()
            && self.detail == Detail::default()
            && self.lens == Lens::default()
            && self.effects.vignette.is_zero()
            && self.effects.grain.is_zero()
            && self.effects.film.is_zero()
            && self
                .locals
                .iter()
                .all(|l| !l.enabled || (l.color.is_empty() && l.look.is_empty()))
    }
    pub fn clamp(&mut self, rep: &mut Vec<String>) {
        clamp_look(&mut self.look, rep);
        for s in [
            &mut self.detail.luma,
            &mut self.detail.chroma,
            &mut self.effects.midpoint,
            &mut self.effects.feather,
            &mut self.effects.grain,
            &mut self.effects.size,
            &mut self.effects.roughness,
            &mut self.effects.film,
        ] {
            slider(s, 0, 100, rep);
        }
        for s in [
            &mut self.lens.distortion,
            &mut self.lens.chromatic,
            &mut self.effects.vignette,
        ] {
            slider(s, -100, 100, rep);
        }
        if self.locals.len() > 16 {
            self.locals.truncate(16);
            rep.push("develop.locals".into());
        }
        for l in &mut self.locals {
            clamp_look(&mut l.look, rep);
            for s in [
                &mut l.color.exposure,
                &mut l.color.contrast,
                &mut l.color.highlights,
                &mut l.color.shadows,
                &mut l.color.temp,
                &mut l.color.tint,
                &mut l.color.saturation,
                &mut l.color.vibrance,
                &mut l.color.clarity,
                &mut l.color.sharpen,
            ] {
                slider(s, -100, 100, rep);
            }
            for p in [&mut l.mask.center, &mut l.mask.end] {
                for v in p {
                    number(v, 0.0, 1.0, rep);
                }
            }
            for v in &mut l.mask.radius {
                number(v, 0.005, 1.5, rep);
            }
            slider(&mut l.mask.feather, 0, 100, rep);
            if l.mask.strokes.len() > 128 {
                l.mask.strokes.truncate(128);
                rep.push("develop.strokes".into());
            }
            let mut remaining = 4096;
            for stroke in &mut l.mask.strokes {
                number(&mut stroke.radius, 0.002, 0.5, rep);
                if stroke.points.len() > remaining {
                    stroke.points.truncate(remaining);
                    rep.push("develop.points".into());
                }
                remaining -= stroke.points.len();
                for p in &mut stroke.points {
                    for v in p {
                        number(v, 0.0, 1.0, rep);
                    }
                }
            }
        }
    }
}
fn slider(s: &mut Slider, lo: i32, hi: i32, rep: &mut Vec<String>) {
    let n = s.0.clamp(lo, hi);
    if n != s.0 {
        rep.push("develop.slider".into());
        s.0 = n;
    }
}
fn number(v: &mut f32, lo: f32, hi: f32, rep: &mut Vec<String>) {
    let n = if v.is_finite() { v.clamp(lo, hi) } else { lo };
    if n != *v {
        rep.push("develop.coordinate".into());
        *v = n;
    }
}
fn clamp_look(l: &mut Look, rep: &mut Vec<String>) {
    for s in [
        &mut l.basic.brightness,
        &mut l.basic.whites,
        &mut l.basic.blacks,
    ] {
        slider(s, -100, 100, rep);
    }
    for band in &mut l.hsl {
        for s in [&mut band.hue, &mut band.saturation, &mut band.luminance] {
            slider(s, -100, 100, rep);
        }
    }
    for curve in [
        &mut l.curves.rgb,
        &mut l.curves.red,
        &mut l.curves.green,
        &mut l.curves.blue,
        &mut l.curves.hue_hue,
        &mut l.curves.hue_sat,
        &mut l.curves.hue_lum,
    ] {
        if curve.len() > 32 {
            curve.truncate(32);
            rep.push("develop.curve".into());
        }
        for p in curve.iter_mut() {
            for v in p {
                number(v, 0.0, 1.0, rep);
            }
        }
        curve.sort_by(|a, b| a[0].total_cmp(&b[0]));
        curve.dedup_by(|a, b| (a[0] - b[0]).abs() < 0.0001);
    }
    for g in [
        &mut l.grading.shadows,
        &mut l.grading.midtones,
        &mut l.grading.highlights,
    ] {
        g.hue %= 360;
        slider(&mut g.saturation, 0, 100, rep);
        slider(&mut g.luminance, -100, 100, rep);
    }
    slider(&mut l.grading.balance, -100, 100, rep);
    slider(&mut l.grading.blending, 0, 100, rep);
}
