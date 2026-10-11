//! 业务服务层：ComfyUI 客户端、云端通道、后端注册表、目录体检、结果推进与批量队列。
//! 这里只放"能脱离 HTTP 单测"的东西，axum handler 全在 `crate::api`。

pub mod backend;
pub mod adjust;
pub mod cloud;
pub mod canvas;
pub mod comfy;
pub mod face;
pub mod imagesvc;
pub mod queue;
pub mod refs;
pub mod reclaim;
pub mod releases;
pub mod setup;
pub mod workflow;
