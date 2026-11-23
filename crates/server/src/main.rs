mod arxiv;
mod config;
mod db;
mod error;
mod paper;
mod sanitize;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use paper_reader_shared::HealthResponse;
use sqlx::SqlitePool;
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::error::AppError;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    /// arXiv 取得用（リダイレクト検証つき）
    pub http: reqwest::Client,
    /// LLM ランタイム用（127.0.0.1 のみ）
    pub llm_http: reqwest::Client,
}

#[tokio::main]
async fn main() -> Result<()> {
    // .env が無くてもよい
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,sqlx=warn")),
        )
        .init();

    let config = Config::from_env()?;
    tracing::info!(?config, "starting");

    let db = db::connect(&config.database_url).await?;
    let http = paper::build_client(config.allowed_hosts.clone())?;
    let llm_http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .build()?;

    let state = AppState {
        config: Arc::new(config.clone()),
        db,
        http,
        llm_http,
    };

    let app = router(state, &config);
    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("{} に bind できません", config.bind_addr))?;
    tracing::info!("listening on http://{}", config.bind_addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn router(state: AppState, config: &Config) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/paper", get(paper::get_paper))
        .fallback(|| async { AppError::NotFound("API が見つかりません".into()) });

    let index = config.static_dir.join("index.html");
    let static_files = ServeDir::new(&config.static_dir).fallback(ServeFile::new(index));

    Router::new()
        .nest("/api", api)
        .fallback_service(static_files)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let url = format!("{}/models", state.config.llm_base_url.trim_end_matches('/'));
    let llm = match state.llm_http.get(&url).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    };
    Json(HealthResponse {
        server: true,
        llm: Some(llm),
    })
}

async fn shutdown_signal() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %e, "failed to listen for ctrl-c");
    }
}
