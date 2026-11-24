//! `GET /api/grammar-map` と `POST /api/admin/reload-grammar`。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use paper_reader_shared::{
    GrammarCategoryInfo, GrammarMapResponse, GrammarTagInfo, OUT_OF_COURSE_CATEGORY,
};
use serde_json::{Value, json};

use crate::AppState;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::grammar::{CATEGORIES, GrammarData, Scope};

/// タグ一覧・カテゴリ・レッスンと、履歴での出現回数。
pub async fn get_grammar_map(State(state): State<AppState>) -> AppResult<Json<GrammarMapResponse>> {
    let g = state.grammar();
    let counts: HashMap<String, i64> = db::tag_counts(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?
        .into_iter()
        .collect();
    Ok(Json(map_response(&g, &counts)))
}

pub fn map_response(g: &GrammarData, counts: &HashMap<String, i64>) -> GrammarMapResponse {
    let categories = CATEGORIES
        .iter()
        .map(|c| GrammarCategoryInfo {
            name: (*c).to_string(),
            in_course: *c != OUT_OF_COURSE_CATEGORY,
            lesson_count: g.lesson_count(c),
        })
        .collect();
    let tags = g
        .tags
        .iter()
        .map(|t| {
            let (in_scope, lessons) = match g.scope(&t.id) {
                Scope::Lessons(l) => (true, l),
                Scope::CategoryOnly => (true, Vec::new()),
                Scope::OutOfCourse => (false, Vec::new()),
            };
            GrammarTagInfo {
                id: t.id.clone(),
                name: t.name.clone(),
                category: t.category.clone(),
                aliases: t.aliases.clone(),
                hint: t.hint.clone(),
                in_scope,
                lessons,
                count: counts.get(&t.id).copied().unwrap_or(0).max(0) as u64,
            }
        })
        .collect();
    GrammarMapResponse {
        course: g.course.clone(),
        course_short: g.course_short.clone(),
        source: g.source.as_str().to_string(),
        categories,
        tags,
    }
}

/// 文法タグとレッスン対応を読み直す（開発用）。127.0.0.1 から直接来たものだけ受け付ける。
///
/// tailscale serve などのリバースプロキシ経由の要求も接続元は 127.0.0.1 になるため、
/// 転送ヘッダー（X-Forwarded-For / Forwarded / Tailscale-*）が付いていれば拒否する。
pub async fn post_reload(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    if !is_direct_local(&peer, &headers) {
        return Err(AppError::Forbidden(
            "この操作はサーバーの Mac から直接（127.0.0.1）しか行えません".into(),
        ));
    }
    let paths = state.config.grammar_paths();
    let g = tokio::task::spawn_blocking(move || GrammarData::load(&paths))
        .await
        .map_err(|e| AppError::Internal(e.into()))?
        // 読み込めなければ今のデータを使い続ける
        .map_err(|e| AppError::BadRequest(format!("{e:#}")))?;
    let summary = json!({
        "tags": g.tags.len(),
        "lessons": g.lessons.len(),
        "source": g.source.as_str(),
    });
    tracing::info!(%summary, "grammar reloaded");
    state.set_grammar(Arc::new(g));
    Ok(Json(summary))
}

pub fn is_direct_local(peer: &SocketAddr, headers: &HeaderMap) -> bool {
    let proxied = headers.contains_key("x-forwarded-for")
        || headers.contains_key("forwarded")
        || headers.keys().any(|k| k.as_str().starts_with("tailscale-"));
    peer.ip().is_loopback() && !proxied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::tests::sample;
    use axum::http::HeaderValue;

    #[test]
    fn local_only() {
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let remote: SocketAddr = "100.64.0.2:5000".parse().unwrap();
        let empty = HeaderMap::new();
        assert!(is_direct_local(&local, &empty));
        assert!(!is_direct_local(&remote, &empty));
        let mut proxied = HeaderMap::new();
        proxied.insert("tailscale-user-login", HeaderValue::from_static("x"));
        assert!(!is_direct_local(&local, &proxied));
        let mut xff = HeaderMap::new();
        xff.insert("x-forwarded-for", HeaderValue::from_static("100.64.0.2"));
        assert!(!is_direct_local(&local, &xff));
    }

    #[test]
    fn response_has_counts_and_scope() {
        let g = sample();
        let counts = HashMap::from([("passive".to_string(), 3)]);
        let r = map_response(&g, &counts);
        assert_eq!(r.source, "local");
        assert_eq!(r.categories.len(), CATEGORIES.len());
        let rel = r.categories.iter().find(|c| c.name == "関係詞").unwrap();
        assert_eq!(rel.lesson_count, 1);
        let passive = r.tags.iter().find(|t| t.id == "passive").unwrap();
        assert_eq!((passive.count, passive.in_scope), (3, true));
        let app = r.tags.iter().find(|t| t.id == "appositive-that").unwrap();
        assert!(!app.in_scope);
    }
}
