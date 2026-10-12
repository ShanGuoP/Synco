//! 变形段：手动液化笔画 + 关键点驱动的一键塑形（MLS）。
//!
//! 两种变形都走**反向映射**：目标像素去源图取色，而不是把源像素推出去。
//! 前者每个目标像素必定有值、不会出现洞；后者要靠散布加权，还要处理重叠。
//! 代价是"推"的方向得反着算——位移场 `d(p)` 表示 p 该被搬去哪，取样就取 `p − d(p)`。

use crate::ops::{Warp};
use px_core::{Rgba};

/// 滑杆满档时，一个笔画步长最多搬掉半径的多少比例。
/// 超过这个量就会把采样点推出盘外，等于自己咬自己的输出。
pub const STROKE_MAX_SHIFT: f32 = 0.45;
/// MLS 稀疏网格步长（像素）。网格越密越准也越慢，8px 是样张上看不出台阶的那一档
pub const MLS_GRID: usize = 8;
/// 一键塑形的局部域 = 人脸框外扩倍数，域外恒等
pub const MLS_SCOPE: f32 = 1.2;

/// 变形段总入口：先笔画后 MLS（笔画是用户手工意图，放在前面不被自动结果覆盖）
pub fn apply(img: &Rgba, warp: &Warp, shape: Option<&FaceShape>) -> Rgba {
    let mut cur = strokes(img, warp);
    if !warp.auto.is_empty() {
        if let Some(s) = shape {
            cur = mls(&cur, &warp.auto, s);
        }
    }
    cur
}


mod stroke;
mod face;
pub use stroke::{simplify, strokes};
pub use face::{control_pairs, mls, FaceShape};
pub(crate) use face::mls_pairs;
#[cfg(test)]
use stroke::field_at;
#[cfg(test)]
mod tests;
