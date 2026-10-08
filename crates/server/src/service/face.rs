//! 人脸感知这一侧目前只做一件事：读关键点缓存。
//!
//! 0.3.0 里没有生产者——官方那份 YuNet 导出跑起来是"锚点头"（一个 1×3×640×640 输入、
//! 九个输出且**没有关键点分支**），要配一套锚点先验才能用；稠密 468 点模型也没拿到
//! 可以核可再分发来源的权重。所以自动变形与构图建议留到 0.3.x 接生产者，
//! 这一层先按"表里有就用、没有就不动"实现：一键塑形整段跳过，手动液化/调色/美颜不受影响。
//!
//! 推理本身将来放这里（tract 的会话是有状态的），"张量变成框与点"是纯函数，在
//! `photoedit_core::face`，两边各测各的。

use crate::models::entity::Image;
use crate::repo::adjust as radj;
use crate::state::Ctx;
use photoedit_core::face;
use photoedit_core::FaceShape;
use serde_json::Value;

/// 读出这张图已经落表的稠密关键点，折成一键塑形要的那组控制点。
/// 表是空的（还没检出过）或点数不足以定边界就返回 `None`——
/// 自动变形整段跳过，手动液化与调色都不依赖它。
pub fn cached_shape(ctx: &Ctx, img: &Image) -> Option<FaceShape> {
    let rows = radj::landmarks(ctx, img.id).ok()?;
    if rows.is_empty() {
        return None;
    }
    let groups: Vec<Vec<[f32; 2]>> = rows
        .iter()
        .filter_map(|r| r.get("points").and_then(Value::as_str).map(face::parse_points))
        .collect();
    let all = face::points_from_rows(&groups)?;
    // 主脸（idx 最小的那一行）的粗定位五点：认左眼右眼与鼻尖要靠它
    let hint: [[f32; 2]; 5] = radj::faces(ctx, img.id)
        .ok()?
        .first()
        .and_then(|f| f.get("landmarks").and_then(Value::as_str))
        .map(face::parse_points)?
        .try_into()
        .ok()?;
    face::shape_from_landmarks(&all, &hint)
}

/// 这张图有没有可用的关键点（面板据此决定一键塑形那排滑杆是灰的还是能拖）
pub fn has_landmarks(ctx: &Ctx, img: &Image) -> bool {
    cached_shape(ctx, img).is_some()
}
