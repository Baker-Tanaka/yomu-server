//! `/api/history` — 論文ごとの履歴（調べた単語・解説した文）。

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use paper_reader_shared::{
    ExplainRequest, HistoryEntry, HistoryQuery, HistoryRecord, HistoryResponse, MAX_CONTEXT_CHARS,
    MAX_TEXT_CHARS, Mode,
};

use crate::AppState;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::explain;

/// 一覧で返す最大件数
const LIST_LIMIT: i64 = 200;
const MAX_PAPER_ID_CHARS: usize = 64;
const MAX_TITLE_CHARS: usize = 500;

pub async fn post_history(
    State(state): State<AppState>,
    body: Result<Json<HistoryRecord>, JsonRejection>,
) -> AppResult<StatusCode> {
    let Json(record) =
        body.map_err(|e| AppError::BadRequest(format!("リクエストを解釈できません: {e}")))?;
    let record = validate(record)?;
    db::put_history(&state.db, &record)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_history(
    State(state): State<AppState>,
    query: Result<Query<HistoryQuery>, QueryRejection>,
) -> AppResult<Json<HistoryResponse>> {
    let Query(q) = query.map_err(|e| AppError::BadRequest(format!("クエリが不正です: {e}")))?;
    let paper = q.paper.as_deref().map(str::trim).filter(|p| !p.is_empty());
    let rows = db::list_history(&state.db, paper, LIST_LIMIT)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let mut entries = Vec::with_capacity(rows.len());
    for (id, paper_id, paper_title, mode, text, context, updated_at, tags) in rows {
        let Some(mode) = parse_mode(&mode) else {
            continue;
        };
        // 解説キャッシュと同じキーで、今のモデルの解説があるかを調べる（スラッシュリーディングの有無は問わない）
        let mut has_explanation = false;
        for slash in [false, true] {
            if slash && mode != Mode::Sentence {
                break;
            }
            let key = explain::key_for(
                &state,
                &ExplainRequest {
                    mode,
                    text: text.clone(),
                    context: context.clone(),
                    regenerate: false,
                    slash,
                },
            );
            if db::explanation_exists(&state.db, &key)
                .await
                .map_err(|e| AppError::Internal(e.into()))?
            {
                has_explanation = true;
                break;
            }
        }
        entries.push(HistoryEntry {
            id,
            paper_id,
            paper_title,
            mode,
            text,
            context,
            updated_at,
            has_explanation,
            tags: serde_json::from_str(&tags).unwrap_or_default(),
        });
    }
    Ok(Json(HistoryResponse { entries }))
}

pub async fn delete_history(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> AppResult<StatusCode> {
    let Path(id) = id.map_err(|_| AppError::BadRequest("履歴の ID が不正です".into()))?;
    let deleted = db::delete_history(&state.db, id)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound("履歴が見つかりません".into()))
    }
}

fn parse_mode(s: &str) -> Option<Mode> {
    match s {
        "word" => Some(Mode::Word),
        "sentence" => Some(Mode::Sentence),
        _ => None,
    }
}

/// 前後の空白を削り、長さを検証する（解説と同じ上限）。
pub fn validate(mut r: HistoryRecord) -> AppResult<HistoryRecord> {
    r.paper_id = r.paper_id.trim().to_string();
    r.paper_title = r.paper_title.trim().to_string();
    r.text = r.text.trim().to_string();
    r.context = r.context.trim().to_string();
    if r.paper_id.is_empty() || r.text.is_empty() {
        return Err(AppError::BadRequest("論文 ID と文字列は必須です".into()));
    }
    let too_long = |s: &str, max: usize| s.chars().count() > max;
    if too_long(&r.paper_id, MAX_PAPER_ID_CHARS)
        || too_long(&r.text, MAX_TEXT_CHARS)
        || too_long(&r.context, MAX_CONTEXT_CHARS)
    {
        return Err(AppError::BadRequest("履歴の項目が長すぎます".into()));
    }
    if too_long(&r.paper_title, MAX_TITLE_CHARS) {
        r.paper_title = r.paper_title.chars().take(MAX_TITLE_CHARS).collect();
    }
    // 文法タグは id の形のものだけ、最大 10 件
    r.tags.retain(|t| {
        !t.is_empty()
            && t.len() <= 64
            && t.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    });
    r.tags.dedup();
    r.tags.truncate(10);
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(paper_id: &str, text: &str, context: &str) -> HistoryRecord {
        HistoryRecord {
            paper_id: paper_id.into(),
            paper_title: " Title ".into(),
            mode: Mode::Word,
            text: text.into(),
            context: context.into(),
            tags: vec!["passive".into(), "Bad Tag".into(), "x".into()],
        }
    }

    #[test]
    fn validate_trims_and_limits() {
        let r = validate(rec(" 1706.03762v7 ", " attention ", " ctx ")).unwrap();
        assert_eq!(r.paper_id, "1706.03762v7");
        assert_eq!(r.paper_title, "Title");
        assert_eq!((r.text.as_str(), r.context.as_str()), ("attention", "ctx"));
        assert_eq!(r.tags, vec!["passive", "x"]);

        assert!(validate(rec("", "x", "")).is_err());
        assert!(validate(rec("p", "  ", "")).is_err());
        assert!(validate(rec("p", &"a".repeat(MAX_TEXT_CHARS + 1), "")).is_err());
        assert!(validate(rec("p", "x", &"あ".repeat(MAX_CONTEXT_CHARS + 1))).is_err());
        assert!(validate(rec(&"9".repeat(65), "x", "")).is_err());
    }

    #[test]
    fn modes() {
        assert_eq!(parse_mode("word"), Some(Mode::Word));
        assert_eq!(parse_mode("sentence"), Some(Mode::Sentence));
        assert_eq!(parse_mode("x"), None);
    }
}
