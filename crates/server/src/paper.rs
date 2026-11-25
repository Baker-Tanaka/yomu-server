//! `GET /api/paper?url=` — arXiv の HTML 版を取得し、本文を無害化して返す。

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use paper_reader_shared::{PaperQuery, PaperResponse};
use reqwest::StatusCode;
use url::Url;

use crate::AppState;
use crate::arxiv;
use crate::error::{AppError, AppResult};
use crate::sanitize;

/// 取得する HTML の上限サイズ
const MAX_HTML_BYTES: usize = 30 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

/// arXiv 取得用の HTTP クライアント。リダイレクト先も https + 許可ホストに限定する。
pub fn build_client(allowed_hosts: Vec<String>) -> anyhow::Result<reqwest::Client> {
    let hosts = Arc::new(allowed_hosts);
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("リダイレクトが多すぎます");
        }
        match arxiv::validate_url(attempt.url(), &hosts) {
            Ok(()) => attempt.follow(),
            Err(e) => attempt.error(format!("許可されていないリダイレクト先です: {e}")),
        }
    });
    Ok(reqwest::Client::builder()
        .redirect(policy)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("paper-reader/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

pub async fn get_paper(
    State(state): State<AppState>,
    query: Result<Query<PaperQuery>, QueryRejection>,
) -> AppResult<Json<PaperResponse>> {
    let Query(q) =
        query.map_err(|_| AppError::BadRequest("url パラメータを指定してください".into()))?;
    let id = arxiv::parse_paper_url(&q.url, &state.config.allowed_hosts)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    if let Some(cached) = crate::db::get_paper(&state.db, &id).await? {
        tracing::debug!(%id, "paper cache hit");
        return Ok(Json(respond(&state, cached).await));
    }

    let source = arxiv::html_url(&id);
    let source_url = Url::parse(&source).map_err(|e| AppError::Internal(e.into()))?;
    // html_url は固定ホストだが、設定の ALLOWED_HOSTS とも整合するか念のため確認する
    arxiv::validate_url(&source_url, &state.config.allowed_hosts)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let (final_url, body) = fetch_html(&state.http, source_url).await?;

    let paper = tokio::task::spawn_blocking(move || -> AppResult<PaperResponse> {
        let extracted = sanitize::extract_article(&body, &final_url).ok_or_else(|| {
            AppError::NotFound(format!(
                "論文 {id} の HTML 版から本文を見つけられませんでした"
            ))
        })?;
        Ok(PaperResponse {
            title: if extracted.title.is_empty() {
                id.clone()
            } else {
                extracted.title
            },
            html: sanitize::sanitize(&extracted.body_html, &extracted.base_url),
            source_url: final_url.to_string(),
            id,
            bookmark: None,
        })
    })
    .await
    .map_err(|e| AppError::Internal(e.into()))??;

    if let Err(e) = crate::db::put_paper(&state.db, &paper).await {
        // キャッシュの失敗は応答を妨げない
        tracing::warn!(error = %e, "failed to cache paper");
    }
    Ok(Json(respond(&state, paper).await))
}

/// 見た論文として記録し、図をサーバー経由（キャッシュ）に書き換えて返す。
async fn respond(state: &AppState, mut paper: PaperResponse) -> PaperResponse {
    if let Err(e) = crate::db::record_visit(&state.db, &paper).await {
        tracing::warn!(error = %e, "failed to record paper visit");
    }
    paper.html = crate::papers::proxy_images(&paper.html, &state.config.allowed_hosts);
    // 付箋も一緒に返す（開いてすぐ付箋の位置へ移れるように）
    paper.bookmark = match crate::db::get_bookmark(&state.db, arxiv::base_id(&paper.id)).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load bookmark");
            None
        }
    };
    paper
}

async fn fetch_html(client: &reqwest::Client, url: Url) -> AppResult<(Url, String)> {
    let mut resp = client.get(url.clone()).send().await.map_err(map_reqwest)?;
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
        return Err(AppError::NotFound(
            "この論文には HTML 版がありません（arXiv で HTML 版が生成されていないか、論文 ID が存在しません）"
                .into(),
        ));
    }
    if !status.is_success() {
        return Err(AppError::Upstream(format!(
            "arXiv から取得できませんでした（HTTP {}）",
            status.as_u16()
        )));
    }
    if resp
        .content_length()
        .is_some_and(|n| n as usize > MAX_HTML_BYTES)
    {
        return Err(AppError::Upstream("論文の HTML が大きすぎます".into()));
    }
    let final_url = resp.url().clone();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(map_reqwest)? {
        if buf.len() + chunk.len() > MAX_HTML_BYTES {
            return Err(AppError::Upstream("論文の HTML が大きすぎます".into()));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok((final_url, String::from_utf8_lossy(&buf).into_owned()))
}

fn map_reqwest(e: reqwest::Error) -> AppError {
    if e.is_timeout() {
        AppError::UpstreamTimeout("arXiv への接続がタイムアウトしました".into())
    } else if e.is_redirect() {
        AppError::BadRequest(format!("リダイレクトを拒否しました: {e}"))
    } else {
        AppError::Upstream(format!("arXiv への接続に失敗しました: {e}"))
    }
}
