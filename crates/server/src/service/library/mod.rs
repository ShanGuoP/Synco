//! 资料快照用例：无活跃任务时冻结本实例的全部资料操作，备份数据库、校验文件树后整体发布。
use crate::{error::{AppError, Result}, repo, state::Shared, util};
use std::{collections::BTreeMap, fs, io::Read, path::{Path, PathBuf}};
use sha2::{Digest, Sha256};

pub struct Copied { pub path: PathBuf, pub files: usize, pub bytes: u64 }

pub async fn copy(ctx: &Shared, destination: &Path) -> Result<Copied> {
    let lease = ctx.work().try_snapshot()?;
    let (queued, running) = repo::results::count_by_status(ctx)?;
    if queued != 0 || running != 0 { return Err(AppError::coded(409, "srv.shell.libraryBusy")); }
    let c = ctx.clone();
    let destination = destination.to_path_buf();
    util::blocking(move || {
        // 请求被取消也不能提前解冻；写许可随实际磁盘工作存活。
        let _lease = lease;
        snapshot(&c, &destination)
    }).await
}

pub async fn selectable(ctx: &Shared, path: &Path) -> Result<PathBuf> {
    let path = path.to_path_buf();
    util::library_work(ctx.clone(), move || {
        if !path.is_absolute() { return Err(AppError::bad("srv.shell.absNeeded")); }
        fs::create_dir_all(&path)?;
        let path = path_identity(&path)?;
        let probe = path.join(format!(".synco-write-{}-{}", util::now_ms(), util::r4()));
        let file = fs::OpenOptions::new().create_new(true).write(true).open(&probe)?;
        drop(file);
        fs::remove_file(probe)?;
        Ok(path)
    }).await
}

pub fn path_identity(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) { return Err(AppError::bad("srv.shell.absNeeded")); }
    if fs::symlink_metadata(path).is_ok() { return Ok(fs::canonicalize(path)?); }
    let parent = path.parent().ok_or_else(|| AppError::bad("srv.shell.absNeeded"))?;
    let name = path.file_name().ok_or_else(|| AppError::bad("srv.shell.absNeeded"))?;
    Ok(path_identity(parent)?.join(name))
}

fn copy_destination(source: &Path, destination: &Path) -> Result<PathBuf> {
    let source = fs::canonicalize(source)?;
    let destination = path_identity(destination)?;
    #[cfg(windows)] let inside = { let src = source.to_string_lossy().to_lowercase(); let dst = destination.to_string_lossy().to_lowercase(); src == dst || dst.starts_with(&format!("{}\\", src.trim_end_matches('\\'))) };
    #[cfg(not(windows))] let inside = destination.starts_with(&source);
    if inside { return Err(AppError::bad("srv.shell.copyInsideSource")); }
    Ok(destination)
}

pub fn directory_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)] { use std::os::windows::fs::MetadataExt; metadata.file_attributes() & 0x400 != 0 }
    #[cfg(not(windows))] { metadata.file_type().is_symlink() }
}

fn skip(name: &str) -> bool {
    ["runtime", "app.db", "port.txt", "desktop.json", "instance.lock", "server.log"].contains(&name) || name.starts_with("app.db-")
}
type Manifest = BTreeMap<PathBuf, (u64, Vec<u8>)>;

fn manifest(root: &Path) -> Result<Manifest> {
    fn visit(root: &Path, dir: &Path, out: &mut Manifest) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if dir == root && skip(&entry.file_name().to_string_lossy()) { continue; }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if directory_link(&metadata) { return Err(AppError::bad_args("srv.shell.copyLink", serde_json::json!({"path":path.display().to_string()}))); }
            if metadata.is_dir() { visit(root, &path, out)?; }
            else if metadata.is_file() {
                let mut file = fs::File::open(&path)?;
                let mut hash = Sha256::new();
                let mut bytes = 0;
                let mut buf = [0; 65536];
                loop { let n = file.read(&mut buf)?; if n == 0 { break; } hash.update(&buf[..n]); bytes += n as u64; }
                out.insert(path.strip_prefix(root).map_err(|e| AppError::Fail(e.to_string()))?.to_path_buf(), (bytes, hash.finalize().to_vec()));
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out)?;
    Ok(out)
}

struct Stage(PathBuf);
impl Drop for Stage { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

struct Draft { destination: PathBuf, stage: Stage, version: i64, before: Manifest }

fn prepare(ctx: &Shared, destination: &Path) -> Result<Draft> {
    let destination = copy_destination(&ctx.data, destination)?;
    if destination.join("app.db").exists() { return Err(AppError::bad("srv.shell.targetHasLibrary")); }
    if destination.exists() && fs::read_dir(&destination)?.next().is_some() { return Err(AppError::bad("srv.shell.targetNotEmpty")); }
    let parent = destination.parent().ok_or_else(|| AppError::bad("srv.shell.absDirNeeded"))?;
    fs::create_dir_all(parent)?;
    let stage = copy_destination(&ctx.data, &parent.join(format!(".synco-copy-{}-{}", util::now_ms(), util::r4())))?;
    fs::create_dir(&stage)?;
    // 只有本次成功创建的、已确认在源外的暂存根才交给清理守卫。
    let stage = Stage(stage);
    let version = repo::db::data_version(ctx)?;
    let before = manifest(&ctx.data)?;
    let source = fs::canonicalize(&ctx.data)?;
    let mut settings = Vec::new();
    for (key, value) in repo::settings::workflow_locations(ctx)? {
        let prefix = ["workflow_roles:", "workflow_profile:"].into_iter().find(|p| key.starts_with(p));
        let raw = match prefix { Some(prefix) => &key[prefix.len()..], None => &value };
        if let Ok(path) = fs::canonicalize(raw) {
            if let Ok(rel) = path.strip_prefix(&source) {
                // 指向被排除的运行时材料不能作为可迁移配置发布。
                if !before.contains_key(rel) { return Err(AppError::fail("srv.shell.sourceChanged")); }
                let next = util::rel_slash(&destination.join(rel));
                settings.push(match prefix { Some(prefix) => (key.clone(), format!("{prefix}{next}"), value), None => (key.clone(), key, next) });
            }
        }
    }
    repo::db::snapshot_into(ctx, &stage.0.join("app.db"), &settings)?;
    Ok(Draft { destination, stage, version, before })
}

impl Draft {
    fn copy_files(&self, ctx: &Shared) -> Result<()> {
        for rel in self.before.keys() {
            let destination = self.stage.0.join(rel);
            if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
            fs::copy(ctx.data.join(rel), destination)?;
        }
        Ok(())
    }
    fn publish(self, ctx: &Shared) -> Result<Copied> {
        if repo::db::data_version(ctx)? != self.version || manifest(&ctx.data)? != self.before || manifest(&self.stage.0)? != self.before {
            return Err(AppError::fail("srv.shell.sourceChanged"));
        }
        let files = self.before.len() + 1;
        let bytes = self.before.values().map(|(n, _)| n).sum::<u64>() + fs::metadata(self.stage.0.join("app.db"))?.len();
        if self.destination.exists() { fs::remove_dir(&self.destination)?; }
        fs::rename(&self.stage.0, &self.destination)?;
        Ok(Copied { path: self.destination.clone(), files, bytes })
    }
}

fn snapshot(ctx: &Shared, destination: &Path) -> Result<Copied> {
    let draft = prepare(ctx, destination)?;
    draft.copy_files(ctx)?;
    draft.publish(ctx)
}

#[cfg(test)]
mod tests;
