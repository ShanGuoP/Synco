//! 本地精修内核。全部是"给一张图和一组参数，还一张图"的纯函数：
//! 没有 IO、没有 tokio、不认 HTTP，`tools/lint-layers.js` 的 R4 就是钉这件事的。
//!
//! 链的固定顺序是 **几何 → 变形 → 调色 → 美颜**：变形产生的插值像素先落定，
//! 调色吃的是最终几何，美颜蒙版的坐标则约定在"变形之后"的那个域里。

pub mod beauty;
pub mod color;
pub mod face;
pub mod geometry;
pub mod lut;
pub mod ops;
pub mod pipeline;
pub mod warp;
pub(crate) mod px;

pub use beauty::{apply as apply_beauty, bilateral};
pub use color::{apply as apply_color, preset, PRESETS};
pub use face::{beauty_region, crop_suggestion, nms, FaceBox};
pub use geometry::apply as apply_geometry;
pub use lut::{apply as apply_lut, parse_cube, Lut as LutTable};
pub use ops::{Auto, Beauty, Color, EditOps, Fill, Geometry, Lut as LutRef, Slider, Stroke, Tool, Warp, DEFAULT_JSON, SLIDER_MAX, SLIDER_MIN};
pub use pipeline::{apply_chain, Chain};
pub use warp::{control_pairs, simplify, FaceShape};
