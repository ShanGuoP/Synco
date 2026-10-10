//! 像素层：缓冲、行带并行、重采样，以及那个与浏览器 `Uint8ClampedArray` 同口径的取整。
//!
//! 从 `stitch-core` 分出来是因为 `photoedit-core` 只用得上这五个符号
//! （`Rgba` / `Alpha` / `par::par_chunks_mut` / `resize_alpha` / `resize_rgba`），
//! 却要连着把 imageproc（形态学膨胀）一起拖进自己的依赖树——依赖图因此不说实话。
//! 缝合语义（膨胀、金字塔融合、裁切贴回）留在 `stitch-core`，那才是它的域。

pub mod buffer;
pub mod par;
pub mod resize;

pub use buffer::{Alpha, Box2, Rgba};
pub use resize::{
    crop_scale_alpha, crop_scale_rgba, grow_alpha, grow_rgba, half_alpha, half_rgba,
    resize_alpha, resize_rgba, to_u8,
};
