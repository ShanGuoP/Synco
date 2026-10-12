//! 多人精修与手动人体区域参数。所有位置在源图归一化域，强度入库前统一夹逼。
use crate::ops::{Auto, Beauty, Slider};
use serde::{Deserialize, Serialize};

macro_rules! sliders {
    ($name:ident, $min:expr, $($field:ident),+ $(,)?) => {
        #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct $name { $(pub $field: Slider,)+ }
        impl $name {
            pub fn is_empty(&self) -> bool { true $(&& self.$field.is_zero())+ }
            fn clamp(&mut self, prefix: &str, report: &mut Vec<String>) {
                $(let old = self.$field.0; self.$field.0 = old.clamp($min, 100);
                if old != self.$field.0 { report.push(format!("{prefix}.{}", stringify!($field))); })+
            }
        }
    }
}
sliders!(Shape, -100, cheekbone, jaw_width, face_width, face_length, chin_length,
    eye_distance, eye_height, eye_tilt, nose_tip, nose_length, mouth_width, upper_lip, lower_lip);
sliders!(Retouch, 0, dark_circles, eye_bags, teeth, eye_light, rosy, redness, yellow);
sliders!(Makeup, 0, blush, lipstick, brows, gloss);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FaceEdit {
    pub idx: usize,
    pub auto: Auto,
    pub shape: Shape,
    pub beauty: Beauty,
    pub retouch: Retouch,
    pub makeup: Makeup,
    pub lip_color: [u8; 3],
    pub blush_color: [u8; 3],
    pub protect_features: bool,
    pub protect_background: bool,
}
impl Default for FaceEdit {
    fn default() -> Self {
        Self { idx: 0, auto: Auto::default(), shape: Shape::default(), beauty: Beauty::default(),
            retouch: Retouch::default(), makeup: Makeup::default(), lip_color: [180, 65, 85],
            blush_color: [220, 110, 125], protect_features: true, protect_background: true }
    }
}
impl FaceEdit {
    pub fn is_empty(&self) -> bool {
        self.auto.is_empty() && self.shape.is_empty() && self.beauty.is_empty()
            && self.retouch.is_empty() && self.makeup.is_empty()
    }
}
sliders!(BodyStrength, -100, waist, shoulders, arms, legs, long_legs);
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Body {
    /// 手动校准的完整人体框；不根据人脸臆测身体位置。
    pub region: [f32; 4],
    pub strength: BodyStrength,
    /// 肩、腰、胯、膝在人体框内的纵向比例。
    pub levels: [f32; 4],
}
impl Default for Body {
    fn default() -> Self {
        Self { region: [0.2, 0.05, 0.6, 0.9], strength: BodyStrength::default(), levels: [0.2, 0.43, 0.55, 0.76] }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Portrait {
    pub faces: Vec<FaceEdit>,
    pub body: Body,
}
impl Portrait {
    pub fn is_empty(&self) -> bool { self.faces.iter().all(FaceEdit::is_empty) && self.body.strength.is_empty() }
    pub fn needs_faces(&self) -> bool { self.faces.iter().any(|f| !f.is_empty()) }
    pub fn clamp(&mut self, report: &mut Vec<String>) {
        if self.faces.len()>16 {report.push("portrait.faces".into());self.faces.truncate(16);}
        // 每张脸只有一份权威参数，重复编号保留首份。
        let mut seen = std::collections::HashSet::new();
        self.faces.retain(|f| {let keep=f.idx<16 && seen.insert(f.idx);if !keep {report.push("portrait.faces".into());}keep});
        self.faces.sort_by_key(|f| f.idx);
        for f in &mut self.faces {
            let prefix = format!("portrait.faces.{}", f.idx);
            crate::ops::clamp_auto(&mut f.auto,&format!("{prefix}.auto"),report);
            crate::ops::clamp_beauty(&mut f.beauty,&format!("{prefix}.beauty"),report);
            f.shape.clamp(&format!("{prefix}.shape"), report);
            f.retouch.clamp(&format!("{prefix}.retouch"), report);
            f.makeup.clamp(&format!("{prefix}.makeup"), report);
        }
        self.body.strength.clamp("portrait.body.strength", report);
        for (i, v) in self.body.region.iter_mut().enumerate() {
            let old = *v;
            *v = if v.is_finite() { v.clamp(if i < 2 { 0.0 } else { 0.02 }, 1.0) } else { [0.2, 0.05, 0.6, 0.9][i] };
            if old != *v { report.push("portrait.body.region".into()); }
        }
        for a in 0..2 { let old=self.body.region[a];self.body.region[a] = old.min(1.0 - self.body.region[a+2]);if old!=self.body.region[a] {report.push("portrait.body.region".into());} }
        for (i, v) in self.body.levels.iter_mut().enumerate() {
            let old = *v;
            *v = if v.is_finite() { v.clamp(0.05, 0.95) } else { [0.2, 0.43, 0.55, 0.76][i] };
            if old != *v { report.push("portrait.body.levels".into()); }
        }
        let old=self.body.levels;self.body.levels.sort_by(f32::total_cmp);
        if old!=self.body.levels {report.push("portrait.body.levels".into());}
    }
}

/// 检测索引和点必须一起传递，缺点的脸不能造成后面编号移位。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mesh { pub idx: usize, pub points: Vec<[f32; 2]> }

#[cfg(test)]
mod tests {
    #[test]
    fn portrait_parameters_roundtrip_clamp_and_invalidate_identity() {
        let raw=r#"{"portrait":{"faces":[{"idx":2,"shape":{"mouth_width":-300},"retouch":{"teeth":250},"makeup":{"lipstick":35},"beauty":{"smooth":40},"lip_color":[150,60,80]},{"idx":2},{"idx":99}],"body":{"region":[0.8,0.8,0.6,0.6],"strength":{"waist":-500},"levels":[0.9,0.2,0.6,0.5]}}}"#;
        let (ops,rep)=crate::EditOps::parse(raw).unwrap(); assert!(!ops.is_identity());assert!(!rep.is_empty());
        assert_eq!(ops.portrait.faces.len(),1);assert_eq!(ops.portrait.faces[0].shape.mouth_width.0,-100);
        assert_eq!(ops.portrait.faces[0].retouch.teeth.0,100); assert_eq!(ops.portrait.body.strength.waist.0,-100);
        assert_eq!(crate::EditOps::parse(&ops.to_json()).unwrap().0,ops);
        assert!(crate::EditOps::parse(r#"{"portrait":{"faces":"bad"}}"#).is_err());
        assert!(crate::EditOps::parse(r#"{"portrait":{"faces":[{"lip_color":[-1,0,0]}]}}"#).is_err());
    }
}
