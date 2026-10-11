//! 提交规格存于内部表；照片原件为不可变文件，蒙版和调整参数则必须复制。
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum JobKind { Crop, Full, Sketch }

#[derive(Serialize, Deserialize)]
pub(super) struct JobSpec {
    pub version: u32,
    pub kind: JobKind,
    pub orig_path: String,
    pub cloud: cloud::CloudSettings,
    // CloudSettings 的公共序列化跳过 key；这一格只进入内部规格表。
    pub credential: String,
    pub snapshot: Value,
}

pub(super) fn capture(ctx: &Shared, img: &Image, settings: &Value, cloud: cloud::CloudSettings) -> Result<(JobSpec, Vec<u8>)> {
    let kind = if img.is_sketch() { JobKind::Sketch }
        else if settings.get("full").and_then(Value::as_bool).unwrap_or(false) { JobKind::Full }
        else { JobKind::Crop };
    let params = params_for(&cloud, settings);
    let ops: photoedit_core::EditOps = match crate::repo::adjust::ops_of(ctx, img.id)? {
        Some(s) => serde_json::from_str(&s).map_err(|_| AppError::bad("srv.queue.specMissing"))?,
        None => photoedit_core::EditOps::default(),
    };
    let mask = if matches!(kind, JobKind::Crop) || ops.beauty.by_mask {
        match img.mask_path.as_deref().and_then(|m| util::data_file(&ctx.data, m)) {
            Some(p) => std::fs::read(p).map_err(|e| AppError::fail_detail("srv.image.maskRead", e))?,
            None if params.invert || !matches!(kind, JobKind::Crop) => Vec::new(),
            None => return Err(AppError::bad("srv.queue.noInk")),
        }
    } else { Vec::new() };
    let mut snapshot = stitch_snapshot(&params, &ops);
    snapshot["adjust_inputs"] = crate::service::adjust::snapshot_inputs(ctx, img, &ops)?;
    Ok((JobSpec { version: 1, kind, orig_path: img.orig_path.clone(), credential: cloud.key.clone(), cloud, snapshot }, mask))
}

pub(super) fn load(ctx: &Shared, id: i64) -> Result<(JobSpec, Vec<u8>)> {
    let (json, mask) = rres::job_spec(ctx, id)?.ok_or_else(|| AppError::bad("srv.queue.specMissing"))?;
    let mut spec: JobSpec = serde_json::from_str(&json).map_err(|_| AppError::bad("srv.queue.specMissing"))?;
    if spec.version != 1 { return Err(AppError::bad("srv.queue.specMissing")); }
    spec.cloud.key = spec.credential.clone();
    Ok((spec, mask))
}

fn params_for(s: &cloud::CloudSettings, settings: &Value) -> StitchParams {
    let edge = util::number_of(settings.get("edge")).filter(|n| *n >= 1.0).map(|n| n.clamp(512.0, 3840.0) as i64);
    StitchParams { expand: s.stitch_expand as f64, context: stitch_core::DEFAULT_CONTEXT,
        crop_edge: edge.unwrap_or(s.stitch_edge.max(1)) as u32, feather: s.stitch_feather as f64,
        levels: stitch_core::DEFAULT_LEVELS, invert: settings.get("invert").and_then(Value::as_bool).unwrap_or(false) }
}

pub(super) fn prepare_job(ctx: &Shared, img: &Image, spec: &JobSpec, mask: Vec<u8>) -> Result<Prepared> {
    let mut pre = prepared_from_snapshot(ctx, img, &spec.snapshot, if mask.is_empty() { None } else { Some(&mask) })?;
    pre.mask_png = mask;
    pre.snap = spec.snapshot.clone();
    Ok(pre)
}
