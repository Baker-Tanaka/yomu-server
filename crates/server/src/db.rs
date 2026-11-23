//! SQLite の接続とキャッシュ操作。

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use paper_reader_shared::PaperResponse;
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
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
