//! SQLite の接続とキャッシュ操作。

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use paper_reader_shared::{
    Bookmark, HistoryRecord, Mode, PaperCacheStats, PaperResponse, RecentPaper,
};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

pub async fn connect(database_url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("DATABASE_URL を解釈できません: {database_url}"))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    if let Some(parent) = options.get_filename().parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("{} を作成できません", parent.display()))?;
    }
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .context("SQLite に接続できません")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("マイグレーションに失敗しました")?;
    Ok(pool)
}

pub async fn get_paper(pool: &SqlitePool, id: &str) -> sqlx::Result<Option<PaperResponse>> {
    let row: Option<(String, String, String, String)> =
        sqlx::query_as("SELECT id, title, html, source_url FROM paper_cache WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(id, title, html, source_url)| PaperResponse {
        id,
        title,
        html,
        source_url,
        bookmark: None,
    }))
}

pub async fn put_paper(pool: &SqlitePool, paper: &PaperResponse) -> sqlx::Result<()> {
    let now = now_secs();
    sqlx::query(
        "INSERT INTO paper_cache (id, title, html, source_url, fetched_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET title = excluded.title, html = excluded.html,
           source_url = excluded.source_url, fetched_at = excluded.fetched_at",
    )
    .bind(&paper.id)
    .bind(&paper.title)
    .bind(&paper.html)
    .bind(&paper.source_url)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// キャッシュした解説（Markdown と、英文なら検出した文法タグの JSON）。
pub async fn get_explanation(
    pool: &SqlitePool,
    key: &str,
) -> sqlx::Result<Option<(String, Option<String>)>> {
    sqlx::query_as("SELECT markdown, tags FROM explain_cache WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
}

pub struct NewExplanation<'a> {
    pub key: &'a str,
    pub model: &'a str,
    pub mode: Mode,
    pub text: &'a str,
    pub markdown: &'a str,
    pub prompt_version: u32,
    /// 検出した文法タグの JSON（英文のみ）
    pub tags: Option<&'a str>,
}

pub async fn put_explanation(pool: &SqlitePool, e: &NewExplanation<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO explain_cache (key, model, mode, text, markdown, prompt_version, created_at, tags)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET markdown = excluded.markdown, created_at = excluded.created_at,
           tags = excluded.tags",
    )
    .bind(e.key)
    .bind(e.model)
    .bind(e.mode.as_str())
    .bind(e.text)
    .bind(e.markdown)
    .bind(e.prompt_version as i64)
    .bind(now_secs())
    .bind(e.tags)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn explanation_exists(pool: &SqlitePool, key: &str) -> sqlx::Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM explain_cache WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

/// 履歴を追加する。同じ論文・mode・text があれば文脈と時刻を更新して先頭に上げる。
/// 文法タグは空でなければ置き換え、空なら既存の値を残す。
pub async fn put_history(pool: &SqlitePool, r: &HistoryRecord) -> sqlx::Result<()> {
    let tags = serde_json::to_string(&r.tags).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        "INSERT INTO history (paper_id, paper_title, mode, text, context, updated_at, tags)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(paper_id, mode, text) DO UPDATE SET paper_title = excluded.paper_title,
           context = excluded.context, updated_at = excluded.updated_at,
           tags = CASE WHEN excluded.tags = '[]' THEN history.tags ELSE excluded.tags END",
    )
    .bind(&r.paper_id)
    .bind(&r.paper_title)
    .bind(r.mode.as_str())
    .bind(&r.text)
    .bind(&r.context)
    .bind(now_secs())
    .bind(tags)
    .execute(pool)
    .await?;
    Ok(())
}

/// 履歴の行（has_explanation は呼び出し側で埋める）。最後は文法タグの JSON。
pub type HistoryRow = (i64, String, String, String, String, String, i64, String);

pub async fn list_history(
    pool: &SqlitePool,
    paper: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<HistoryRow>> {
    sqlx::query_as(
        "SELECT id, paper_id, paper_title, mode, text, context, updated_at, tags FROM history
         WHERE ?1 IS NULL OR paper_id = ?1
         ORDER BY updated_at DESC, id DESC LIMIT ?2",
    )
    .bind(paper)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// 履歴を 1 件削除する。消えたら true。
pub async fn delete_history(pool: &SqlitePool, id: i64) -> sqlx::Result<bool> {
    let r = sqlx::query("DELETE FROM history WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

/// 文法タグごとの出現回数（そのタグを含む履歴の英文の数）。
pub async fn tag_counts(pool: &SqlitePool) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as(
        "SELECT j.value, COUNT(*) FROM history, json_each(history.tags) AS j
         WHERE json_valid(history.tags) GROUP BY j.value",
    )
    .fetch_all(pool)
    .await
}

/// 論文を開いたことを記録する（見た論文の一覧）。
pub async fn record_visit(pool: &SqlitePool, paper: &PaperResponse) -> sqlx::Result<()> {
    let now = now_secs();
    sqlx::query(
        "INSERT INTO paper_visits (paper_id, title, source_url, first_opened_at, last_opened_at, open_count)
         VALUES (?, ?, ?, ?, ?, 1)
         ON CONFLICT(paper_id) DO UPDATE SET title = excluded.title, source_url = excluded.source_url,
           last_opened_at = excluded.last_opened_at, open_count = paper_visits.open_count + 1",
    )
    .bind(&paper.id)
    .bind(&paper.title)
    .bind(&paper.source_url)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

type VisitRow = (String, String, String, i64, i64, i64, i64, bool);

/// 見た論文（最後に開いた順）。履歴の単語・英文の数と、本文がキャッシュにあるかも付ける。
pub async fn list_visits(pool: &SqlitePool, limit: i64) -> sqlx::Result<Vec<RecentPaper>> {
    let rows: Vec<VisitRow> = sqlx::query_as(
        "SELECT v.paper_id, v.title, v.source_url, v.last_opened_at, v.open_count,
           (SELECT COUNT(*) FROM history h WHERE h.paper_id = v.paper_id AND h.mode = 'word'),
           (SELECT COUNT(*) FROM history h WHERE h.paper_id = v.paper_id AND h.mode = 'sentence'),
           EXISTS (SELECT 1 FROM paper_cache c WHERE c.id = v.paper_id)
         FROM paper_visits v ORDER BY v.last_opened_at DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                title,
                source_url,
                last_opened_at,
                open_count,
                word_count,
                sentence_count,
                cached,
            )| {
                RecentPaper {
                    id,
                    title,
                    source_url,
                    last_opened_at,
                    open_count,
                    word_count,
                    sentence_count,
                    cached,
                    bookmarked: false,
                }
            },
        )
        .collect())
}

/// 見た論文から外す（版違い `<id>v1` なども含めて）。`purge` なら本文と図のキャッシュも消す
/// （単語・英文の履歴は残す）。消えたら true。
pub async fn delete_visit(pool: &SqlitePool, id: &str, purge: bool) -> sqlx::Result<bool> {
    let base = crate::arxiv::base_id(id);
    // LIKE の特殊文字（% _）は論文 ID に含まれない（呼び出し側で検証済み）
    let versions = format!("{base}v%");
    let mut tx = pool.begin().await?;
    let removed = sqlx::query("DELETE FROM paper_visits WHERE paper_id = ? OR paper_id LIKE ?")
        .bind(base)
        .bind(&versions)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let mut purged = 0;
    if purge {
        purged += sqlx::query("DELETE FROM paper_cache WHERE id = ? OR id LIKE ?")
            .bind(base)
            .bind(&versions)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        sqlx::query("DELETE FROM paper_assets WHERE paper_id = ?")
            .bind(base)
            .execute(&mut *tx)
            .await?;
    }
    // 一覧から外した論文の付箋も消す（読み終えた・もう読まない論文とみなす）
    sqlx::query("DELETE FROM paper_bookmarks WHERE paper_id = ?")
        .bind(base)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(removed + purged > 0)
}

/// 論文の付箋（`base` は版なしの ID）。
pub async fn get_bookmark(pool: &SqlitePool, base: &str) -> sqlx::Result<Option<Bookmark>> {
    let row: Option<(i64, String, i64)> =
        sqlx::query_as("SELECT block, snippet, updated_at FROM paper_bookmarks WHERE paper_id = ?")
            .bind(base)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(block, snippet, updated_at)| Bookmark {
        block: u32::try_from(block).unwrap_or(0),
        snippet,
        updated_at,
    }))
}

/// 付箋を貼る（1 論文に 1 枚。貼り直したら置き換える）。
pub async fn put_bookmark(
    pool: &SqlitePool,
    base: &str,
    block: u32,
    snippet: &str,
) -> sqlx::Result<Bookmark> {
    let now = now_secs();
    sqlx::query(
        "INSERT INTO paper_bookmarks (paper_id, block, snippet, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(paper_id) DO UPDATE SET block = excluded.block, snippet = excluded.snippet,
           updated_at = excluded.updated_at",
    )
    .bind(base)
    .bind(i64::from(block))
    .bind(snippet)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(Bookmark {
        block,
        snippet: snippet.to_string(),
        updated_at: now,
    })
}

pub async fn delete_bookmark(pool: &SqlitePool, base: &str) -> sqlx::Result<bool> {
    let n = sqlx::query("DELETE FROM paper_bookmarks WHERE paper_id = ?")
        .bind(base)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

/// 付箋を貼っている論文（版なしの ID）。
pub async fn bookmarked_papers(pool: &SqlitePool) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar("SELECT paper_id FROM paper_bookmarks")
        .fetch_all(pool)
        .await
}

pub async fn cache_stats(pool: &SqlitePool) -> sqlx::Result<PaperCacheStats> {
    let (papers, html_bytes): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(LENGTH(html)), 0) FROM paper_cache")
            .fetch_one(pool)
            .await?;
    let (assets, asset_bytes): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(LENGTH(data)), 0) FROM paper_assets")
            .fetch_one(pool)
            .await?;
    Ok(PaperCacheStats {
        papers,
        html_bytes,
        assets,
        asset_bytes,
    })
}

/// キャッシュした図（Content-Type と中身）。
pub async fn get_asset(pool: &SqlitePool, url: &str) -> sqlx::Result<Option<(String, Vec<u8>)>> {
    sqlx::query_as("SELECT content_type, data FROM paper_assets WHERE url = ?")
        .bind(url)
        .fetch_optional(pool)
        .await
}

pub async fn put_asset(
    pool: &SqlitePool,
    url: &str,
    paper_id: &str,
    content_type: &str,
    data: &[u8],
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO paper_assets (url, paper_id, content_type, data, fetched_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(url) DO UPDATE SET content_type = excluded.content_type, data = excluded.data,
           fetched_at = excluded.fetched_at",
    )
    .bind(url)
    .bind(paper_id)
    .bind(content_type)
    .bind(data)
    .bind(now_secs())
    .execute(pool)
    .await?;
    Ok(())
}
