//! 論文の付箋（どこまで読んだか）: `PUT /api/bookmark` と `DELETE /api/bookmark?paper_id=`。
//!
//! 1 論文に 1 枚。版違い（`2509.19480v1` と `v2`）でも同じ論文として扱うため、版なしの ID で保存する。
//! 付箋は `GET /api/paper` の応答にも含めるので、論文を開くときに追加の通信は要らない。

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use paper_reader_shared::{
    Bookmark, BookmarkPut, BookmarkQuery, MAX_BOOKMARK_SNIPPET_CHARS, normalize_whitespace,
};

use crate::AppState;
use crate::arxiv;
use crate::db;
use crate::error::{AppError, AppResult};

/// 本文のブロック要素の番号の上限（大きな論文でも数千程度）
const MAX_BLOCK: u32 = 1_000_000;
/// ブロックの中の文字の位置の上限（長い段落でも数万程度）
const MAX_OFFSET: u32 = 1_000_000;

pub async fn put_bookmark(
    State(state): State<AppState>,
    body: Result<Json<BookmarkPut>, JsonRejection>,
) -> AppResult<Json<Bookmark>> {
    let Json(req) =
        body.map_err(|e| AppError::BadRequest(format!("リクエストを解釈できません: {e}")))?;
    let base = valid_base_id(&req.paper_id)?;
    if req.block > MAX_BLOCK || req.offset > MAX_OFFSET {
        return Err(AppError::BadRequest("付箋の位置が不正です".into()));
    }
    let snippet = clean_snippet(&req.snippet);
    let bookmark = db::put_bookmark(&state.db, base, req.block, req.offset, &snippet)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(bookmark))
}

pub async fn delete_bookmark(
    State(state): State<AppState>,
    query: Result<Query<BookmarkQuery>, QueryRejection>,
) -> AppResult<StatusCode> {
    let Query(q) = query.map_err(|_| AppError::BadRequest("paper_id を指定してください".into()))?;
    let base = valid_base_id(&q.paper_id)?;
    db::delete_bookmark(&state.db, base)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    // はがす前から無くても、結果は同じなので成功とする
    Ok(StatusCode::NO_CONTENT)
}

/// 論文 ID を検証し、版なしの ID を返す。
pub fn valid_base_id(id: &str) -> AppResult<&str> {
    let id = id.trim();
    if crate::papers::is_valid_paper_id(id) {
        Ok(arxiv::base_id(id))
    } else {
        Err(AppError::BadRequest("論文 ID が不正です".into()))
    }
}

/// 書き出しの空白をまとめ、長さを抑える。
pub fn clean_snippet(s: &str) -> String {
    normalize_whitespace(s)
        .chars()
        .take(MAX_BOOKMARK_SNIPPET_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_ids() {
        assert_eq!(valid_base_id("2509.19480v2").ok(), Some("2509.19480"));
        assert_eq!(valid_base_id(" 2509.19480 ").ok(), Some("2509.19480"));
        assert_eq!(
            valid_base_id("hep-th/9901001v1").ok(),
            Some("hep-th/9901001")
        );
        assert!(valid_base_id("").is_err());
        assert!(valid_base_id("../etc/passwd%").is_err());
        assert!(valid_base_id("a b").is_err());
    }

    #[tokio::test]
    async fn stores_the_line_position() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        for offset in [120, 480] {
            db::put_bookmark(&pool, "2509.19480", 3, offset, "We propose")
                .await
                .unwrap();
            let b = db::get_bookmark(&pool, "2509.19480")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (b.block, b.offset, b.snippet.as_str()),
                (3, offset, "We propose")
            );
        }
    }

    #[test]
    fn snippets() {
        assert_eq!(
            clean_snippet("  We  propose\n a model "),
            "We propose a model"
        );
        let long = "あ".repeat(200);
        assert_eq!(
            clean_snippet(&long).chars().count(),
            MAX_BOOKMARK_SNIPPET_CHARS
        );
    }
}
