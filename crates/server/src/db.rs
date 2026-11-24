//! SQLite の接続とキャッシュ操作。

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use paper_reader_shared::{HistoryRecord, Mode, PaperResponse};
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

pub async fn get_explanation(pool: &SqlitePool, key: &str) -> sqlx::Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT markdown FROM explain_cache WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(m,)| m))
}

pub struct NewExplanation<'a> {
    pub key: &'a str,
    pub model: &'a str,
    pub mode: Mode,
    pub text: &'a str,
    pub markdown: &'a str,
    pub prompt_version: u32,
}

pub async fn put_explanation(pool: &SqlitePool, e: &NewExplanation<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO explain_cache (key, model, mode, text, markdown, prompt_version, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET markdown = excluded.markdown, created_at = excluded.created_at",
    )
    .bind(e.key)
    .bind(e.model)
    .bind(e.mode.as_str())
    .bind(e.text)
    .bind(e.markdown)
    .bind(e.prompt_version as i64)
    .bind(now_secs())
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
pub async fn put_history(pool: &SqlitePool, r: &HistoryRecord) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO history (paper_id, paper_title, mode, text, context, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(paper_id, mode, text) DO UPDATE SET paper_title = excluded.paper_title,
           context = excluded.context, updated_at = excluded.updated_at",
    )
    .bind(&r.paper_id)
    .bind(&r.paper_title)
    .bind(r.mode.as_str())
    .bind(&r.text)
    .bind(&r.context)
    .bind(now_secs())
    .execute(pool)
    .await?;
    Ok(())
}

/// 履歴の行（has_explanation は呼び出し側で埋める）。
pub type HistoryRow = (i64, String, String, String, String, String, i64);

pub async fn list_history(
    pool: &SqlitePool,
    paper: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<HistoryRow>> {
    sqlx::query_as(
        "SELECT id, paper_id, paper_title, mode, text, context, updated_at FROM history
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
