//! 服务内共享状态：数据目录、SQLite 连接、内存里的推进状态与 HTTP 客户端。
//! `Connection` 是 Send 的，锁只在同步语句期间持有，不跨 await。

use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 一次在飞的云端任务：什么时候起的（判超时）、有没有被点了中断
pub struct LiveJob {
    pub started: Instant,
    pub cancelled: bool,
}

/// 回传成图的占位：Drop 时释放。拿不到占位说明同一行已经有人在下载了
pub struct DlGuard<'a> {
    ctx: &'a Ctx,
    id: i64,
}
impl Drop for DlGuard<'_> {
    fn drop(&mut self) {
        self.ctx.downloading().remove(&self.id);
    }
}

pub struct Ctx {
    pub data: PathBuf,
    pub public: PathBuf,
    conn: Mutex<Connection>,
    /// ComfyUI 连不上时的"连判几轮才认失败"计数
    pub misses: Mutex<HashMap<i64, u8>>,
    /// 本进程正在跑的云端任务。队列 worker 进出这里，读接口靠它区分"还在飞"和"上一轮留下的僵尸"
    pub jobs: Mutex<HashMap<i64, LiveJob>>,
    /// 正在回传成图的行。成图 20–33MB 要下好几秒，而轮询 2.5 秒一轮——没有这个占位，
    /// 两轮会各自下载一套文件，先落的那套没人认领
    pub downloading: Mutex<HashSet<i64>>,
    pub cfg: Mutex<Option<crate::service::workflow::Cache>>,
    /// 云端并发闸：许可数每轮由 `queue::sync_gate` 对齐成「设置里的并发 − 在飞数」，
    /// 起点是 0——没泵之前谁也别想发出去
    pub gate: Arc<tokio::sync::Semaphore>,
    /// 已经发出去的坑位数，也就是本进程在飞的云端任务数
    pub live: Arc<AtomicUsize>,
    /// 把「读在飞 → 读可得 → 补/削」三步串起来的那把小锁：这三步本身没有原子性
    pub gate_sync: Mutex<()>,
    /// 同时最多两份派生档在加工：一次全分辨率解码就是 100MB 级，让阻塞池随便铺开会把内存压穿，
    /// 比慢几秒严重得多
    pub slots: Arc<tokio::sync::Semaphore>,
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
            downloading: Mutex::new(HashSet::new()),
            cfg: Mutex::new(None),
            gate: Arc::new(tokio::sync::Semaphore::new(0)),
            live: Arc::new(AtomicUsize::new(0)),
            gate_sync: Mutex::new(()),
            slots: Arc::new(tokio::sync::Semaphore::const_new(2)),
        })
    }

    /// 锁中毒就接着用：这些都是短事务，中毒只意味着某个 handler panic 过，
    /// 直接 unwrap 会让整个服务跟着第一次意外就废掉。
    ///
    /// ⚠️ 这把锁不可重入：`repo::run(ctx, sql, &[repo::s(&util::now_localtime(&ctx.db()))])`
    /// 这种"参数里再取一次锁"的写法会当场死锁（对拍第一轮就被这条卡住），要先把值取成局部变量。
    ///
    /// 收不到 `pub(in crate::repo)`——可见性只能往祖先收，不能限给兄弟模块，所以只能到 `pub(crate)`：
    /// 壳层（`src-tauri`）已经摸不到连接，crate 内"绕过执行器直接握锁"由 R1 盯着。
    /// 编译期真正拦住的是另一头：`repo::one/all/run/insert_id` 与 SQL 值构造器都是
    /// `pub(in crate::repo)`，出了 DAO 拼不出一条能跑的 SQL。
    pub(crate) fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 把一行判死并写下原因。这里只管两件自己的事：清掉这行的"读不到文件"计数，
    /// 落库那半交给 `repo::results::mark_failed`（句子与参数怎么进库，DAO 的事）。
    pub fn mark_error(&self, id: i64, code: &str, args: serde_json::Value) {
        self.misses.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        crate::repo::results::mark_failed(self, id, code, args);
    }

    /// 用另一把错误的钥匙与参数判死这一行：嵌套失败（快照没写成，这一版就没有）原样传钥匙，不包句子。
    pub fn mark_error_of(&self, id: i64, e: &crate::error::AppError) {
        match e.keyed() {
            Some((code, args)) => self.mark_error(id, code, serde_json::Value::Object(args)),
            None => self.mark_error(id, &e.to_string(), serde_json::Value::Null),
        }
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

    fn downloading(&self) -> std::sync::MutexGuard<'_, HashSet<i64>> {
        self.downloading.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 占这一行的回传位：已经被占就返 `None`，调用方直接走，别下载第二套
    pub fn dl_enter(&self, id: i64) -> Option<DlGuard<'_>> {
        self.downloading().insert(id).then(|| DlGuard { ctx: self, id })
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
