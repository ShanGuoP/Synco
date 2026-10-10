//! 调整参数的形状、取值域与"越界夹逼"。
//!
//! 空间类参数一律归一化：位置按各轴边长、半径按几何均边（`√(w·h)`）。
//! 这样同一份参数在 320 / proxy / 原图三档上落在同一相对位置——
//! 本项目三档共用宽高比，归一化后画笔在每档都是正圆且覆盖同样的画面比例，
//! 预览与成图的差别只剩重采样本身。

use serde::{Deserialize, Serialize};

/// 滑杆统一量程：调色与一键塑形都是 -100~100，0 = 不动
pub const SLIDER_MIN: i32 = -100;
pub const SLIDER_MAX: i32 = 100;
/// 半径归一化下限：小于一颗像素的盘没有意义
pub const RADIUS_MIN: f32 = 0.002;
/// 半径归一化上限：超过这个盘就是全局变形，不是笔刷
pub const RADIUS_MAX: f32 = 0.5;

#[inline]
fn clamp_i(v: i64, lo: i32, hi: i32, field: &str, rep: &mut Vec<String>) -> i32 {
    let v = if v < lo as i64 {
        rep.push(field.to_string());
        lo as i64
    } else if v > hi as i64 {
        rep.push(field.to_string());
        hi as i64
    } else {
        v
    };
    v as i32
}

/// 一个滑杆字段：serde 收 i64（JSON 里可能是 300 这类越界值），落库前夹到量程内
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Slider(pub i32);

impl Default for Slider {
    fn default() -> Self {
        Slider(0)
    }
}

impl Slider {
    /// 归一化到 -1..1，算子内部按各自曲线映射
    #[inline]
    pub fn norm(self) -> f32 {
        self.0 as f32 / 100.0
    }

    /// 非负量程（美颜强度、笔刷压力）：负值按 0 处理
    #[inline]
    pub fn pos(self) -> f32 {
        self.norm().max(0.0)
    }

    #[inline]
    pub fn is_zero(self) -> bool {
        self.0 == 0
    }
}

/// 补边策略：微调旋转后画面外的像素从哪来
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fill {
    /// 边缘延伸（默认）：不引入新颜色，构图中最不容易被看出来
    #[default]
    Edge,
    /// 纯色填充，取源图四角均值：需要"留白"效果时用
    Avg,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Geometry {
    /// 归一化裁切框 `[x, y, w, h]`；`null` = 不裁
    pub crop: Option<[f32; 4]>,
    /// 旋转角度：90 的整数倍走无损象限变换，余下的小角度走双线性重采样
    pub rotate_deg: f32,
    pub flip_h: bool,
    pub flip_v: bool,
    pub fill: Fill,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    /// 推挤：顺着拖动方向把像素推走
    #[default]
    Push,
    /// 收缩：朝盘心吸
    Pucker,
    /// 膨胀：自盘心向外胀
    Bloat,
    /// 恢复：向未变形的原图回拉
    Restore,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stroke {
    pub tool: Tool,
    /// 归一化轨迹点（按各轴边长）；入库前应过 [`crate::warp::simplify`] 抽稀
    pub points: Vec<[f32; 2]>,
    /// 盘半径，归一化到几何均边
    pub radius: f32,
    /// 压力/强度 -100~100，映射为每步位移占半径的比例
    pub strength: Slider,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Auto {
    pub face_slim: Slider,
    pub eye_big: Slider,
    pub nose_slim: Slider,
    pub chin: Slider,
}

impl Auto {
    pub fn is_empty(&self) -> bool {
        self.face_slim.is_zero() && self.eye_big.is_zero() && self.nose_slim.is_zero() && self.chin.is_zero()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Warp {
    pub strokes: Vec<Stroke>,
    pub auto: Auto,
}

/// 调色滑杆组。全部 -100~100，算子内部各自换算
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Color {
    pub exposure: Slider,
    pub contrast: Slider,
    pub highlights: Slider,
    pub shadows: Slider,
    pub temp: Slider,
    pub tint: Slider,
    pub saturation: Slider,
    pub vibrance: Slider,
    pub clarity: Slider,
    pub sharpen: Slider,
    /// 预设名：预设只是参数包，命中时前端把滑杆刷一遍，服务端不额外做处理
    pub preset: Option<String>,
}

impl Color {
    pub fn is_empty(&self) -> bool {
        self.exposure.is_zero()
            && self.contrast.is_zero()
            && self.highlights.is_zero()
            && self.shadows.is_zero()
            && self.temp.is_zero()
            && self.tint.is_zero()
            && self.saturation.is_zero()
            && self.vibrance.is_zero()
            && self.clarity.is_zero()
            && self.sharpen.is_zero()
    }
}

/// 美颜：磨皮/质感/祛瑕疵/匀肤/美白/去油光/锐化，量程 0~100
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Beauty {
    pub smooth: Slider,
    /// 高频回填的下限。0 = 沿用磨皮强度自己那条曲线（与 0.3.0 逐位一致）
    pub texture: Slider,
    /// 点状瑕疵回收（软阈值高通），只在涂过的地方最准
    pub blemish: Slider,
    /// 色度往大尺度低频搬：去红绿不均与成片色斑
    pub even_tone: Slider,
    pub brighten: Slider,
    /// 肤色域内超线高光往回压（去油光）
    pub de_shine: Slider,
    pub sharpen: Slider,
    /// true = 只在已涂的蒙版区域内生效；false = 全图
    pub by_mask: bool,
}

impl Beauty {
    /// `texture` 不在内：它只是磨皮曲线的下限，`smooth=0` 时它一根像素都不碰。
    /// 把它算进来会让"只拖了质感没拖磨皮"这套参数被判成非恒等，白渲染一张预览。
    pub fn is_empty(&self) -> bool {
        self.smooth.is_zero()
            && self.blemish.is_zero()
            && self.even_tone.is_zero()
            && self.brighten.is_zero()
            && self.de_shine.is_zero()
            && self.sharpen.is_zero()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lut {
    /// `data/luts/` 下的文件名（不含路径分隔符，服务端还要再过一道白名单）
    pub name: String,
    pub strength: Slider,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditOps {
    /// 参数格式版本，留演进余地；未知版本按字段名尽力解析
    pub v: u32,
    pub geometry: Geometry,
    pub warp: Warp,
    pub color: Color,
    pub beauty: Beauty,
    pub lut: Option<Lut>,
}

/// `v` 的默认值就是当前版本号：新起的参数对象序列化出去要能被后续版本认出来
impl Default for EditOps {
    fn default() -> Self {
        Self { v: 1, geometry: Geometry::default(), warp: Warp::default(), color: Color::default(), beauty: Beauty::default(), lut: None }
    }
}

/// 全默认那一份 JSON 现算，不再手写。手写这份漂过一次：0.3 给美颜加了四个滑杆（texture / blemish /
/// even_tone / de_shine），常量里的 `beauty` 段还停在三个。它被 `to_json()` 当序列化失败的兜底，
/// 也当测试里"这就是默认值"的输入——所以由 `EditOps::default()` 算出来，加字段不需要再改第二处。
static DEFAULT_JSON: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn default_json() -> &'static str {
    DEFAULT_JSON.get_or_init(|| serde_json::to_string(&EditOps::default()).unwrap_or_else(|_| "{}".into()))
}

impl EditOps {
    /// 解析 + 夹逼。返回 `(参数, 被夹过的字段名)`；结构不对直接报字符串。
    pub fn parse(raw: &str) -> Result<(EditOps, Vec<String>), String> {
        // 这里只把 serde 的原文带出去：那句"参数不是合法 JSON"属于界面文案，在服务端的字典里
        let v: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        let mut rep = Vec::new();
        let ops = read_ops(&v, &mut rep);
        Ok((ops, rep))
    }

    /// 落库用的紧凑 JSON（字段序固定，判新与 ETag 都依赖它稳定）
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| default_json().to_string())
    }

    pub fn is_identity(&self) -> bool {
        let g = &self.geometry;
        let crop_default = match g.crop {
            Some([x, y, w, h]) => x == 0.0 && y == 0.0 && (w - 1.0).abs() <= f32::EPSILON && (h - 1.0).abs() <= f32::EPSILON,
            None => true,
        };
        crop_default
            && g.rotate_deg == 0.0
            && !g.flip_h
            && !g.flip_v
            && self.warp.strokes.iter().all(|s| s.points.is_empty())
            && self.warp.auto.is_empty()
            && self.color.is_empty()
            && self.beauty.is_empty()
            && self.lut.is_none()
    }

    /// 越界项夹回量程，同时把裁切框收进画面。**就地修改**，返回被改过的字段名。
    pub fn clamp(&mut self) -> Vec<String> {
        let mut rep = Vec::new();
        clamp_ops(self, &mut rep);
        rep
    }
}

fn clamp_ops(o: &mut EditOps, rep: &mut Vec<String>) {
    if !o.geometry.rotate_deg.is_finite() {
        rep.push("geometry.rotate_deg".to_string());
    }
    o.geometry.rotate_deg = o.geometry.rotate_deg.clamp(-180.0, 180.0);
    if let Some(c) = o.geometry.crop.as_mut() {
        if !c.iter().all(|v| v.is_finite()) {
            rep.push("geometry.crop".to_string());
        }
        for x in c.iter_mut() {
            *x = x.clamp(0.0, 1.0);
        }
        // 右下角越界整体内移，保持框的尺寸尽量不变
        let over_x = c[0] + c[2] > 1.0;
        let over_y = c[1] + c[3] > 1.0;
        if over_x || over_y {
            rep.push("geometry.crop".to_string());
            if over_x {
                c[0] = (1.0 - c[2]).max(0.0);
            }
            if over_y {
                c[1] = (1.0 - c[3]).max(0.0);
            }
        }
    }
    for s in o.warp.strokes.iter_mut() {
        let raw = s.strength.0 as i64;
        s.strength.0 = clamp_i(raw, SLIDER_MIN, SLIDER_MAX, "warp.strokes.strength", rep);
        if !s.radius.is_finite() || !(RADIUS_MIN..=RADIUS_MAX).contains(&s.radius.max(RADIUS_MIN)) {
            rep.push("warp.strokes.radius".to_string());
        }
        s.radius = s.radius.clamp(RADIUS_MIN, RADIUS_MAX);
        for p in s.points.iter_mut() {
            for k in 0..2 {
                if !p[k].is_finite() {
                    rep.push("warp.strokes.points".to_string());
                }
                p[k] = p[k].clamp(0.0, 1.0);
            }
        }
    }
    let a = &mut o.warp.auto;
    a.face_slim.0 = clamp_i(a.face_slim.0 as i64, SLIDER_MIN, SLIDER_MAX, "warp.auto.face_slim", rep);
    a.eye_big.0 = clamp_i(a.eye_big.0 as i64, SLIDER_MIN, SLIDER_MAX, "warp.auto.eye_big", rep);
    a.nose_slim.0 = clamp_i(a.nose_slim.0 as i64, SLIDER_MIN, SLIDER_MAX, "warp.auto.nose_slim", rep);
    a.chin.0 = clamp_i(a.chin.0 as i64, SLIDER_MIN, SLIDER_MAX, "warp.auto.chin", rep);
    let c = &mut o.color;
    for (name, s) in [
        ("color.exposure", &mut c.exposure),
        ("color.contrast", &mut c.contrast),
        ("color.highlights", &mut c.highlights),
        ("color.shadows", &mut c.shadows),
        ("color.temp", &mut c.temp),
        ("color.tint", &mut c.tint),
        ("color.saturation", &mut c.saturation),
        ("color.vibrance", &mut c.vibrance),
        ("color.clarity", &mut c.clarity),
        ("color.sharpen", &mut c.sharpen),
    ] {
        s.0 = clamp_i(s.0 as i64, SLIDER_MIN, SLIDER_MAX, name, rep);
    }
    let b = &mut o.beauty;
    for (name, s) in [
        ("beauty.smooth", &mut b.smooth),
        ("beauty.texture", &mut b.texture),
        ("beauty.blemish", &mut b.blemish),
        ("beauty.even_tone", &mut b.even_tone),
        ("beauty.brighten", &mut b.brighten),
        ("beauty.de_shine", &mut b.de_shine),
        ("beauty.sharpen", &mut b.sharpen),
    ] {
        s.0 = clamp_i(s.0 as i64, 0, 100, name, rep);
    }
    if let Some(l) = o.lut.as_mut() {
        l.strength.0 = clamp_i(l.strength.0 as i64, 0, 100, "lut.strength", rep);
    }
}

/// 手写一遍读取而不是直接 serde：`{"color":{"exposure":300}}` 这种越界要逐字段记账，
/// 而 serde 的 `deserialize_i32` 拿到 300 不会告诉我们它出格了。
fn read_ops(v: &serde_json::Value, rep: &mut Vec<String>) -> EditOps {
    let mut o = EditOps { v: v.get("v").and_then(|x| x.as_u64()).unwrap_or(1) as u32, ..Default::default() };
    if let Some(g) = v.get("geometry") {
        o.geometry.crop = g.get("crop").and_then(|c| {
            let a = c.as_array()?;
            if a.len() != 4 {
                return None;
            }
            let f = |i: usize| a[i].as_f64().unwrap_or(0.0) as f32;
            Some([f(0), f(1), f(2), f(3)])
        });
        o.geometry.rotate_deg = g.get("rotate_deg").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        o.geometry.flip_h = g.get("flip_h").and_then(|x| x.as_bool()).unwrap_or(false);
        o.geometry.flip_v = g.get("flip_v").and_then(|x| x.as_bool()).unwrap_or(false);
        o.geometry.fill = match g.get("fill").and_then(|x| x.as_str()) {
            Some("avg") => Fill::Avg,
            _ => Fill::Edge,
        };
    }
    if let Some(w) = v.get("warp") {
        if let Some(arr) = w.get("strokes").and_then(|s| s.as_array()) {
            for s in arr {
                let tool = match s.get("tool").and_then(|t| t.as_str()) {
                    Some("pucker") => Tool::Pucker,
                    Some("bloat") => Tool::Bloat,
                    Some("restore") => Tool::Restore,
                    _ => Tool::Push,
                };
                let points = s
                    .get("points")
                    .and_then(|p| p.as_array())
                    .map(|ps| {
                        ps.iter()
                            .filter_map(|p| {
                                let a = p.as_array()?;
                                if a.len() < 2 {
                                    return None;
                                }
                                Some([a[0].as_f64().unwrap_or(0.0) as f32, a[1].as_f64().unwrap_or(0.0) as f32])
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let radius = s.get("radius").and_then(|x| x.as_f64()).unwrap_or(0.05) as f32;
                let strength = Slider(clamp_i(s.get("strength").and_then(|x| x.as_i64()).unwrap_or(0), SLIDER_MIN, SLIDER_MAX, "warp.strokes.strength", rep));
                o.warp.strokes.push(Stroke { tool, points, radius, strength });
            }
        }
        if let Some(a) = w.get("auto") {
            let mut one = |k: &str| Slider(clamp_i(a.get(k).and_then(|x| x.as_i64()).unwrap_or(0), SLIDER_MIN, SLIDER_MAX, &format!("warp.auto.{k}"), rep));
            o.warp.auto = Auto { face_slim: one("face_slim"), eye_big: one("eye_big"), nose_slim: one("nose_slim"), chin: one("chin") };
        }
    }
    if let Some(c) = v.get("color") {
        let mut one = |k: &str| Slider(clamp_i(c.get(k).and_then(|x| x.as_i64()).unwrap_or(0), SLIDER_MIN, SLIDER_MAX, &format!("color.{k}"), rep));
        o.color = Color {
            exposure: one("exposure"),
            contrast: one("contrast"),
            highlights: one("highlights"),
            shadows: one("shadows"),
            temp: one("temp"),
            tint: one("tint"),
            saturation: one("saturation"),
            vibrance: one("vibrance"),
            clarity: one("clarity"),
            sharpen: one("sharpen"),
            preset: c.get("preset").and_then(|x| x.as_str()).map(str::to_string),
        };
    }
    if let Some(b) = v.get("beauty") {
        let mut one = |k: &str| Slider(clamp_i(b.get(k).and_then(|x| x.as_i64()).unwrap_or(0), 0, 100, &format!("beauty.{k}"), rep));
        o.beauty = Beauty {
            smooth: one("smooth"),
            texture: one("texture"),
            blemish: one("blemish"),
            even_tone: one("even_tone"),
            brighten: one("brighten"),
            de_shine: one("de_shine"),
            sharpen: one("sharpen"),
            by_mask: b.get("by_mask").and_then(|x| x.as_bool()).unwrap_or(false),
        };
    }
    if let Some(l) = v.get("lut").filter(|x| !x.is_null()) {
        o.lut = Some(Lut {
            name: l.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            strength: Slider(clamp_i(l.get("strength").and_then(|x| x.as_i64()).unwrap_or(100), 0, 100, "lut.strength", rep)),
        });
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 默认参数就是恒等() {
        let (ops, rep) = EditOps::parse(default_json()).unwrap();
        assert!(rep.is_empty(), "默认参数不该被夹：{rep:?}");
        assert!(ops.is_identity());
    }

    #[test]
    fn 缺字段按默认补() {
        let (ops, rep) = EditOps::parse(r#"{"color":{"exposure":20}}"#).unwrap();
        assert!(rep.is_empty());
        assert_eq!(ops.color.exposure.0, 20);
        assert!(ops.beauty.is_empty());
        assert!(!ops.geometry.flip_h);
    }

    #[test]
    fn 越界逐项夹逼并记账() {
        let (mut ops, rep) = EditOps::parse(r#"{
            "geometry":{"crop":[0.8,0.8,0.5,0.5],"rotate_deg":900},
            "color":{"exposure":300,"contrast":-400,"clarity":5},
            "beauty":{"smooth":120},
            "warp":{"strokes":[{"tool":"push","points":[[2.0,-1.0]],"radius":9.0,"strength":500}],
                     "auto":{"face_slim":-250}}
        }"#)
        .unwrap();
        // parse 通道就夹一遍，记账要能看见
        assert!(rep.contains(&"color.exposure".to_string()), "{rep:?}");
        assert!(rep.contains(&"color.contrast".to_string()));
        assert!(rep.contains(&"beauty.smooth".to_string()));
        assert!(rep.contains(&"warp.strokes.strength".to_string()));
        assert!(rep.contains(&"warp.auto.face_slim".to_string()));
        assert_eq!(ops.color.exposure.0, 100);
        assert_eq!(ops.color.contrast.0, -100);
        assert_eq!(ops.beauty.smooth.0, 100);
        assert_eq!(ops.warp.auto.face_slim.0, -100);
        ops.clamp();
        assert_eq!(ops.geometry.rotate_deg, 180.0);
        let c = ops.geometry.crop.unwrap();
        assert_eq!(c[0], 0.5, "右下角越界要整体内移");
        assert_eq!(c[2], 0.5, "框宽保持不变");
        assert_eq!(ops.warp.strokes[0].points[0], [1.0, 0.0]);
        assert_eq!(ops.warp.strokes[0].radius, RADIUS_MAX);
        assert_eq!(ops.color.clarity.0, 5, "量程内的不该被动过");
    }

    #[test]
    fn 坏结构报错而不是崩() {
        assert!(EditOps::parse("not json").is_err());
        let (ops, _) = EditOps::parse(r#"{"color":"乱填"}"#).unwrap();
        assert!(ops.is_identity(), "字段类型不对就当没填");
    }

    #[test]
    fn 往返稳定() {
        let (ops, _) = EditOps::parse(default_json()).unwrap();
        assert_eq!(ops.to_json(), EditOps::default().to_json());
        let raw = r#"{"v":1,"geometry":{"crop":[0.1,0.2,0.3,0.4],"rotate_deg":0.0,"flip_h":true,"flip_v":false,"fill":"avg"},"warp":{"strokes":[],"auto":{"face_slim":0,"eye_big":0,"nose_slim":0,"chin":0}},"color":{"exposure":0,"contrast":0,"highlights":0,"shadows":0,"temp":0,"tint":0,"saturation":0,"vibrance":0,"clarity":0,"sharpen":0,"preset":null},"beauty":{"smooth":0,"brighten":0,"sharpen":0,"by_mask":false},"lut":null}"#;
        let (a, _) = EditOps::parse(raw).unwrap();
        let (b, _) = EditOps::parse(&a.to_json()).unwrap();
        assert_eq!(a, b, "落库再读出必须一模一样");
    }

    #[test]
    fn 裁满与不裁都算恒等() {
        let mut o = EditOps::default();
        o.geometry.crop = Some([0.0, 0.0, 1.0, 1.0]);
        assert!(o.is_identity());
        o.geometry.crop = Some([0.0, 0.0, 0.99, 1.0]);
        assert!(!o.is_identity());
        o.geometry.crop = Some([0.0, 0.0, 1.0, 1.0]);
        o.geometry.rotate_deg = 90.0;
        assert!(!o.is_identity(), "转了 90° 就不是恒等");
    }
}
