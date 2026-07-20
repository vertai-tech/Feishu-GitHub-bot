mod binding;
mod cards;
mod config;
mod feishu;
mod github;
mod handlers;
mod state;
mod store;

use axum::{
    routing::{get, post},
    Router,
};
use config::Config;
use state::AppState;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,feishu_github_bot=info".into()),
        )
        .init();

    // 让后台 tokio 任务里的 panic 也能被记录，而非静默丢失。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default_hook(info);
    }));

    // 预览模式：打印所有卡片 JSON（[{name, card}, ...]）后退出，供本地可视化核对。
    if std::env::args().nth(1).as_deref() == Some("dump-cards") {
        let items: Vec<_> = cards::sample_cards()
            .into_iter()
            .map(|(name, card)| serde_json::json!({ "name": name, "card": card }))
            .collect();
        println!("{}", serde_json::to_string(&items)?);
        return Ok(());
    }

    // 配置文件路径：第一个参数或环境变量 FGB_CONFIG，默认 ./config.toml
    let config_path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("FGB_CONFIG").ok())
        .unwrap_or_else(|| "config.toml".to_string());
    let cfg = Config::load(&config_path)?;
    info!("已加载配置 {config_path}，监听 {}", cfg.listen_addr);

    let listen_addr = cfg.listen_addr.clone();
    let state = AppState::new(cfg);

    let app = Router::new()
        .route("/health", get(handlers::health))
        .route("/webhook/github", post(handlers::github_webhook))
        .route("/webhook/feishu", post(handlers::feishu_webhook))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    info!("服务启动，监听 http://{listen_addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
