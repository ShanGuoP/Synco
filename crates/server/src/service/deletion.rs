//! 删除后清理磁盘；失败的清单留在库中，由推进器和重启重试。
use crate::{error::Result, repo, state::Shared, util};

pub async fn delete_project(ctx: &Shared, id: i64) -> Result<()> {
    repo::deletion::delete_project(ctx, id)?;
    let _ = retry(ctx).await;
    Ok(())
}

pub async fn delete_image(ctx: &Shared, id: i64) -> Result<()> {
    repo::deletion::delete_image(ctx, id)?;
    let _ = retry(ctx).await;
    Ok(())
}

pub async fn retry(ctx: &Shared) -> Result<()> {
    for row in repo::deletion::pending(ctx)? {
        let plan: serde_json::Value = serde_json::from_str(row["value"].as_str().unwrap_or("")).map_err(|e| crate::error::AppError::Fail(e.to_string()))?;
        let data = ctx.data.clone();
        let registered: std::collections::HashSet<String> = repo::results::registered_paths(ctx)?.into_iter().collect();
        let cleaned = util::blocking(move || {
            let mut clean = true;
            for row in plan["images"].as_array().into_iter().flatten() {
                let img = crate::models::entity::Image::from_value(row);
                let Some(original) = util::data_file(&data, &img.orig_path) else { continue };
                let Some(parent) = original.parent() else { continue };
                let stem = util::stem_of(&img.orig_path).to_lowercase();
                let input = crate::service::adjust::input_prefix(&img);
                match std::fs::read_dir(parent) {
                    Ok(entries) => for entry in entries {
                        let entry = match entry { Ok(e) => e, Err(_) => { clean = false; continue; } };
                        let name = entry.file_name().to_string_lossy().to_lowercase();
                        if !(name.starts_with(&input) || name.starts_with(&format!("{stem}_thumb.")) || name.starts_with(&format!("{stem}_proxy"))) { continue; }
                        let rel = entry.path().strip_prefix(&data).ok().map(|p| p.to_string_lossy().replace('\\', "/"));
                        if rel.as_ref().is_some_and(|p| registered.contains(p)) { continue; }
                        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) && std::fs::remove_file(entry.path()).is_err() { clean = false; }
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                    Err(_) => clean = false,
                }
            }
            for (k, directory) in [("files", false), ("dirs", true)] {
                for rel in plan[k].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
                    if !directory && registered.contains(rel) { continue; }
                    let Some(p) = util::data_file(&data, rel) else { continue };
                    let result = if directory { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
                    if let Err(e) = result { if e.kind() != std::io::ErrorKind::NotFound { clean = false; } }
                }
            }
            Ok(clean)
        }).await?;
        if cleaned { repo::settings::del(ctx, row["key"].as_str().unwrap())?; }
    }
    Ok(())
}
