# 第三方组件与许可（NOTICE）

Synco 0.3 的发行二进制里包含下列第三方组件的**编译产物**。这里列许可与来源，
许可证全文随各组件上游仓库发布；本项目自身是 Apache-2.0（字体另计，见文末）。
每项的许可字段取自本机 `~/.cargo/registry` 里对应版本的 `Cargo.toml`，不是照抄记忆。

## 直接依赖（Rust）

| 组件 | 版本 | 许可 | 用在哪 |
|---|---|---|---|
| [axum](https://github.com/tokio-rs/axum) | 0.8.9 | MIT | 本机 HTTP 服务与路由 |
| [tokio](https://github.com/tokio-rs/tokio) | 1.53.2 | MIT | 异步运行时、阻塞线程池 |
| [serde](https://github.com/dtolnay/serde) / [serde_json](https://github.com/dtolnay/serde_json) | 1.0.229 / 1.0.151 | MIT OR Apache-2.0 | 参数链与接口 JSON |
| [rusqlite](https://github.com/rusqlite/rusqlite)（内含 SQLite，public domain） | 0.40.2 | MIT | 库与迁移 |
| [image](https://github.com/image-rs/image) | 0.25.10 | MIT OR Apache-2.0 | 解码与 EXIF 方向烘焙 |
| [imageproc](https://github.com/image-rs/imageproc) | 0.27.0 | MIT | 形态学膨胀（缝合前的掩膜准备） |
| [fast_image_resize](https://github.com/Cykooz/fast_image_resize) | 6.1.0 | MIT OR Apache-2.0 | 派生档与调整预览的重采样 |
| [rust-embed](https://github.com/pyros2097/rust-embed) | 8.12.0 | MIT | 把 `public/` 打进 exe |
| [reqwest](https://github.com/seanmonstar/reqwest)（含 rustls / ring） | 0.12.28 | MIT OR Apache-2.0 | 本机与云端后端调用 |
| [rand](https://github.com/rust-random/rand) | 0.10.3 | MIT OR Apache-2.0 | 随机种子 |
| [base64](https://github.com/marshallpierce/rust-base64)（含 0.22 与 0.23 两份，上游各自许可） | 0.23.1 | MIT OR Apache-2.0 | 图片导入导出 |
| [sha2](https://github.com/RustCrypto/hashes) | 0.11.0 | MIT OR Apache-2.0 | 资源指纹 |
| [thiserror](https://github.com/dtolnay/thiserror) | 2.0.21 | MIT OR Apache-2.0 | 错误类型 |
| [url](https://github.com/servo/rust-url) / [pathdiff](https://github.com/manisheus/pathdiff) | 2.5.8 / 0.2.3 | MIT OR Apache-2.0 / MIT OR Apache-2.0 | 地址与相对路径 |
| [futures-util](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 | 流式下载 |
| **[moving-least-squares](https://github.com/mpizenberg/rust_mls)** | 0.1.0 | **MPL-2.0** | 一键塑形的形变数学（`crates/photoedit-core` 的 `mls_at`） |
| [zune-jpeg](https://github.com/etemesi254/zune-image)（`image` 的 JPEG 解码后端） | 0.5.15 | MIT OR Apache-2.0 OR Zlib | JPEG 解码 |

**关于 MPL-2.0**：`moving-least-squares` 是**文件级**弱著佐权——它以原样 crate 依赖，
没有被改动、没有被复制源码进本仓库，因此只要求那两个文件本身继续留在 MPL-2.0 下，
不影响本项目整体按 Apache-2.0 发布。源码与许可证见上面的仓库地址（同时镜像在 crates.io）。

## 桌面壳

[Tauri 2](https://tauri.app)（`tauri` 2.12.1 及其 build/codegen/macros/plugin 系列）— Apache-2.0 OR MIT。
含单实例插件与窗口/对话框插件。

## 字体

`public/fonts/NotoSerifSC-VF.woff2` 是 Google 的 **Noto Serif SC（Source Han Serif 派生）**，
单独适用 **SIL Open Font License 1.1**，不受本项目 Apache-2.0 覆盖。
授权全文见 [`public/fonts/OFL.txt`](public/fonts/OFL.txt)。

---

## 评估过但**没有**打进 0.3 的东西

写在这里是防止后来人按方案文档以为它们已经在二进制里：

- **photon-rs**：方案里作为调色算子的参考实现。实际调色算子（曝光/对比/高光阴影/色温色调/
  饱和度/自然饱和度/清晰度/锐化）与磨皮、液化、MLS 包装都是本仓库自行实现并各自钉测的，
  **没有翻译或复制任何 photon 源码**，所以不引依赖也不据其主张任何署名义务。
- **opencv_zoo 的 YuNet 人脸检测模型 + tract-onnx**：实测该 ONNX 导出是"锚点头"
  （单输入 1×3×640×640、九个输出且**不含关键点分支**），要另配一套锚点先验解码才能用；
  因此 0.3.0 未内嵌模型、未引入 tract。人脸相关的那一格（构图建议、自动美颜区、
  一键塑形）随之推到 0.3.x，`crates/photoedit-core` 里的 MLS 与关键点几何已备好并被单测钉住。
- **MediaPipe FaceMesh（468 点）模型**：没找到可核查许可与内容的现成 ONNX 权重，
  不经核可不内嵌分发。
- **onnxruntime / opencv 等带 C++ 运行时的依赖**：与"单个 exe 离线可用"冲突，一开始就排除。
