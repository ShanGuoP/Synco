//! 命令行入口：所有启动语义都在 `lib.rs`，这里只决定"用哪个数据目录跑"，
//! 以及起不来的时候把**给人看的那一句**留在控制台里（`Debug` 形状是给开发者的，不是理由）。

use synco_server::error::AppError;

#[tokio::main]
async fn main() {
    let boot = match synco_server::serve(synco_server::data_dir(), synco_server::public_dir(), synco_server::want_port()).await {
        Ok(b) => b,
        Err(e) => die(&e),
    };
    // 监听任务正常退出只会是 axum 停了；崩掉的那次拿不到返回值，只能说"任务没了"
    if let Err(e) = boot.server.await {
        die(&AppError::fail_args("srv.app.taskPanic", serde_json::json!({ "msg": e.to_string() })));
    }
}

fn die(e: &AppError) -> ! {
    eprintln!("  本地服务起不来：{}", e.text());
    std::process::exit(1);
}
