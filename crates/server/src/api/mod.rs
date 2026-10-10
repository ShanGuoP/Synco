//! 路由表：一张表看全 39 条端点，实现在同目录按域分开的文件里。

pub mod backends;
pub mod adjust;
pub mod canvas;
pub mod cloud;
pub mod common;
pub mod images;
pub mod presets;
pub mod projects;
pub mod results;
pub mod settings;
pub mod setup;

use crate::web::files;
use crate::state::Shared;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

pub use common::BODY_LIMIT;

async fn version_get(State(ctx): State<Shared>) -> Response {
    // 桌面壳与「关于」分区都要看：这次跑的库在哪、界面是从盘上还是从 exe 里取的
    let v = serde_json::to_value(crate::version::info().clone()).unwrap_or_else(|_| serde_json::json!({}));
    let mut o = v.as_object().cloned().unwrap_or_default();
    let show = |p: &std::path::Path| serde_json::Value::String(crate::util::neat_path(p));
    o.insert("data_dir".into(), show(&ctx.data));
    o.insert("public_dir".into(), show(&ctx.public));
    o.insert("public_from_disk".into(), serde_json::Value::Bool(ctx.public.is_dir()));
    o.insert("desktop".into(), serde_json::Value::Bool(crate::DESKTOP.load(std::sync::atomic::Ordering::Relaxed)));
    axum::Json(serde_json::Value::Object(o)).into_response()
}

/// 更新日志：从 GitHub Releases 拉，打包态也看得到
async fn releases_get() -> Response {
    axum::Json(crate::service::releases::list().await).into_response()
}

pub fn router() -> Router<Shared> {
    Router::new()
        .route("/api/projects", get(projects::projects_list).post(projects::projects_create))
        .route("/api/projects/{id}", get(projects::project_get).delete(projects::project_delete))
        .route("/api/projects/{id}/rename", post(projects::project_rename))
        .route("/api/projects/{id}/images", post(projects::images_add))
        .route("/api/projects/{id}/settings", get(projects::project_settings_get).post(projects::project_settings_set))
        .route("/api/images/{id}", get(images::image_get).delete(images::image_delete))
        .route("/api/images/{id}/mask", post(images::mask_post))
        .route("/api/images/{id}/adjust", get(adjust::adjust_get).post(adjust::adjust_post))
        .route("/api/images/{id}/adjust/preview", post(adjust::preview_post))
        .route("/api/images/{id}/adjust/tiles", get(adjust::tiles_get))
        .route("/api/images/{id}/adjust/render", post(adjust::render_post))
        .route("/api/images/{id}/adjust/fork", post(adjust::fork_post))
        .route("/api/images/{id}/derived", get(images::derived_get))
        .route("/api/images/{id}/thumb", get(images::thumb_get))
        .route("/api/images/{id}/tiles", get(images::tiles_get))
        .route("/api/run", post(results::run_post))
        .route("/api/canvas/create", post(canvas::canvas_create))
        .route("/api/canvas/{id}", get(canvas::canvas_get))
        .route("/api/canvas/{id}/sketch", post(canvas::sketch_post))
        .route("/api/canvas/{id}/refs", post(canvas::canvas_refs_add).put(canvas::canvas_refs_set))
        .route("/api/canvas/{id}/use-sketch", post(canvas::canvas_use_sketch))
        .route("/api/canvas/{id}/generate", post(canvas::canvas_generate))
        .route("/api/results", get(results::results_list))
        .route("/api/results/{id}", get(results::result_get).delete(results::result_delete))
        .route("/api/results/{id}/interrupt", post(results::interrupt_post))
        .route("/api/results/{id}/fork", post(results::fork_post))
        .route("/api/results/{id}/lossless", get(results::lossless_get))
        .route("/api/setup", get(setup::setup_get))
        .route("/api/setup/progress", get(setup::setup_progress))
        .route("/api/setup/root", post(setup::setup_root))
        .route("/api/setup/script", post(setup::setup_script))
        .route("/api/setup/verify", post(setup::setup_verify))
        .route("/api/backends", get(backends::backends_get).post(backends::backends_add))
        .route("/api/backends/scan", post(backends::backends_scan))
        .route("/api/backends/remove", post(backends::backends_remove))
        .route("/api/backends/select", post(backends::backends_select))
        .route("/api/cloud", get(cloud::cloud_get).post(cloud::cloud_set))
        .route("/api/cloud/test", post(cloud::cloud_test))
        .route("/api/cloud/edit", post(cloud::cloud_edit))
        .route("/api/cloud/queue", get(cloud::queue_get).post(cloud::queue_post))
        .route("/api/presets", get(presets::presets_list).post(presets::presets_add))
        .route("/api/presets/{id}/delete", post(presets::preset_delete))
        .route("/api/presets/{id}/update", post(presets::preset_update))
        .route("/api/settings", get(settings::api_settings))
        .route("/api/settings/workflow", post(settings::workflow_set))
        .route("/api/workflow/inspect", get(settings::workflow_inspect))
        .route("/api/workflow/roles", get(settings::workflow_roles_get).post(settings::workflow_roles_post))
        .route("/api/settings/proxy-edge", post(settings::proxy_edge_set))
        .route("/api/settings/lang", post(settings::lang_set))
        .route("/api/export", get(settings::export_get))
        .route("/api/export/dir", post(settings::export_dir_set))
        .route("/api/export/run", post(settings::export_run))
        .route("/api/cfg", get(settings::cfg_get))
        .route("/api/version", get(version_get))
        .route("/api/releases", get(releases_get))
        .fallback(files::fallback)
}
