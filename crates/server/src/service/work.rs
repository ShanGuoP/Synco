//! 资料操作与图像资源的共同所有者；许可跟随实际阻塞工作，不随等待请求提前释放。
use crate::error::{AppError, Result};
use serde::Serialize;
use std::{collections::HashSet, sync::{Arc, Mutex}};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, OwnedSemaphorePermit, RwLock, Semaphore};

pub const IMAGE_SLOTS: usize = 2;
pub const IMAGE_BUDGET_MIB: u32 = 4096;

#[derive(Clone, Default, Serialize)]
pub struct WorkStats {
    pub active: usize,
    pub peak_active: usize,
    pub reserved_mib: u32,
    pub peak_reserved_mib: u32,
    pub budget_mib: u32,
    pub slots: usize,
}

pub(crate) struct Work {
    library: Arc<RwLock<()>>,
    cpu: Arc<Semaphore>,
    memory: Arc<Semaphore>,
    stats: Mutex<WorkStats>,
    deriving: Mutex<HashSet<i64>>,
}

pub(crate) struct DeriveLease { owner: Arc<Work>, id: i64 }
impl Drop for DeriveLease {
    fn drop(&mut self) { self.owner.deriving.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id); }
}

pub(crate) struct ImageLease {
    owner: Arc<Work>,
    mib: u32,
    _cpu: OwnedSemaphorePermit,
    _memory: OwnedSemaphorePermit,
}
impl Drop for ImageLease {
    fn drop(&mut self) {
        let mut stats = self.owner.stats.lock().unwrap_or_else(|e| e.into_inner());
        stats.active -= 1;
        stats.reserved_mib -= self.mib;
    }
}

impl Work {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            library: Arc::new(RwLock::new(())),
            cpu: Arc::new(Semaphore::new(IMAGE_SLOTS)),
            memory: Arc::new(Semaphore::new(IMAGE_BUDGET_MIB as usize)),
            stats: Mutex::new(WorkStats { budget_mib: IMAGE_BUDGET_MIB, slots: IMAGE_SLOTS, ..WorkStats::default() }),
            deriving: Mutex::new(HashSet::new()),
        })
    }

    /// 在任何 await/调度之前认领图片，慢派生时的轮询不能重复排入同图重活。
    pub fn claim_derive(self: &Arc<Self>, id: i64) -> Option<DeriveLease> {
        self.deriving.lock().unwrap_or_else(|e| e.into_inner()).insert(id)
            .then(|| DeriveLease { owner: self.clone(), id })
    }

    pub async fn enter(&self) -> OwnedRwLockReadGuard<()> { self.library.clone().read_owned().await }

    // 不排一个等待中的写锁：既有请求还要启动子工作，排队写锁会与嵌套读许可死锁。
    pub fn try_snapshot(&self) -> Result<OwnedRwLockWriteGuard<()>> {
        self.library.clone().try_write_owned().map_err(|_| AppError::coded(409, "srv.shell.libraryBusy"))
    }

    pub fn stats(&self) -> WorkStats { self.stats.lock().unwrap_or_else(|e| e.into_inner()).clone() }

    pub async fn image(self: &Arc<Self>, pixels: u64) -> Result<ImageLease> {
        // 未知尺寸独占预约预算；64 B/px 覆盖几何扩幅和多份 RGBA/alpha 中间缓冲。
        // 这是准入估算，不是进程 RSS 的硬上限；网络载荷与模型常驻内存另计。
        let mib = if pixels == 0 { IMAGE_BUDGET_MIB } else {
            ((pixels.saturating_mul(64).saturating_add(1_048_575) / 1_048_576) + 64).min(IMAGE_BUDGET_MIB as u64) as u32
        };
        let cpu = self.cpu.clone().acquire_owned().await.map_err(|_| AppError::fail("srv.util.poolDead"))?;
        let memory = self.memory.clone().acquire_many_owned(mib).await.map_err(|_| AppError::fail("srv.util.poolDead"))?;
        let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        stats.active += 1;
        stats.reserved_mib += mib;
        stats.peak_active = stats.peak_active.max(stats.active);
        stats.peak_reserved_mib = stats.peak_reserved_mib.max(stats.reserved_mib);
        Ok(ImageLease { owner: self.clone(), mib, _cpu: cpu, _memory: memory })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_derivation_is_rejected_until_its_owner_finishes() {
        let work = Work::new();
        let first = work.claim_derive(1).unwrap();
        assert!(work.claim_derive(1).is_none());
        assert!(work.claim_derive(2).is_some());
        drop(first);
        assert!(work.claim_derive(1).is_some());
    }

    #[tokio::test]
    async fn large_images_share_memory_budget_and_release_all_units() {
        let work = Work::new();
        let first = work.image(60_000_000).await.unwrap();
        let copy = work.clone();
        let waiting = tokio::spawn(async move { copy.image(60_000_000).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!waiting.is_finished());
        assert_eq!(work.stats().active, 1);
        drop(first);
        drop(waiting.await.unwrap());
        assert_eq!(work.stats().active, 0);
        assert_eq!(work.stats().reserved_mib, 0);
        assert_eq!(work.stats().peak_active, 1);
        assert!(work.stats().peak_reserved_mib <= IMAGE_BUDGET_MIB);
    }

    #[tokio::test(flavor="multi_thread", worker_threads=2)]
    async fn request_cancellation_does_not_release_running_image_or_library_work() {
        use crate::{repo, state::Ctx, util};
        let root = std::env::temp_dir().join(format!("synco-work-cancel-{}-{}", util::now_ms(), util::r4()));
        std::fs::create_dir_all(&root).unwrap();
        let ctx = Ctx::new(root.clone(), root.clone(), repo::db::open(&root).unwrap());
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let c = ctx.clone();
        let task = tokio::spawn(async move { util::image_work(c, 24_000_000, move || {
            started.send(()).unwrap(); wait.recv().unwrap(); Ok(())
        }).await });
        entered.await.unwrap();
        task.abort(); let _ = task.await;
        assert_eq!(ctx.work().stats().active, 1);
        assert!(ctx.work().try_snapshot().is_err());
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop { if ctx.work().stats().active == 0 && ctx.work().try_snapshot().is_ok() { break; } tokio::task::yield_now().await; }
        }).await.unwrap();
        let failed = util::image_work(ctx.clone(), 24_000_000, || -> Result<()> { panic!("injected image panic"); }).await;
        assert!(failed.is_err());
        assert_eq!(ctx.work().stats().active, 0);
        assert_eq!(ctx.work().stats().reserved_mib, 0);
        assert!(ctx.work().try_snapshot().is_ok());
        drop(ctx); std::fs::remove_dir_all(root).unwrap();
    }
}
