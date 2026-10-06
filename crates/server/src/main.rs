//! 命令行入口：所有启动语义都在 `lib.rs`，这里只决定"用哪个数据目录跑"。

use synco_server::error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let boot = synco_server::serve(synco_server::data_dir(), synco_server::public_dir(), synco_server::want_port()).await?;
    // 监听任务正常退出只会是 axum 停了；把它的错误当进程错误报出去
    match boot.server.await {
        Ok(()) => Ok(()),
        Err(e) => Err(synco_server::error::AppError::Fail(format!("服务任务崩了：{e}"))),
    }
}
