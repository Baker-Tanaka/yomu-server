//! 単語帳（word_notes）: 辞書で引いた語の検索回数と、辞書を補う情報。
//!
//! - `GET /api/dict` で引くたびに検索回数を数える（見出し語があればその語、無ければ検索語）
//! - `POST /api/dict/ai` … 辞書に無い語の訳と、カタカナの読みをローカル LLM に推定させて保存する
//! - `GET /api/words` / `POST /api/words` / `POST /api/words/delete` / `POST /api/words/prune` … 一覧・登録・削除
//! - 語数が上限（WORD_STORE_MAX）を超えたら、検索回数の少ない語（同じなら古い語）から消す。
//!   自分で登録した語は自動では消さない

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use paper_reader_shared::{
    DictAiRequest, DictResponse, WORD_PAGE_SIZE, WordDelete, WordDeleteResult, WordListQuery,
    WordListResponse, WordNote, WordPrune, WordSort, WordSource, WordStoreStats, WordUpsert,
    clip_context, is_kana_reading, normalize_whitespace, split_user_meanings,
};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::AppState;
use crate::dict::{self, normalize_query};
use crate::error::{AppError, AppResult};
use crate::llm::{JsonChat, JsonRequest, extract_json_object};
use crate::prompts;

/// 訳 1 つの最大文字数
const MAX_MEANING_CHARS: usize = 80;
/// 保存する訳の最大数
const MAX_MEANINGS: usize = 6;
/// 読みの最大文字数
const MAX_KANA_CHARS: usize = 40;
/// LLM に渡す文脈の最大文字数（訳を文脈に合わせるためだけなので短くする）
const MAX_AI_CONTEXT_CHARS: usize = 600;
/// LLM の順番待ちの上限（長い解説の生成中なら諦める）
const AI_QUEUE_TIMEOUT: Duration = Duration::from_secs(90);
const AI_TIMEOUT: Duration = Duration::from_secs(45);
const AI_MAX_TOKENS: u32 = 200;
/// 一度に削除できる語数
const MAX_DELETE: usize = 1_000;

/// 自分で登録した情報が無い行（自動で消してよい行）の条件
macro_rules! not_user {
    () => {
        "COALESCE(kana_source, '') <> 'user' AND COALESCE(meanings_source, '') <> 'user'"
    };
}

/// 単語帳の行と、辞書の訳（一覧の表示用）
macro_rules! note_select {
    () => {
        "SELECT n.word, n.kana, n.kana_source, n.meanings, n.meanings_source,
           n.lookup_count, n.last_looked_at, n.ai_checked_at, d.meaning
         FROM word_notes n LEFT JOIN dict d ON d.word = n.word "
    };
}

// ───────────── DB ─────────────

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

type NoteRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    i64,
    Option<i64>,
    Option<String>,
);

fn note_from_row(row: NoteRow) -> WordNote {
    let (
        word,
        kana,
        kana_source,
        meanings,
        meanings_source,
        lookup_count,
        last_looked_at,
        ai_checked_at,
        dict_meaning,
    ) = row;
    WordNote {
        word,
        kana,
        kana_source: kana_source.as_deref().and_then(WordSource::parse),
        meanings: meanings
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or_default(),
        meanings_source: meanings_source.as_deref().and_then(WordSource::parse),
        lookup_count,
        last_looked_at,
        ai_checked: ai_checked_at.is_some(),
        dict_summary: dict_meaning
            .as_deref()
            .and_then(|m| dict::split_meanings(m).into_iter().next()),
    }
}

pub async fn get_note(pool: &SqlitePool, word: &str) -> sqlx::Result<Option<WordNote>> {
    let row: Option<NoteRow> = sqlx::query_as(concat!(note_select!(), "WHERE n.word = ?"))
        .bind(word)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(note_from_row))
}

pub async fn stats(pool: &SqlitePool, max: i64) -> sqlx::Result<WordStoreStats> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM word_notes")
        .fetch_one(pool)
        .await?;
    Ok(WordStoreStats { count, max })
}

/// 引いたことを記録する（検索回数を 1 増やす）。上限を超えたら少ない語から消す。
pub async fn record_lookup(pool: &SqlitePool, word: &str, max: i64) -> sqlx::Result<()> {
    let now = now_secs();
    sqlx::query(
        "INSERT INTO word_notes (word, lookup_count, created_at, last_looked_at) VALUES (?, 1, ?, ?)
         ON CONFLICT(word) DO UPDATE SET lookup_count = word_notes.lookup_count + 1,
           last_looked_at = excluded.last_looked_at",
    )
    .bind(word)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    evict_over(pool, max, word).await
}

/// 上限を超えた分を、検索回数の少ない順（同じなら古い順）に消す。`keep` の語と自分で登録した語は残す。
async fn evict_over(pool: &SqlitePool, max: i64, keep: &str) -> sqlx::Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM word_notes")
        .fetch_one(pool)
        .await?;
    let over = count - max;
    if over <= 0 {
        return Ok(());
    }
    let r = sqlx::query(concat!(
        "DELETE FROM word_notes WHERE word IN (
           SELECT word FROM word_notes WHERE word <> ? AND ",
        not_user!(),
        " ORDER BY lookup_count ASC, last_looked_at ASC LIMIT ?)"
    ))
    .bind(keep)
    .bind(over)
    .execute(pool)
    .await?;
    tracing::info!(
        deleted = r.rows_affected(),
        max,
        "word store is full; evicted"
    );
    Ok(())
}

/// LLM の推定を保存する。自分で登録した読み・訳は上書きしない。`meanings` が None なら訳は変えない。
async fn save_ai(
    pool: &SqlitePool,
    word: &str,
    kana: Option<&str>,
    meanings: Option<&[String]>,
    max: i64,
) -> sqlx::Result<()> {
    let now = now_secs();
    let meanings_json = meanings
        .filter(|m| !m.is_empty())
        .and_then(|m| serde_json::to_string(m).ok());
    sqlx::query(
        "INSERT INTO word_notes (word, kana, kana_source, meanings, meanings_source, lookup_count,
           created_at, last_looked_at, ai_checked_at)
         VALUES (?1, ?2, CASE WHEN ?2 IS NULL THEN NULL ELSE 'llm' END,
                 ?3, CASE WHEN ?3 IS NULL THEN NULL ELSE 'llm' END, 0, ?4, ?4, ?4)
         ON CONFLICT(word) DO UPDATE SET
           kana = CASE WHEN ?2 IS NOT NULL AND COALESCE(word_notes.kana_source, '') <> 'user'
                       THEN ?2 ELSE word_notes.kana END,
           kana_source = CASE WHEN ?2 IS NOT NULL AND COALESCE(word_notes.kana_source, '') <> 'user'
                       THEN 'llm' ELSE word_notes.kana_source END,
           meanings = CASE WHEN ?3 IS NOT NULL AND COALESCE(word_notes.meanings_source, '') <> 'user'
                       THEN ?3 ELSE word_notes.meanings END,
           meanings_source = CASE WHEN ?3 IS NOT NULL AND COALESCE(word_notes.meanings_source, '') <> 'user'
                       THEN 'llm' ELSE word_notes.meanings_source END,
           ai_checked_at = ?4",
    )
    .bind(word)
    .bind(kana)
    .bind(meanings_json)
    .bind(now)
    .execute(pool)
    .await?;
    evict_over(pool, max, word).await
}

/// 自分で登録・編集する。None の項目は変えない。
async fn save_user(
    pool: &SqlitePool,
    word: &str,
    kana: Option<&str>,
    meanings: Option<&[String]>,
    max: i64,
) -> sqlx::Result<()> {
    let now = now_secs();
    let meanings_json = meanings.and_then(|m| serde_json::to_string(m).ok());
    sqlx::query(
        "INSERT INTO word_notes (word, kana, kana_source, meanings, meanings_source, lookup_count,
           created_at, last_looked_at)
         VALUES (?1, ?2, CASE WHEN ?2 IS NULL THEN NULL ELSE 'user' END,
                 ?3, CASE WHEN ?3 IS NULL THEN NULL ELSE 'user' END, 0, ?4, ?4)
         ON CONFLICT(word) DO UPDATE SET
           kana = COALESCE(?2, word_notes.kana),
           kana_source = CASE WHEN ?2 IS NULL THEN word_notes.kana_source ELSE 'user' END,
           meanings = COALESCE(?3, word_notes.meanings),
           meanings_source = CASE WHEN ?3 IS NULL THEN word_notes.meanings_source ELSE 'user' END",
    )
    .bind(word)
    .bind(kana)
    .bind(meanings_json)
    .bind(now)
    .execute(pool)
    .await?;
    evict_over(pool, max, word).await
}

/// LIKE の前方一致のパターン（% _ \ をエスケープする）。
pub fn like_prefix(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 1);
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

async fn list(
    pool: &SqlitePool,
    q: &str,
    sort: WordSort,
    offset: i64,
) -> sqlx::Result<(Vec<WordNote>, i64)> {
    macro_rules! page {
        ($order:literal) => {
            concat!(
                note_select!(),
                "WHERE n.word LIKE ?1 ESCAPE '\\' ORDER BY ",
                $order,
                " LIMIT ?2 OFFSET ?3"
            )
        };
    }
    let sql = match sort {
        WordSort::Count => page!("n.lookup_count DESC, n.last_looked_at DESC"),
        WordSort::Recent => page!("n.last_looked_at DESC"),
        WordSort::Alpha => page!("n.word ASC"),
    };
    let pattern = like_prefix(q);
    let rows: Vec<NoteRow> = sqlx::query_as(sql)
        .bind(&pattern)
        .bind(WORD_PAGE_SIZE)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM word_notes WHERE word LIKE ? ESCAPE '\\'")
            .bind(&pattern)
            .fetch_one(pool)
            .await?;
    Ok((rows.into_iter().map(note_from_row).collect(), total))
}

async fn delete_words(pool: &SqlitePool, words: &[String]) -> sqlx::Result<i64> {
    let mut tx = pool.begin().await?;
    let mut deleted = 0;
    for w in words {
        deleted += sqlx::query("DELETE FROM word_notes WHERE word = ?")
            .bind(w)
            .execute(&mut *tx)
            .await?
            .rows_affected() as i64;
    }
    tx.commit().await?;
    Ok(deleted)
}

async fn prune(pool: &SqlitePool, count: i64) -> sqlx::Result<i64> {
    let r = sqlx::query(concat!(
        "DELETE FROM word_notes WHERE word IN (SELECT word FROM word_notes WHERE ",
        not_user!(),
        " ORDER BY lookup_count ASC, last_looked_at ASC LIMIT ?)"
    ))
    .bind(count)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() as i64)
}

// ───────────── LLM の応答 ─────────────

/// LLM が返した読みと訳（検証済み）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AiNote {
    pub kana: Option<String>,
    pub meanings: Vec<String>,
}

pub fn word_note_schema() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "word_note",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "kana": { "type": "string" },
                    "meanings": { "type": "array", "items": { "type": "string" }, "maxItems": 4 }
                },
                "required": ["kana", "meanings"],
                "additionalProperties": false
            }
        }
    })
}

/// LLM の応答（JSON 文字列）を検証する。読みはカタカナだけのときに使い、訳は短く整える。
pub fn parse_ai_note(content: &str) -> AiNote {
    let Some(v) = extract_json_object(content).and_then(|j| serde_json::from_str::<Value>(j).ok())
    else {
        return AiNote::default();
    };
    let kana = v
        .get("kana")
        .and_then(Value::as_str)
        .map(|k| normalize_whitespace(k).replace(' ', ""))
        .filter(|k| is_kana_reading(k) && k.chars().count() <= MAX_KANA_CHARS);
    let mut meanings: Vec<String> = Vec::new();
    for m in v
        .get("meanings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let m = normalize_whitespace(m);
        if m.is_empty() || m.chars().count() > MAX_MEANING_CHARS || meanings.contains(&m) {
            continue;
        }
        meanings.push(m);
        if meanings.len() >= 4 {
            break;
        }
    }
    AiNote { kana, meanings }
}

// ───────────── ハンドラ ─────────────

/// 辞書の結果に単語帳の情報と使用量を付ける。
pub async fn attach(state: &AppState, resp: &mut DictResponse) -> sqlx::Result<()> {
    let word = resp.note_word().to_string();
    resp.note = get_note(&state.db, &word).await?;
    resp.store = Some(stats(&state.db, state.config.word_store_max).await?);
    Ok(())
}

pub async fn post_dict_ai(
    State(state): State<AppState>,
    body: Result<Json<DictAiRequest>, JsonRejection>,
) -> AppResult<Json<DictResponse>> {
    let Json(req) =
        body.map_err(|e| AppError::BadRequest(format!("リクエストを解釈できません: {e}")))?;
    let query = normalize_query(&req.word)
        .filter(|w| w.chars().any(|c| c.is_ascii_alphabetic()))
        .ok_or_else(|| AppError::BadRequest("AI で訳せる単語ではありません".into()))?;
    let context = clip_context(
        &normalize_whitespace(&req.context),
        &req.word,
        MAX_AI_CONTEXT_CHARS,
    );

    let mut resp = dict::lookup_word(&state.db, query).await?;
    attach(&state, &mut resp).await?;
    let need_meanings = resp.needs_ai_meanings();
    let need_kana = resp.note.as_ref().is_none_or(|n| n.kana.is_none());
    if !need_meanings && !need_kana {
        return Ok(Json(resp));
    }
    if state.config.llm_model.is_empty() {
        return Err(AppError::ServiceUnavailable(
            "LLM_MODEL が設定されていません".into(),
        ));
    }
    let word = resp.note_word().to_string();

    // 解説と同じく LLM への同時リクエストは 1 件（切断されたらこのハンドラごと止まる）
    let _permit = tokio::time::timeout(AI_QUEUE_TIMEOUT, state.llm_gate.clone().acquire_owned())
        .await
        .map_err(|_| {
            AppError::ServiceUnavailable(
                "LLM が混み合っています。しばらくしてからもう一度お試しください".into(),
            )
        })?
        .map_err(|_| AppError::ServiceUnavailable("LLM を使えません".into()))?;

    let chat = JsonChat {
        http: &state.llm_stream_http,
        base_url: &state.config.llm_base_url,
        model: &state.config.llm_model,
        disable_thinking: state.config.llm_disable_thinking,
        structured_output: &state.structured_output,
    };
    let system = prompts::word_note_system_prompt(need_meanings);
    let user = prompts::word_note_user_message(&word, &context, need_meanings);
    let content = chat
        .complete(&JsonRequest {
            system: &system,
            user: &user,
            response_format: word_note_schema(),
            max_tokens: AI_MAX_TOKENS,
            timeout: AI_TIMEOUT,
        })
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "word note failed");
            AppError::Upstream(
                "AI に問い合わせできませんでした（LLM の状態を確認してください）".into(),
            )
        })?;
    let note = parse_ai_note(&content);
    tracing::info!(
        kana = note.kana.is_some(),
        meanings = note.meanings.len(),
        "word note generated"
    );
    save_ai(
        &state.db,
        &word,
        note.kana.as_deref(),
        need_meanings.then_some(note.meanings.as_slice()),
        state.config.word_store_max,
    )
    .await?;
    attach(&state, &mut resp).await?;
    Ok(Json(resp))
}

pub async fn get_words(
    State(state): State<AppState>,
    query: Result<Query<WordListQuery>, QueryRejection>,
) -> AppResult<Json<WordListResponse>> {
    let Query(q) = query.map_err(|e| AppError::BadRequest(format!("クエリが不正です: {e}")))?;
    let prefix: String = q.q.trim().to_lowercase().chars().take(64).collect();
    let (items, total) = list(&state.db, &prefix, q.sort, q.offset.max(0)).await?;
    Ok(Json(WordListResponse {
        items,
        total,
        store: stats(&state.db, state.config.word_store_max).await?,
    }))
}

/// 自分で登録した内容を検証する。読み・訳が空なら None（その項目は変えない）。
pub fn validate_upsert(u: &WordUpsert) -> AppResult<(String, Option<String>, Option<Vec<String>>)> {
    let word = normalize_query(&u.word)
        .filter(|w| w.chars().any(|c| c.is_ascii_alphabetic()))
        .ok_or_else(|| AppError::BadRequest("英単語を入力してください".into()))?;
    let kana = normalize_whitespace(&u.kana);
    let kana = if kana.is_empty() {
        None
    } else if is_kana_reading(&kana) && kana.chars().count() <= MAX_KANA_CHARS {
        Some(kana)
    } else {
        return Err(AppError::BadRequest(format!(
            "読みはカタカナで入力してください（{MAX_KANA_CHARS} 文字まで）"
        )));
    };
    let meanings = split_user_meanings(&u.meanings);
    if meanings.len() > MAX_MEANINGS {
        return Err(AppError::BadRequest(format!(
            "訳は {MAX_MEANINGS} 個までにしてください"
        )));
    }
    if meanings
        .iter()
        .any(|m| m.chars().count() > MAX_MEANING_CHARS)
    {
        return Err(AppError::BadRequest(format!(
            "訳は 1 つ {MAX_MEANING_CHARS} 文字までにしてください"
        )));
    }
    let meanings = (!meanings.is_empty()).then_some(meanings);
    if kana.is_none() && meanings.is_none() {
        return Err(AppError::BadRequest("読みか訳を入力してください".into()));
    }
    Ok((word, kana, meanings))
}

pub async fn post_word(
    State(state): State<AppState>,
    body: Result<Json<WordUpsert>, JsonRejection>,
) -> AppResult<Json<WordNote>> {
    let Json(u) =
        body.map_err(|e| AppError::BadRequest(format!("リクエストを解釈できません: {e}")))?;
    let (word, kana, meanings) = validate_upsert(&u)?;
    save_user(
        &state.db,
        &word,
        kana.as_deref(),
        meanings.as_deref(),
        state.config.word_store_max,
    )
    .await?;
    get_note(&state.db, &word)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("保存した語が見つかりません")))
}

pub async fn post_delete(
    State(state): State<AppState>,
    body: Result<Json<WordDelete>, JsonRejection>,
) -> AppResult<Json<WordDeleteResult>> {
    let Json(d) =
        body.map_err(|e| AppError::BadRequest(format!("リクエストを解釈できません: {e}")))?;
    if d.words.len() > MAX_DELETE {
        return Err(AppError::BadRequest(format!(
            "一度に削除できるのは {MAX_DELETE} 語までです"
        )));
    }
    let deleted = delete_words(&state.db, &d.words).await?;
    Ok(Json(WordDeleteResult {
        deleted,
        store: stats(&state.db, state.config.word_store_max).await?,
    }))
}

pub async fn post_prune(
    State(state): State<AppState>,
    body: Result<Json<WordPrune>, JsonRejection>,
) -> AppResult<Json<WordDeleteResult>> {
    let Json(p) =
        body.map_err(|e| AppError::BadRequest(format!("リクエストを解釈できません: {e}")))?;
    if p.count <= 0 {
        return Err(AppError::BadRequest(
            "削除する語数を指定してください".into(),
        ));
    }
    let deleted = prune(&state.db, p.count).await?;
    tracing::info!(deleted, "word store pruned");
    Ok(Json(WordDeleteResult {
        deleted,
        store: stats(&state.db, state.config.word_store_max).await?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[test]
    fn ai_note_parsing() {
        let n = parse_ai_note(
            r#"```json
{"kana": "モダリティ", "meanings": ["（名）様式", " 様相 ", "", "（名）様式"]}
```"#,
        );
        assert_eq!(n.kana.as_deref(), Some("モダリティ"));
        assert_eq!(n.meanings, vec!["（名）様式", "様相"]);
        // カタカナでない読みは捨てる
        let n = parse_ai_note(r#"{"kana": "modality", "meanings": []}"#);
        assert_eq!(n, AiNote::default());
        let n = parse_ai_note(r#"{"kana": "ステート オブ ジ アート", "meanings": []}"#);
        assert_eq!(n.kana.as_deref(), Some("ステートオブジアート"));
        let long = "あ".repeat(MAX_MEANING_CHARS + 1);
        let n = parse_ai_note(&format!(
            r#"{{"kana": "", "meanings": ["{long}", "a", "b", "c", "d", "e"]}}"#
        ));
        assert_eq!(n.kana, None);
        assert_eq!(n.meanings, vec!["a", "b", "c", "d"]);
        assert_eq!(parse_ai_note("broken"), AiNote::default());
    }

    #[test]
    fn like_escape() {
        assert_eq!(like_prefix("ab"), "ab%");
        assert_eq!(like_prefix("a%_\\"), "a\\%\\_\\\\%");
    }

    #[test]
    fn upsert_validation() {
        let u = |word: &str, kana: &str, meanings: &str| WordUpsert {
            word: word.into(),
            kana: kana.into(),
            meanings: meanings.into(),
        };
        let (w, k, m) = validate_upsert(&u(" Modality, ", "モダリティ", "様式、様相")).unwrap();
        assert_eq!(w, "modality");
        assert_eq!(k.as_deref(), Some("モダリティ"));
        assert_eq!(m.unwrap(), vec!["様式", "様相"]);
        let (_, k, m) = validate_upsert(&u("x-ray", "", "エックス線")).unwrap();
        assert!(k.is_none() && m.is_some());
        assert!(validate_upsert(&u("modality", "", "")).is_err());
        assert!(validate_upsert(&u("modality", "もだりてぃ", "")).is_err());
        assert!(validate_upsert(&u("123", "", "数")).is_err());
        assert!(validate_upsert(&u("a", "", "1/2/3/4/5/6/7")).is_err());
    }

    #[tokio::test]
    async fn lookup_counts_and_eviction() {
        let pool = pool().await;
        for _ in 0..3 {
            record_lookup(&pool, "model", 3).await.unwrap();
        }
        record_lookup(&pool, "paper", 3).await.unwrap();
        record_lookup(&pool, "paper", 3).await.unwrap();
        save_user(&pool, "mine", Some("マイン"), None, 3)
            .await
            .unwrap();
        let n = get_note(&pool, "model").await.unwrap().unwrap();
        assert_eq!(n.lookup_count, 3);
        assert!(!n.ai_checked && n.kana.is_none());

        // 4 語目で上限 3 を超える → 自分で登録した語と今引いた語は残し、少ない語から消す
        record_lookup(&pool, "new", 3).await.unwrap();
        assert_eq!(stats(&pool, 3).await.unwrap().count, 3);
        assert!(get_note(&pool, "paper").await.unwrap().is_none());
        assert!(get_note(&pool, "mine").await.unwrap().is_some());
        assert!(get_note(&pool, "new").await.unwrap().is_some());

        // 手動の整理も自分で登録した語は消さない
        assert_eq!(prune(&pool, 10).await.unwrap(), 2);
        assert_eq!(stats(&pool, 3).await.unwrap().count, 1);
        assert_eq!(
            delete_words(&pool, &["mine".into(), "none".into()])
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn ai_does_not_overwrite_user() {
        let pool = pool().await;
        save_user(&pool, "modality", None, Some(&["様式".into()]), 100)
            .await
            .unwrap();
        save_ai(
            &pool,
            "modality",
            Some("モダリティ"),
            Some(&["（名）様相".into()]),
            100,
        )
        .await
        .unwrap();
        let n = get_note(&pool, "modality").await.unwrap().unwrap();
        assert_eq!(n.meanings, vec!["様式"]);
        assert_eq!(n.meanings_source, Some(WordSource::User));
        assert_eq!(n.kana.as_deref(), Some("モダリティ"));
        assert_eq!(n.kana_source, Some(WordSource::Llm));
        assert!(n.ai_checked && n.is_user());

        // 訳を頼まなかったとき（None）は既存の訳を変えない・空の訳は保存しない
        save_ai(&pool, "relevant", Some("レレバント"), None, 100)
            .await
            .unwrap();
        save_ai(&pool, "relevant", None, Some(&[]), 100)
            .await
            .unwrap();
        let n = get_note(&pool, "relevant").await.unwrap().unwrap();
        assert_eq!(n.kana.as_deref(), Some("レレバント"));
        assert!(n.meanings.is_empty() && n.meanings_source.is_none());
        assert_eq!(n.lookup_count, 0);

        let (items, total) = list(&pool, "mod", WordSort::Alpha, 0).await.unwrap();
        assert_eq!((items.len(), total), (1, 1));
        let (_, total) = list(&pool, "%", WordSort::Count, 0).await.unwrap();
        assert_eq!(total, 0, "% はワイルドカードにしない");
    }
}
