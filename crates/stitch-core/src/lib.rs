//! 缝合内核的公共出口。取值条件（阈值、夹逼区间、闭开区间）由各模块的回归测试钉住，
//! 改任何一段先看对应 tests 里的断言。
//!
//! 缓冲、行带并行与重采样住在 `px-core`（`photoedit-core` 也用那一层）；这里只 re-export，
//! 让 `stitch_core::Rgba` 这类老写法继续有效——两边必须是同一个类型，跨层的图与蒙版才传得动。

pub use px_core::{buffer, par, resize};

pub mod color;
pub mod geom;
pub mod mask;
pub mod pyramid;
pub mod stitch;

pub use buffer::{Alpha, Box2, Rgba};
pub use color::color_match;
pub use geom::{fit_size, fit_size_from_setting, long_edge, Fit};
pub use mask::{dilate, ink_bbox, paste_alpha};
pub use pyramid::{pyramid_blend, pyramid_blend_boxed};
pub use resize::{crop_scale_alpha, crop_scale_rgba, half_alpha, half_rgba, resize_alpha, resize_rgba};
pub use stitch::{build_crop_payload, paste_weights, stitch_crop, stitch_crop_boxed, Build, CropPayload, StitchParams, DEFAULT_CONTEXT, DEFAULT_CROP_EDGE, DEFAULT_EXPAND, DEFAULT_FEATHER, DEFAULT_LEVELS};
