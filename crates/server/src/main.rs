mod arxiv;
mod bookmark;
mod cache;
mod config;
mod db;
mod dict;
mod error;
mod explain;
mod feedback;
mod grammar;
mod grammar_api;
mod history;
mod llm;
mod paper;
mod papers;
mod prompts;
mod sanitize;
mod search;
mod static_files;
mod tagger;
mod words;

use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use paper_reader_shared::HealthResponse;
use sqlx::SqlitePool;
use tower_http::compression::CompressionLayer;
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
    /// 解説のプロンプトを最後に LLM へ読み込ませた時刻（UNIX 秒。explain::warm_up）
    pub warmed_at: Arc<std::sync::atomic::AtomicI64>,
    /// arXiv API（検索）用（宛先は export.arxiv.org に固定）
    pub search_http: reqwest::Client,
    pub searcher: search::SharedSearcher,
    /// 文法タグ（/api/admin/reload-grammar で差し替える）
    grammar: Arc<RwLock<Arc<grammar::GrammarData>>>,
    /// LLM ランタイムが response_format: json_schema を使えるか
    pub structured_output: Arc<AtomicBool>,
    /// GitHub API 用（フィードバックを Issue にする）
    pub github_http: reqwest::Client,
    pub feedback: Arc<feedback::FeedbackState>,
}

impl AppState {
    pub fn grammar(&self) -> Arc<grammar::GrammarData> {
        match self.grammar.read() {
            Ok(g) => g.clone(),
            // 書き込み中に panic したことは無いはずだが、その場合も読めるものを使う
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn set_grammar(&self, g: Arc<grammar::GrammarData>) {
        match self.grammar.write() {
            Ok(mut cur) => *cur = g,
            Err(poisoned) => *poisoned.into_inner() = g,
        }
    }
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

    // 文法タグを読み込んで検証する（不正なら起動しない）
    let grammar_data = grammar::GrammarData::load(&config.grammar_tags_path)
        .context("文法タグの読み込みに失敗しました")?;
    tracing::info!(tags = grammar_data.tags.len(), "grammar loaded");

    let db = db::connect(&config.database_url).await?;
    let http = paper::build_client(config.allowed_hosts.clone())?;
    let search_http = paper::build_client(vec![search::API_HOST.to_string()])?;
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
        warmed_at: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        search_http,
        searcher: Arc::default(),
        grammar: Arc::new(RwLock::new(Arc::new(grammar_data))),
        structured_output: Arc::new(AtomicBool::new(true)),
        github_http: feedback::build_client()?,
        feedback: Arc::default(),
    };
    match &config.github_repo {
        Some(repo) if config.github_token.is_some() => {
            tracing::info!(%repo, "feedback will be sent to GitHub issues");
            // 前回までに送れなかったフィードバックを送る
            tokio::spawn(feedback::flush_pending(state.clone()));
        }
        _ => tracing::info!(
            "GITHUB_REPO / GITHUB_TOKEN が未設定のため、フィードバックは保存だけします"
        ),
    }

    let app = router(state, &config);
    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("{} に bind できません", config.bind_addr))?;
    tracing::info!(
        "paper-reader {} listening on http://{}",
        paper_reader_shared::build_version_label(),
        config.bind_addr
    );
    // /api/admin/* で接続元を確認するため、接続情報を付ける
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

fn router(state: AppState, config: &Config) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/paper", get(paper::get_paper))
        .route(
            "/papers",
            get(papers::get_papers).delete(papers::delete_paper),
        )
        .route("/paper-asset", get(papers::get_asset))
        .route(
            "/bookmark",
            put(bookmark::put_bookmark).delete(bookmark::delete_bookmark),
        )
        .route("/dict", get(dict::get_dict))
        .route("/dict/ai", post(words::post_dict_ai))
        .route("/words", get(words::get_words).post(words::post_word))
        .route("/words/delete", post(words::post_delete))
        .route("/words/prune", post(words::post_prune))
        .route("/search", get(search::get_search))
        .route("/explain", post(explain::post_explain))
        .route(
            "/history",
            get(history::get_history).post(history::post_history),
        )
        .route("/history/{id}", delete(history::delete_history))
        .route("/grammar-tags", get(grammar_api::get_grammar_tags))
        .route(
            "/feedback",
            get(feedback::get_feedback).post(feedback::post_submit),
        )
        .route("/feedback/preview", post(feedback::post_preview))
        .route("/feedback/{id}/retry", post(feedback::post_retry))
        .route("/admin/reload-grammar", post(grammar_api::post_reload))
        .fallback(|| async { AppError::NotFound("API が見つかりません".into()) });

    // 実験モード（端末の LLM）の WebLLM とモデル。重みは圧縮しても小さくならず大きいので、
    // CompressionLayer の外に置く（layer より後に足したルートには layer がかからない）
    let webllm = Router::new()
        .fallback_service(tower_http::services::ServeDir::new(&config.webllm_dir))
        .layer(axum::middleware::from_fn(static_files::webllm_headers));

    Router::new()
        .nest("/api", api)
        .fallback_service(static_files::service(&config.static_dir))
        .layer(axum::middleware::from_fn(static_files::cache_headers))
        // SSE（text/event-stream）は圧縮しない（tower-http の既定）。.br / .gz があるファイルはそのまま返す
        .layer(CompressionLayer::new())
        .nest("/webllm", webllm)
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
        version: paper_reader_shared::APP_VERSION.to_string(),
        git_hash: paper_reader_shared::GIT_HASH.to_string(),
        git_date: paper_reader_shared::GIT_DATE.to_string(),
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

/// Ctrl-C（SIGINT）か、launchd が停止時に送る SIGTERM で終了する。
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to listen for ctrl-c");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutting down");
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
