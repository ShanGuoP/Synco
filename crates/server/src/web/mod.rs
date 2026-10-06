//! HTTP 传输层：静态资源与取图、回环守卫、内嵌资源兜底。
//! 与 `api/` 同族——`api/` 管业务端点，这里管"字节怎么进出"。

pub mod assets;
pub mod files;
pub mod guard;
