//! 見た論文の一覧（`/api/papers`）と、論文の図のキャッシュ（`/api/paper-asset`）。
//!
//! 図は arXiv の HTML 版の画像を、サーバー経由で配信して SQLite に保存する。2 回目からは arXiv にアクセスしない。
//! 取得先は /api/paper と同じく ALLOWED_HOSTS の https に限り、さらに `/html/` 以下（論文の HTML 版）に限る。

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use paper_reader_shared::{DeletePaperQuery, RecentPaper, RecentPapersResponse};
use serde::Deserialize;
use url::Url;

use crate::AppState;
use crate::arxiv;
use crate::db;
use crate::error::{AppError, AppResult};

/// 一覧で返す最大件数
const LIST_LIMIT: i64 = 300;
/// 図 1 枚の上限サイズ
const MAX_ASSET_BYTES: usize = 15 * 1024 * 1024;
/// 図を配信するパス
pub const ASSET_PATH: &str = "/api/paper-asset";

pub async fn get_papers(State(state): State<AppState>) -> AppResult<Json<RecentPapersResponse>> {
    let papers = db::list_visits(&state.db, LIST_LIMIT * 2)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    let mut papers = merge_versions(papers);
    papers.truncate(LIST_LIMIT as usize);
    let bookmarked = db::bookmarked_papers(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    for p in &mut papers {
        p.bookmarked = bookmarked.iter().any(|b| b == arxiv::base_id(&p.id));
    }
    let cache = db::cache_stats(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(RecentPapersResponse { papers, cache }))
}

pub async fn delete_paper(
    State(state): State<AppState>,
    query: Result<Query<DeletePaperQuery>, QueryRejection>,
) -> AppResult<StatusCode> {
    let Query(q) = query.map_err(|_| AppError::BadRequest("id を指定してください".into()))?;
    let id = q.id.trim();
    if !is_valid_paper_id(id) {
        return Err(AppError::BadRequest("論文 ID が不正です".into()));
    }
    let deleted = db::delete_visit(&state.db, id, q.purge)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound("その論文は一覧にありません".into()))
    }
}

/// 論文 ID として使える文字だけか（英数字と . - /、64 文字まで。LIKE の特殊文字も含まない）。
pub fn is_valid_paper_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'/'))
}

/// 同じ論文の版違い（`2509.19480` と `2509.19480v1`）を 1 件にまとめる。
///
/// 入力は最後に開いた順。まとめた行は、最も最近開いた版の ID・タイトル・キャッシュの有無を使い、
/// 開いた回数と単語・英文の数は合計する。
pub fn merge_versions(papers: Vec<RecentPaper>) -> Vec<RecentPaper> {
    let mut out: Vec<RecentPaper> = Vec::with_capacity(papers.len());
    for p in papers {
        let base = arxiv::base_id(&p.id).to_string();
        match out.iter_mut().find(|q| arxiv::base_id(&q.id) == base) {
            Some(q) => {
                q.open_count += p.open_count;
                q.word_count += p.word_count;
                q.sentence_count += p.sentence_count;
                q.cached |= p.cached && q.id == p.id;
            }
            None => out.push(p),
        }
    }
    out
}

#[derive(Debug, Deserialize)]
pub struct AssetQuery {
    pub url: String,
}

pub async fn get_asset(
    State(state): State<AppState>,
    query: Result<Query<AssetQuery>, QueryRejection>,
) -> AppResult<Response> {
    let Query(q) = query.map_err(|_| AppError::BadRequest("url を指定してください".into()))?;
    let url = validate_asset_url(&q.url, &state.config.allowed_hosts)?;

    if let Some((content_type, data)) = db::get_asset(&state.db, url.as_str())
        .await
        .map_err(|e| AppError::Internal(e.into()))?
    {
        return Ok(asset_response(&content_type, data));
    }

    let (content_type, data) = fetch_asset(&state.http, url.clone()).await?;
    let paper_id = arxiv::extract_arxiv_id(&url)
        .map(|id| arxiv::base_id(&id).to_string())
        .unwrap_or_default();
    if let Err(e) = db::put_asset(&state.db, url.as_str(), &paper_id, &content_type, &data).await {
        // キャッシュの失敗は表示を妨げない
        tracing::warn!(error = %e, "failed to cache paper asset");
    }
    Ok(asset_response(&content_type, data))
}

/// 取得してよい図の URL か（https・許可ホスト・論文の HTML 版の下）。
pub fn validate_asset_url(input: &str, allowed_hosts: &[String]) -> AppResult<Url> {
    let url = Url::parse(input).map_err(|_| AppError::BadRequest("URL が不正です".into()))?;
    arxiv::validate_url(&url, allowed_hosts).map_err(|e| AppError::BadRequest(e.to_string()))?;
    if !url.path().starts_with("/html/") || url.query().is_some() {
        return Err(AppError::BadRequest(
            "論文の HTML 版の図だけを取得できます".into(),
        ));
    }
    Ok(url)
}

async fn fetch_asset(client: &reqwest::Client, url: Url) -> AppResult<(String, Vec<u8>)> {
    let mut resp = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            AppError::UpstreamTimeout("arXiv から図を取得できませんでした（タイムアウト）".into())
        } else {
            AppError::Upstream(format!("arXiv から図を取得できませんでした: {e}"))
        }
    })?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(AppError::NotFound("図が見つかりません".into()));
    }
    if !status.is_success() {
        return Err(AppError::Upstream(format!(
            "arXiv から図を取得できませんでした（HTTP {}）",
            status.as_u16()
        )));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !content_type.starts_with("image/") {
        return Err(AppError::BadRequest("画像ではありません".into()));
    }
    if resp
        .content_length()
        .is_some_and(|n| n as usize > MAX_ASSET_BYTES)
    {
        return Err(AppError::Upstream("図が大きすぎます".into()));
    }
    let mut data = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| AppError::Upstream(format!("図の受信に失敗しました: {e}")))?
    {
        if data.len() + chunk.len() > MAX_ASSET_BYTES {
            return Err(AppError::Upstream("図が大きすぎます".into()));
        }
        data.extend_from_slice(&chunk);
    }
    Ok((content_type, data))
}

fn asset_response(content_type: &str, data: Vec<u8>) -> Response {
    let mut resp = data.into_response();
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=2592000"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // SVG などを直接開かれてもスクリプトが動かないようにする
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
    );
    resp
}

/// 無害化済みの論文 HTML の `<img src="https://arxiv.org/html/...">` を、サーバー経由（図のキャッシュ）に書き換える。
///
/// ammonia の出力（属性は `"` で囲まれ、`&` は `&amp;`）を前提にする。許可ホスト以外の画像はそのまま。
pub fn proxy_images(html: &str, allowed_hosts: &[String]) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(tag_start) = rest.find("<img") {
        let Some(tag_len) = rest[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + tag_len;
        out.push_str(&rest[..tag_start]);
        out.push_str(&rewrite_img_tag(&rest[tag_start..tag_end], allowed_hosts));
        rest = &rest[tag_end..];
    }
    out.push_str(rest);
    out
}

fn rewrite_img_tag(tag: &str, allowed_hosts: &[String]) -> String {
    let Some(attr) = tag.find(" src=\"") else {
        return tag.to_string();
    };
    let value_start = attr + " src=\"".len();
    let Some(value_len) = tag[value_start..].find('"') else {
        return tag.to_string();
    };
    let value_end = value_start + value_len;
    let original = tag[value_start..value_end].replace("&amp;", "&");
    if validate_asset_url(&original, allowed_hosts).is_err() {
        return tag.to_string();
    }
    let encoded: String = url::form_urlencoded::byte_serialize(original.as_bytes()).collect();
    format!(
        "{}{ASSET_PATH}?url={encoded}{}",
        &tag[..value_start],
        &tag[value_end..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts() -> Vec<String> {
        vec!["arxiv.org".into(), "www.arxiv.org".into()]
    }

    #[test]
    fn asset_urls() {
        assert!(validate_asset_url("https://arxiv.org/html/1706.03762v7/x1.png", &hosts()).is_ok());
        for bad in [
            "http://arxiv.org/html/1706.03762v7/x1.png",
            "https://evil.example/html/x.png",
            "https://arxiv.org/abs/1706.03762",
            "https://arxiv.org/html/x.png?a=b",
            "https://127.0.0.1/html/x.png",
            "https://user@arxiv.org/html/x.png",
            "not a url",
        ] {
            assert!(validate_asset_url(bad, &hosts()).is_err(), "{bad}");
        }
    }

    #[test]
    fn images_are_proxied() {
        let html = r#"<p>a</p><img src="https://arxiv.org/html/1706.03762v7/x1.png" alt="Fig &amp; 1"><img alt="b" src="https://evil.example/x.png"><img src="https://arxiv.org/html/2301.00001v1/x2.png?x=1&amp;y=2">tail"#;
        let out = proxy_images(html, &hosts());
        assert!(
            out.contains(r#"<img src="/api/paper-asset?url=https%3A%2F%2Farxiv.org%2Fhtml%2F1706.03762v7%2Fx1.png" alt="Fig &amp; 1">"#),
            "{out}"
        );
        // 許可ホスト以外・クエリ付きはそのまま
        assert!(out.contains(r#"src="https://evil.example/x.png""#));
        assert!(out.contains(r#"x2.png?x=1&amp;y=2""#));
        assert!(out.starts_with("<p>a</p>") && out.ends_with("tail"));
        // img が無ければ変わらない
        assert_eq!(proxy_images("<p>x</p>", &hosts()), "<p>x</p>");
    }

    fn paper(id: &str, t: i64, words: i64) -> RecentPaper {
        RecentPaper {
            id: id.into(),
            title: format!("title {id}"),
            source_url: String::new(),
            last_opened_at: t,
            open_count: 1,
            word_count: words,
            sentence_count: 1,
            cached: true,
            bookmarked: false,
        }
    }

    #[test]
    fn versions_are_merged() {
        let merged = merge_versions(vec![
            paper("2509.19480", 30, 25),
            paper("1706.03762", 20, 1),
            paper("2509.19480v1", 10, 11),
        ]);
        let ids: Vec<_> = merged.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["2509.19480", "1706.03762"]);
        assert_eq!(
            (
                merged[0].open_count,
                merged[0].word_count,
                merged[0].sentence_count
            ),
            (2, 36, 2)
        );
    }

    #[test]
    fn base_ids() {
        assert_eq!(arxiv::base_id("1706.03762v7"), "1706.03762");
        assert_eq!(arxiv::base_id("1706.03762"), "1706.03762");
        assert_eq!(arxiv::base_id("hep-th/9901001v2"), "hep-th/9901001");
    }
}
