//! 後で読む論文: `GET` / `PUT /api/read-later` と `DELETE /api/read-later?paper_id=`。
//!
//! 後から積んだものを上に並べる。版違い（`2509.19480v1` と `v2`）は 1 件にまとめる（版なしの ID で保存する）。
//! 本文は取得しない（開くときに `/api/paper` が取得する）ので、arXiv へのアクセスは増えない。

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use paper_reader_shared::{
    MAX_READ_LATER, ReadLaterPut, ReadLaterQuery, ReadLaterResponse, is_arxiv_id,
    normalize_whitespace,
};

use crate::AppState;
use crate::bookmark::valid_base_id;
use crate::db;
use crate::error::{AppError, AppResult};

const MAX_TITLE_CHARS: usize = 500;
const MAX_AUTHORS: usize = 100;
const MAX_AUTHOR_CHARS: usize = 100;

pub async fn get_list(State(state): State<AppState>) -> AppResult<Json<ReadLaterResponse>> {
    let items = db::list_read_later(&state.db).await?;
    Ok(Json(ReadLaterResponse { items }))
}

pub async fn put_item(
    State(state): State<AppState>,
    body: Result<Json<ReadLaterPut>, JsonRejection>,
) -> AppResult<StatusCode> {
    let Json(req) =
        body.map_err(|e| AppError::BadRequest(format!("リクエストを解釈できません: {e}")))?;
    let base = valid_arxiv_base_id(&req.paper_id)?;
    let title = clean_text(&req.title, MAX_TITLE_CHARS);
    if title.is_empty() {
        return Err(AppError::BadRequest("論文の題名がありません".into()));
    }
    let authors = clean_authors(&req.authors);
    if !db::put_read_later(&state.db, base, &title, &authors, MAX_READ_LATER).await? {
        return Err(AppError::BadRequest(format!(
            "「後で読む」がいっぱいです（上限 {MAX_READ_LATER} 件）。読み終えたものを外してください"
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_item(
    State(state): State<AppState>,
    query: Result<Query<ReadLaterQuery>, QueryRejection>,
) -> AppResult<StatusCode> {
    let Query(q) = query.map_err(|_| AppError::BadRequest("paper_id を指定してください".into()))?;
    let base = valid_base_id(&q.paper_id)?;
    // 外す前から無くても、結果は同じなので成功とする
    db::delete_read_later(&state.db, base).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// arXiv ID の形か確かめ、版なしの ID を返す（一覧から開くときに URL に入れるため、形を厳しく見る）。
fn valid_arxiv_base_id(id: &str) -> AppResult<&str> {
    let base = valid_base_id(id)?;
    if is_arxiv_id(base) {
        Ok(base)
    } else {
        Err(AppError::BadRequest("arXiv の論文 ID が不正です".into()))
    }
}

/// 空白をまとめ、長さを抑える。
fn clean_text(s: &str, max: usize) -> String {
    normalize_whitespace(s).chars().take(max).collect()
}

fn clean_authors(authors: &[String]) -> Vec<String> {
    authors
        .iter()
        .map(|a| clean_text(a, MAX_AUTHOR_CHARS))
        .filter(|a| !a.is_empty())
        .take(MAX_AUTHORS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert_eq!(valid_arxiv_base_id("2509.19480v2").ok(), Some("2509.19480"));
        assert!(valid_arxiv_base_id("not-a-paper").is_err());
        assert!(valid_arxiv_base_id("../x").is_err());
    }

    #[test]
    fn cleans_input() {
        assert_eq!(clean_text("  A\n  title ", 100), "A title");
        assert_eq!(clean_text("abcdef", 3), "abc");
        let many: Vec<String> = (0..200).map(|i| format!(" a{i} ")).collect();
        assert_eq!(clean_authors(&many).len(), MAX_AUTHORS);
        assert_eq!(clean_authors(&["  ".into(), "B".into()]), vec!["B"]);
    }

    #[tokio::test]
    async fn stack_order_limit_and_delete() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let ids = |items: Vec<paper_reader_shared::ReadLaterItem>| {
            items.into_iter().map(|i| i.id).collect::<Vec<_>>()
        };
        let authors = vec!["A".to_string()];
        for id in ["1111.00001", "1111.00002"] {
            assert!(
                db::put_read_later(&pool, id, id, &authors, 2)
                    .await
                    .unwrap()
            );
        }
        let list = db::list_read_later(&pool).await.unwrap();
        assert_eq!(list[0].authors, authors);
        assert_eq!(ids(list), ["1111.00002", "1111.00001"]);
        // いっぱいなら新しいものは積めないが、あるものは積み直して上に移せる
        assert!(
            !db::put_read_later(&pool, "1111.00003", "t", &[], 2)
                .await
                .unwrap()
        );
        assert!(
            db::put_read_later(&pool, "1111.00001", "t", &[], 2)
                .await
                .unwrap()
        );
        assert_eq!(
            ids(db::list_read_later(&pool).await.unwrap()),
            ["1111.00001", "1111.00002"]
        );
        assert!(db::delete_read_later(&pool, "1111.00001").await.unwrap());
        assert!(!db::delete_read_later(&pool, "1111.00001").await.unwrap());
        assert_eq!(
            ids(db::list_read_later(&pool).await.unwrap()),
            ["1111.00002"]
        );
    }
}
