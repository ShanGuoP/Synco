//! 文件交接与导出：真实内容校验、原子落盘，以及结果登记失败的清理。
use crate::{
    error::{AppError, Result},
    img::codec,
    models::interop::{uuid, Create},
    repo::{images, interop as db},
    service::{adjust, imagesvc},
    state::Ctx,
    util,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
fn hash(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("part");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(tmp, path)?;
    Ok(())
}
pub(super) fn import(ctx: &Ctx, row: &Value) -> Result<()> {
    let id = row["id"].as_i64().unwrap();
    let pid = row["project_id"].as_i64().unwrap();
    let spec: Create = serde_json::from_str(row["request_json"].as_str().unwrap())
        .map_err(|e| AppError::Fail(e.to_string()))?;
    let path = Path::new(&spec.input.path);
    let meta = fs::metadata(path).map_err(|_| AppError::coded(404, "srv.interop.input_missing"))?;
    if !meta.is_file() {
        return Err(AppError::bad("srv.interop.bad_request"));
    }
    if meta.len() > super::MAX_FILE_BYTES {
        return Err(AppError::coded(422, "srv.interop.resource_limit"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(super::MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > super::MAX_FILE_BYTES {
        return Err(AppError::coded(422, "srv.interop.resource_limit"));
    }
    if hash(&bytes) != spec.input.sha256 {
        return Err(AppError::coded(409, "srv.interop.hash_mismatch"));
    }
    let format = image::guess_format(&bytes)
        .map_err(|_| AppError::coded(415, "srv.interop.unsupported_format"))?;
    let ext = match format {
        image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::Png => "png",
        image::ImageFormat::WebP => "webp",
        image::ImageFormat::Bmp => "bmp",
        _ => return Err(AppError::coded(415, "srv.interop.unsupported_format")),
    };
    let (w, h) = codec::dimensions(&bytes).map_err(|e| {
        if e.keyed().is_some_and(|(k, _)| k == "srv.image.dimsOut") {
            AppError::coded(422, "srv.interop.resource_limit")
        } else {
            AppError::coded(415, "srv.interop.unsupported_format")
        }
    })?;
    drop(
        codec::decode(&bytes)
            .map_err(|_| AppError::coded(415, "srv.interop.unsupported_format"))?,
    );
    let rel = format!("projects/{pid}/interop-{id}.{ext}");
    let dest = ctx.data.join(&rel);
    write(&dest, &bytes)?;
    let name = util::safe_name(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    );
    match db::attach(ctx, id, &name, &rel, w as i64, h as i64) {
        Ok(Some(image)) => {
            if let Some(img) = images::by_id(ctx, image)? {
                imagesvc::derive(ctx, &img)?;
            }
            Ok(())
        }
        Ok(None) => {
            let _ = fs::remove_file(dest);
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(dest);
            Err(e)
        }
    }
}
pub(super) fn export(
    ctx: &Ctx,
    id: i64,
    image: i64,
    raw: &str,
    result: Option<i64>,
    prepared: tokio::sync::oneshot::Sender<()>,
) -> Result<()> {
    let row = db::by_id(ctx, id)?;
    let spec: Create = serde_json::from_str(row["request_json"].as_str().unwrap())
        .map_err(|e| AppError::Fail(e.to_string()))?;
    let img = images::by_id(ctx, image)?
        .ok_or_else(|| AppError::coded(404, "srv.interop.input_missing"))?;
    let (ops, _) = photoedit_core::EditOps::parse(raw).map_err(AppError::bad_msg)?;
    let inputs = if result.is_none() {
        adjust::snapshot::snapshot_inputs(ctx, &img, &ops)?
    } else {
        Value::Null
    };
    let mask = if result.is_none() {
        img.mask_path
            .as_deref()
            .map(|p| {
                util::data_file(&ctx.data, p)
                    .ok_or_else(|| AppError::bad("srv.image.origOutside"))
                    .and_then(|p| fs::read(p).map_err(AppError::from))
            })
            .transpose()?
    } else {
        None
    };
    let selected = if let Some(rid) = result {
        let r = crate::repo::results::by_id(ctx, rid)?
            .ok_or_else(|| AppError::coded(404, "srv.interop.input_missing"))?;
        if r.image_id != image || r.status != "done" {
            return Err(AppError::bad("srv.interop.bad_request"));
        }
        let path = r
            .final_path
            .as_deref()
            .and_then(|p| util::data_file(&ctx.data, p))
            .ok_or_else(|| AppError::coded(404, "srv.interop.input_missing"))?;
        Some(fs::read(path)?)
    } else {
        None
    };
    let _ = prepared.send(());
    let out = if let Some(bytes) = selected {
        codec::decode(&bytes)?
    } else {
        adjust::snapshot::photo_from_snapshot(ctx, &img, &ops, &inputs, mask.as_deref())?
    };
    let bytes = if spec.output.format == "png" {
        codec::encode_png(&out)
    } else {
        codec::encode_jpeg(&out, spec.output.quality)
    };
    let result_id = uuid();
    let ext = if spec.output.format == "png" {
        "png"
    } else {
        "jpg"
    };
    let path = ctx
        .data
        .join("interop/returns")
        .join(format!("{result_id}.{ext}"));
    write(&path, &bytes)?;
    let result = json!({"result_id":result_id,"source_result_id":result,"relative_path":format!("interop/returns/{result_id}.{ext}"),"output":{"path":fs::canonicalize(&path)?.display().to_string(),"sha256":hash(&bytes),"bytes":bytes.len(),"width":out.w,"height":out.h,"format":spec.output.format}});
    match db::ready(ctx, id, &result) {
        Ok(true) => Ok(()),
        Ok(false) => {
            let _ = fs::remove_file(path);
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(path);
            Err(e)
        }
    }
}
pub(super) fn recover(ctx: &Ctx) -> Result<()> {
    // 未登记的返回文件不可领取，重启清理；已登记成品永远不按普通缓存清理。
    let rows = crate::repo::interop::all_results(ctx)?;
    let names: std::collections::HashSet<String> = rows
        .iter()
        .filter_map(|v| v["result_json"].as_str())
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .filter_map(|v| {
            v["relative_path"]
                .as_str()
                .and_then(|s| Path::new(s).file_name())
                .map(|s| s.to_string_lossy().to_string())
        })
        .collect();
    let dir = ctx.data.join("interop/returns");
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && !names.contains(entry.file_name().to_string_lossy().as_ref())
            {
                fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}
