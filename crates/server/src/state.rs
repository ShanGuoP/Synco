//! 服务内共享状态：数据目录、SQLite 连接、内存里的推进状态与 HTTP 客户端。
//! `Connection` 是 Send 的，锁只在同步语句期间持有，不跨 await。

use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 一次在飞的云端任务：什么时候起的（判超时）、有没有被点了中断
pub struct LiveJob {
    pub started: Instant,
    pub cancelled: bool,
}

pub struct Ctx {
    pub data: PathBuf,
    pub public: PathBuf,
    pub conn: Mutex<Connection>,
    /// ComfyUI 连不上时的"连判几轮才认失败"计数
    pub misses: Mutex<HashMap<i64, u8>>,
    /// 本进程正在跑的云端任务。队列 worker 进出这里，读接口靠它区分"还在飞"和"上一轮留下的僵尸"
    pub jobs: Mutex<HashMap<i64, LiveJob>>,
    pub cfg: Mutex<Option<crate::service::workflow::Cache>>,
    pub http: reqwest::Client,
}

pub type Shared = Arc<Ctx>;

impl Ctx {
    pub fn new(data: PathBuf, public: PathBuf, conn: Connection) -> Shared {
        Arc::new(Self {
            data,
            public,
            // 不跟随系统代理：探活与 ComfyUI/云端调用都是本机或用户手填的地址，
            // 走系统代理会让 127.0.0.1 的连接被拐去代理端，探活从 17ms 变 4.5s
            http: reqwest::Client::builder().no_proxy().build().expect("reqwest client"),
            conn: Mutex::new(conn),
            misses: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            cfg: Mutex::new(None),
        })
    }

    /// 锁中毒就接着用：这些都是短事务，中毒只意味着某个 handler panic 过，
    /// 直接 unwrap 会让整个服务跟着第一次意外就废掉。
    ///
    /// ⚠️ 这把锁不可重入：`repo::run(ctx, sql, &[repo::s(&util::now_localtime(&ctx.db()))])`
    /// 这种"参数里再取一次锁"的写法会当场死锁（对拍第一轮就被这条卡住），要先把值取成局部变量。
    pub fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn mark_error(&self, id: i64, msg: &str) {
        self.misses.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        let truncated: String = msg.chars().take(400).collect();
        let _ = self
            .db()
            .prepare_cached("UPDATE results SET status=?, error=? WHERE id=?")
            .and_then(|mut st| st.execute(("error", truncated.as_str(), id)));
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, HashMap<i64, LiveJob>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// worker 开工：记这一刻，读接口据此区分"还在飞"与"僵尸"
    pub fn job_begin(&self, id: i64) {
        self.jobs().insert(id, LiveJob { started: Instant::now(), cancelled: false });
    }
    pub fn job_end(&self, id: i64) {
        self.jobs().remove(&id);
    }
    pub fn job_live(&self, id: i64) -> bool {
        self.jobs().contains_key(&id)
    }
    /// 点中断：在飞的标记一下，worker 落盘前会看一眼；返回有没有这份在飞的活
    pub fn job_cancel(&self, id: i64) -> bool {
        match self.jobs().get_mut(&id) {
            Some(j) => {
                j.cancelled = true;
                true
            }
            None => false,
        }
    }
    pub fn job_cancelled(&self, id: i64) -> bool {
        self.jobs().get(&id).map(|j| j.cancelled).unwrap_or(false)
    }
    /// 起表之后超过宽限期还没落盘：对面的请求多半已经断了
    pub fn job_overdue(&self, id: i64, grace_ms: u128) -> bool {
        self.jobs().get(&id).map(|j| j.started.elapsed().as_millis() > grace_ms).unwrap_or(false)
    }
}
