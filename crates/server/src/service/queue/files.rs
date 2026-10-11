//! SQLite 提交前由本次调用拥有产物；错误、取消与 unwind 都回收清单。
use std::path::PathBuf;
use crate::{error::Result, repo::results, state::Shared};

pub(crate) struct PendingFiles {
    data: PathBuf,
    paths: Vec<String>,
    committed: bool,
}
impl PendingFiles {
    pub fn new(data: PathBuf) -> Self { Self { data, paths: Vec::new(), committed: false } }
    pub fn track(&mut self, rel: &str) -> PathBuf {
        self.paths.push(rel.to_string());
        self.data.join(rel)
    }
    pub fn commit(&mut self) { self.committed = true; }
}
impl Drop for PendingFiles {
    fn drop(&mut self) {
        if !self.committed {
            for rel in &self.paths { let _ = std::fs::remove_file(self.data.join(rel)); }
        }
    }
}

fn generated_output(name: &str) -> bool {
    if let Some((prefix, tail)) = name.split_once('_') {
        if prefix.starts_with('k') && prefix.len() > 1 && prefix.as_bytes()[1..].iter().all(u8::is_ascii_digit) {
            if let Some(id) = tail.strip_suffix("_sketch.png") { return !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()); }
        }
        if prefix.starts_with('g') && prefix.len() > 1 && prefix.as_bytes()[1..].iter().all(u8::is_ascii_digit) {
            if let Some((rid, index)) = tail.split_once("_ref") {
                if let Some(i) = index.strip_suffix(".png") {
                    return !rid.is_empty() && rid.bytes().all(|b| b.is_ascii_digit()) && !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit());
                }
            }
        }
    }
    let Some((prefix, tail)) = name.split_once('_') else { return false };
    let Some((stamp, kind)) = tail.split_once('_') else { return false };
    if prefix.len() < 2 || !prefix.as_bytes()[1..].iter().copied().all(|b| b.is_ascii_digit())
        || stamp.len() < 13 || !stamp.bytes().all(|b| b.is_ascii_digit()) { return false; }
    match prefix.as_bytes()[0] {
        b'r' => ["final.jpg", "final.png", "raw.png", "msnap.png", "thumb.jpg", "crop.png", "mask.png"].contains(&kind),
        b'c' | b'f' => kind == "final.png",
        _ => false,
    }
}

/// 仅在开始接请求之前运行：崩溃留下的生成文件没有行归属，已登记文件一律保留。
pub(super) fn recover(ctx: &Shared) -> Result<usize> {
    let owned: std::collections::HashSet<_> = results::registered_paths(ctx)?.into_iter().collect();
    let root = ctx.data.join("projects");
    let dirs = match std::fs::read_dir(&root) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let mut removed = 0;
    for dir in dirs {
        let dir = dir?;
        let name = dir.file_name().to_string_lossy().into_owned();
        if !dir.file_type()?.is_dir() || name.parse::<i64>().is_err() { continue; }
        for file in std::fs::read_dir(dir.path())? {
            let file = file?;
            let filename = file.file_name().to_string_lossy().into_owned();
            if !file.file_type()?.is_file() || !generated_output(&filename) { continue; }
            let rel = format!("projects/{name}/{filename}");
            if !owned.contains(&rel) {
                std::fs::remove_file(file.path())?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn 重启回收只删未登记产物并保护原件画稿和参考图() {
        let dir = std::env::temp_dir().join(format!("synco-artifacts-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("projects/1")).unwrap();
        let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
        let original = "projects/1/r99_1791680000000_raw.png";
        let iid = crate::repo::images::insert(&ctx, 1, "photo", original, 1, 1).unwrap();
        let id = results::insert_cloud(&ctx, iid, 1, "p", "{}", "m", None).unwrap();
        let sketch = format!("projects/1/k{iid}_{id}_sketch.png");
        let reference = format!("projects/1/g{iid}_{id}_ref1.png");
        let final_path = format!("projects/1/r{id}_1791680000000_final.jpg");
        results::set_sketch(&ctx, id, &sketch).unwrap();
        results::set_refs(&ctx, id, &[reference.clone()]).unwrap();
        results::set_done(&ctx, id, &final_path, None).unwrap();
        let keep = [original.to_string(), sketch, reference, final_path, "projects/1/r100_foo_raw.png".into()];
        let orphans = ["projects/1/r98_1791680000000_raw.png", "projects/1/c97_1791680000000_final.png"];
        for p in keep.iter().map(String::as_str).chain(orphans) { std::fs::write(dir.join(p), [1]).unwrap(); }
        assert_eq!(recover(&ctx).unwrap(), 2);
        for p in keep { assert!(dir.join(p).is_file()); }
        for p in orphans { assert!(!dir.join(p).exists()); }
        assert!(!generated_output("中99_1791680000000_raw.png"));
        drop(ctx);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
