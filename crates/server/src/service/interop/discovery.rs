//! 本机端点发现；隔离资料目录使用隔离发现文件，不触碰用户运行中的桌面实例。
use crate::{error::Result, models::interop::uuid, state::Ctx};
use serde_json::{json, Value};
use std::{fs, io::Write, path::PathBuf, sync::Mutex};
pub struct Runtime {
    pub instance_id: String,
    info: Mutex<Value>,
    path: Mutex<Option<PathBuf>>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            instance_id: uuid(),
            info: Mutex::new(Value::Null),
            path: Mutex::new(None),
        }
    }
}
impl Runtime {
    pub fn unpublish(&self) {
        let mut slot = self.path.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = slot.as_ref() {
            if let Ok(_lock) = lock(p) {
                let owner = fs::read(p)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
                if owner.as_ref().and_then(|v| v["instance_id"].as_str())
                    == Some(self.instance_id.as_str())
                {
                    let _ = fs::remove_file(p);
                }
                slot.take();
            }
        }
    }
    pub fn info(&self) -> Value {
        self.info.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn publish(&self, ctx: &Ctx, port: u16) -> Result<()> {
        let p = std::env::var_os("SYNCO_INTEROP_DISCOVERY")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if std::env::var_os("SYNCO_DATA").is_some() {
                    ctx.data.join("runtime/interop.json")
                } else {
                    std::env::var_os("LOCALAPPDATA")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| ctx.data.clone())
                        .join("Synco/interop.json")
                }
            });
        let info = json!({"protocol":1,"instance_id":self.instance_id,"base_url":format!("http://127.0.0.1:{port}"),"executable":std::env::current_exe()?.display().to_string(),"input_formats":["jpeg","png","webp","bmp"],"output_formats":["jpeg","png"],"max_pixels":null,"max_edge":null,"max_file_bytes":null});
        fs::create_dir_all(p.parent().unwrap())?;
        let _lock = lock(&p)?;
        let tmp = p.with_extension(format!("{}.part", self.instance_id));
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec(&info).unwrap())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &p)?;
        *self.info.lock().unwrap_or_else(|e| e.into_inner()) = info;
        *self.path.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
        Ok(())
    }
}
fn lock(p: &std::path::Path) -> std::io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.share_mode(0);
    }
    opts.open(p.with_extension("lock"))
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.unpublish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_owner_cannot_remove_replacement_discovery() {
        let dir = std::env::temp_dir().join(format!("synco-discovery-{}", uuid()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("interop.json");
        let old = Runtime::default();
        let new = Runtime::default();
        *old.path.lock().unwrap() = Some(path.clone());
        *new.path.lock().unwrap() = Some(path.clone());
        fs::write(
            &path,
            serde_json::to_vec(&json!({"instance_id":new.instance_id})).unwrap(),
        )
        .unwrap();
        old.unpublish();
        assert!(path.is_file());
        new.unpublish();
        assert!(!path.exists());
        drop(old);
        drop(new);
        fs::remove_dir_all(dir).unwrap();
    }
}
