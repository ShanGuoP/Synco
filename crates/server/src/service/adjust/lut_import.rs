//! Import validated cube files under content-addressed names; edits never overwrite a prior LUT.
use crate::{
    error::{AppError, Result},
    service::imagesvc,
    state::Ctx,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
pub fn import(ctx: &Ctx, name: &str, text: &str) -> Result<Value> {
    if text.len() > 4 * 1024 * 1024
        || !name.to_ascii_lowercase().ends_with(".cube")
        || name.contains(['/', '\\'])
        || name.contains("..")
    {
        return Err(AppError::bad_args(
            "srv.adjust.lutName",
            json!({"name":name}),
        ));
    }
    photoedit_core::parse_cube(text).map_err(|e| {
        AppError::bad_args(
            "srv.adjust.lutParse",
            json!({"msg":{"code":e.code,"args":e.args}}),
        )
    })?;
    let stem = name
        .strip_suffix(".cube")
        .or_else(|| name.strip_suffix(".CUBE"))
        .unwrap_or("lut");
    let stem: String = stem
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
        .take(60)
        .collect();
    let file = format!(
        "{}_{}.cube",
        if stem.is_empty() { "lut" } else { &stem },
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let path = ctx.data.join("luts").join(&file);
    if !path.exists() {
        imagesvc::write_bytes(&path, text.as_bytes())?;
    }
    Ok(json!({"name":file}))
}
