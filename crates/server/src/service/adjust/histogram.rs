//! Cache histogram sidecars with the same revision as adjustment previews.
use super::{pixels, Built, Grade};
use crate::{
    error::{AppError, Result},
    img::codec,
    models::entity::Image,
    service::imagesvc,
    state::Ctx,
    util,
};
use photoedit_core::{
    histogram::{compute, Histogram},
    EditOps,
};
use std::path::Path;

pub(super) fn store(path: &Path, img: &stitch_core::Rgba) -> Result<Histogram> {
    let h = compute(img);
    imagesvc::write_bytes(
        path,
        &serde_json::to_vec(&h).map_err(|e| AppError::fail_detail("srv.adjust.renderGone", e))?,
    )?;
    Ok(h)
}
fn read(path: &Path) -> Option<Histogram> {
    let data = std::fs::read(path).ok()?;
    let h: Histogram = serde_json::from_slice(&data).ok()?;
    h.valid().then_some(h)
}
pub fn preview(ctx: &Ctx, img: &Image, ops: &EditOps, built: &Built) -> Result<Histogram> {
    let path = built.abs.with_extension("hist.json");
    if let Some(h) = read(&path) {
        return Ok(h);
    }
    let out = pixels(ctx, img, ops, Grade::Proxy)?;
    store(&path, &out)
}
pub fn source(ctx: &Ctx, img: &Image) -> Result<Histogram> {
    let rel = img.proxy_path.as_deref().unwrap_or(&img.orig_path);
    let path =
        util::data_file(&ctx.data, rel).ok_or_else(|| AppError::bad("srv.image.origOutside"))?;
    let meta = std::fs::metadata(&path)?;
    let stamp = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let cache = super::slots_dir(ctx, img).join(format!(
        "source-hist-{}-{}-{stamp}.json",
        super::ops_hash(rel),
        meta.len()
    ));
    if let Some(h) = read(&cache) {
        return Ok(h);
    }
    let base = codec::decode(&std::fs::read(&path)?)?;
    let h = store(&cache, &base)?;
    if let Ok(entries) = std::fs::read_dir(super::slots_dir(ctx, img)) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("source-hist-")
                && entry.path() != cache
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(h)
}
