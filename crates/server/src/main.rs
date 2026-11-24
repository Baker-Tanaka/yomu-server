mod arxiv;
mod config;
mod db;
mod dict;
mod error;
mod explain;
mod paper;
mod prompts;
mod sanitize;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::routing::{get, post};
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
    /// LLM ランタイムの疎通確認用（短いタイムアウト）
    pub llm_http: reqwest::Client,
    /// LLM の解説ストリーム用（全体のタイムアウトは無く、無通信が続いたら打ち切る）
    pub llm_stream_http: reqwest::Client,
    /// LLM への同時リクエストを 1 件に制限する
    pub llm_gate: Arc<tokio::sync::Semaphore>,
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
    tracing::info!(
        ?config,
        prompt_version = prompts::PROMPT_VERSION,
        "starting"
    );

    let db = db::connect(&config.database_url).await?;
    let http = paper::build_client(config.allowed_hosts.clone())?;
    let llm_http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .build()?;
    let llm_stream_http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        // モデルの読み込みに時間がかかることがあるので長めにする
        .read_timeout(Duration::from_secs(120))
        .build()?;

    let state = AppState {
        config: Arc::new(config.clone()),
        db,
        http,
        llm_http,
        llm_stream_http,
        llm_gate: Arc::new(tokio::sync::Semaphore::new(1)),
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
        .route("/dict", get(dict::get_dict))
        .route("/explain", post(explain::post_explain))
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
    let model = state.config.llm_model.clone();
    let url = format!("{}/models", state.config.llm_base_url.trim_end_matches('/'));
    let models: Option<Vec<String>> = match state.llm_http.get(&url).send().await {
        Ok(r) if r.status().is_success() => r
            .json::<serde_json::Value>()
            .await
            .ok()
            .map(|v| model_ids(&v)),
        _ => None,
    };
    Json(HealthResponse {
        server: true,
        llm: Some(models.is_some()),
        model_available: models.map(|ids| ids.iter().any(|id| model_matches(id, &model))),
        model,
    })
}

/// OpenAI 互換 /models の `data[].id` を取り出す。
fn model_ids(v: &serde_json::Value) -> Vec<String> {
    v.get("data")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// "qwen3:8b" と "qwen3:8b" は一致。タグ省略時は ":latest" とみなす。
fn model_matches(id: &str, model: &str) -> bool {
    let norm = |s: &str| {
        if s.contains(':') {
            s.to_string()
        } else {
            format!("{s}:latest")
        }
    };
    !model.is_empty() && norm(id) == norm(model)
}

async fn shutdown_signal() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %e, "failed to listen for ctrl-c");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models() {
        let v = serde_json::json!({"object":"list","data":[{"id":"gemma4:e4b"},{"id":"qwen3"}]});
        let ids = model_ids(&v);
        assert_eq!(ids, vec!["gemma4:e4b", "qwen3"]);
        assert!(model_matches("gemma4:e4b", "gemma4:e4b"));
        assert!(model_matches("qwen3", "qwen3:latest"));
        assert!(!model_matches("gemma4:e4b", "gemma4:12b"));
        assert!(!model_matches("x", ""));
    }
}
