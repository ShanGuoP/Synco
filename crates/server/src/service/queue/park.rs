//! 裁切任务跨网络等待只持编码载荷和小快照；全幅像素逐字节暂存，重启清理孤儿。
use crate::{error::{AppError, Result}, state::Shared, util};
use std::{fs, path::PathBuf};
use super::crop::Prepared;

pub(super) struct Parked {
    pre: Prepared,
    dir: PathBuf,
    mask_len: usize,
}
impl Drop for Parked {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.dir); }
}

impl Parked {
    pub(super) fn new(ctx: &Shared, id: i64, pre: Prepared) -> Result<Self> {
        let root = ctx.data.join("runtime/jobs");
        validate_root(ctx, &root)?;
        fs::create_dir_all(&root)?;
        let dir = root.join(format!("crop-{id}-{}-{}", util::now_ms(), util::r4()));
        fs::create_dir(&dir)?;
        let mask_len = pre.mask_png.len();
        let mut parked = Self { pre, dir, mask_len };
        fs::write(parked.dir.join("base.rgba"), &parked.pre.base.px)?;
        fs::write(parked.dir.join("alpha.u8"), &parked.pre.alpha.v)?;
        fs::write(parked.dir.join("mask.png"), &parked.pre.mask_png)?;
        drop(std::mem::take(&mut parked.pre.mask_png));
        // take 后原容量一起释放，clear 会把百 MB 的容量继续留在网络等待中。
        drop(std::mem::take(&mut parked.pre.base.px));
        drop(std::mem::take(&mut parked.pre.alpha.v));
        Ok(parked)
    }
    pub(super) fn payload(&mut self) -> (Vec<u8>, Option<Vec<u8>>, String) {
        let crop = std::mem::take(&mut self.pre.crop);
        let mask = std::mem::take(&mut self.pre.mask);
        let mask = if self.pre.no_mask { None } else { Some(mask) };
        (crop, mask, self.pre.size.clone())
    }
    pub(super) fn reload(&mut self) -> Result<&Prepared> {
        let base_len = self.pre.base.w.checked_mul(self.pre.base.h).and_then(|n| n.checked_mul(4)).ok_or_else(|| AppError::fail("srv.queue.specMissing"))?;
        let alpha_len = self.pre.alpha.w.checked_mul(self.pre.alpha.h).ok_or_else(|| AppError::fail("srv.queue.specMissing"))?;
        self.pre.base.px = read_exact(&self.dir.join("base.rgba"), base_len)?;
        self.pre.alpha.v = read_exact(&self.dir.join("alpha.u8"), alpha_len)?;
        self.pre.mask_png = read_exact(&self.dir.join("mask.png"), self.mask_len)?;
        Ok(&self.pre)
    }
}

fn read_exact(path: &std::path::Path, length: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    if fs::metadata(path)?.len() != length as u64 { return Err(AppError::fail("srv.queue.specMissing")); }
    let mut bytes = vec![0; length];
    let mut file = fs::File::open(path)?;
    file.read_exact(&mut bytes)?;
    let mut extra = [0; 1];
    if file.read(&mut extra)? != 0 { return Err(AppError::fail("srv.queue.specMissing")); }
    Ok(bytes)
}

pub(super) fn recover(ctx: &Shared) -> Result<usize> {
    let root = ctx.data.join("runtime/jobs");
    validate_root(ctx, &root)?;
    let entries = match fs::read_dir(&root) { Ok(e) => e, Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0), Err(e) => return Err(e.into()) };
    let mut count = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let parts: Vec<_> = name.split('-').collect();
        if parts.len() != 4 || parts[0] != "crop" || parts[1].parse::<u64>().ok().filter(|id| *id > 0).is_none() || parts[2].len() < 13 || !parts[2].bytes().all(|b| b.is_ascii_digit()) || parts[3].len() != 4 || !parts[3].bytes().all(|b| b.is_ascii_hexdigit()) { continue; }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_dir() || crate::service::library::directory_link(&metadata) { continue; }
        fs::remove_dir_all(entry.path())?;
        count += 1;
    }
    Ok(count)
}

fn validate_root(ctx: &Shared, root: &std::path::Path) -> Result<()> {
    for path in [ctx.data.join("runtime"), root.to_path_buf()] {
        if fs::symlink_metadata(&path).map(|m| crate::service::library::directory_link(&m)).unwrap_or(false) {
            return Err(AppError::bad_args("srv.shell.copyLink", serde_json::json!({"path":path.display().to_string()})));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parked_pixels_are_released_restored_exactly_and_cleaned_on_failure_or_restart() {
        let root = std::env::temp_dir().join(format!("synco-park-{}-{}", util::now_ms(), util::r4()));
        fs::create_dir_all(&root).unwrap();
        let ctx = crate::state::Ctx::new(root.clone(), root.clone(), crate::repo::db::open(&root).unwrap());
        let mut photo = stitch_core::Rgba::new(32,32);
        for (i, byte) in photo.px.iter_mut().enumerate() { *byte = (i % 251) as u8; }
        let mut alpha = stitch_core::Alpha::new(32,32); alpha.v.fill(255);
        let pixels = photo.px.clone(); let mask = alpha.v.clone();
        let pre = super::super::crop::build_prepared(photo, alpha, &stitch_core::StitchParams::default()).unwrap();
        let mut parked = Parked::new(&ctx, 1, pre).unwrap();
        let dir = parked.dir.clone();
        assert!(parked.pre.base.px.is_empty() && parked.pre.alpha.v.is_empty());
        assert_eq!(parked.pre.base.px.capacity(), 0);
        assert_eq!(parked.pre.alpha.v.capacity(), 0);
        assert_eq!(parked.reload().unwrap().base.px, pixels);
        assert_eq!(parked.pre.alpha.v, mask);
        fs::write(dir.join("base.rgba"), [0]).unwrap();
        assert!(parked.reload().is_err());
        drop(parked); assert!(!dir.exists());
        let orphan = root.join("runtime/jobs/crop-9-1791760000000-abcd");
        fs::create_dir_all(&orphan).unwrap(); fs::write(orphan.join("base.rgba"), [1]).unwrap();
        fs::create_dir(root.join("runtime/jobs/keep-me")).unwrap();
        assert_eq!(recover(&ctx).unwrap(), 1);
        assert!(!orphan.exists()); assert!(root.join("runtime/jobs/keep-me").exists());
        drop(ctx); fs::remove_dir_all(root).unwrap();
    }
}
