//! `POST /api/explain` — LLM の解説を SSE で中継する。
//!
//! - LLM への同時リクエストは `AppState::llm_gate`（Semaphore, 1 件）で制限し、待つときは `queued` を送る
//! - クライアントが切断したら（SSE の受け手が drop されたら）上流のストリームも drop して生成を止める
//! - 最後まで生成できた解説は explain_cache に保存し、次回は LLM を呼ばずに同じ SSE 形式で返す
//! - mode=sentence では、和訳を先に流してから文法タグ付けを行い `tags` イベントを送り、続けて残りの解説を流す
//!   （和訳・タグ付け・解説で Semaphore 1 件）。タグ付けに失敗してもタグ無しで解説を続ける

use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use paper_reader_shared::{
    ExplainRequest, MAX_CONTEXT_CHARS, MAX_TEXT_CHARS, Mode, SseParser, sse_event,
};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

use crate::AppState;
use crate::cache::cache_key;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::grammar::{DetectedTag, GrammarData};
use crate::prompts::{self, GrammarNote};
use crate::tagger::Tagger;

/// 1 回の解説の上限時間（順番待ちは含まない）
pub(crate) const GENERATION_TIMEOUT: Duration = Duration::from_secs(300);
/// 生成する最大トークン数
const MAX_TOKENS: u32 = 1_500;
/// 和訳の最大トークン数（英文は 2,000 文字まで）
const TRANSLATION_MAX_TOKENS: u32 = 1_000;
/// 和訳と、そのあとの解説のあいだ
const SECTION_BREAK: &str = "\n\n";

pub async fn post_explain(
    State(state): State<AppState>,
    body: Result<Json<ExplainRequest>, JsonRejection>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let Json(req) =
        body.map_err(|e| AppError::BadRequest(format!("リクエストを解釈できません: {e}")))?;
    let req = validate(req)?;
    let key = key_for(&state, &req);

    let (tx, rx) = mpsc::channel::<Event>(64);
    let cached = if req.regenerate {
        None
    } else {
        // 英文でスラッシュリーディングの有無だけが違う解説があれば、それを返す（LLM を待たせない）。
        // web は設定に合わせて表示を切り替え、足りなければ「付けて解説し直す」を出す
        let mut keys = vec![key.clone()];
        if req.mode == Mode::Sentence {
            keys.push(key_for(
                &state,
                &ExplainRequest {
                    slash: !req.slash,
                    ..req.clone()
                },
            ));
        }
        let mut hit = None;
        for k in &keys {
            match db::get_explanation(&state.db, k).await {
                Ok(Some(h)) => {
                    hit = Some(h);
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    // キャッシュが読めなくても生成はできる
                    tracing::warn!(error = %e, "explain cache read failed");
                    break;
                }
            }
        }
        hit
    };
    match cached {
        Some((markdown, tags)) => {
            tracing::info!(mode = req.mode.as_str(), "explain cache hit");
            let mut events = vec![(sse_event::CACHED, String::new())];
            if req.mode == Mode::Sentence {
                // 保存するのはタグの id と該当箇所だけなので、名前と説明を付け直す
                let detected: Vec<DetectedTag> = tags
                    .as_deref()
                    .and_then(|t| serde_json::from_str(t).ok())
                    .unwrap_or_default();
                events.push((sse_event::TAGS, tags_event(&state.grammar(), &detected)));
            }
            events.push((sse_event::TOKEN, markdown));
            events.push((sse_event::DONE, String::new()));
            // 4 件だけなのでチャネルの容量内に収まり、待たずに送れる
            for (event, data) in events {
                let _ = tx.try_send(Event::default().event(event).data(data));
            }
            drop(tx);
        }
        None => {
            tokio::spawn(run(state, req, key, tx));
        }
    }

    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| (Ok(ev), rx))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// プロンプトを温め直す間隔（Ollama が使わないモデルを外すまでの時間より短くする）
const WARM_UP_INTERVAL_SECS: i64 = 5 * 60;

/// 論文を開いたときに、解説で使うプロンプトを LLM に読み込ませておく（プロンプトのキャッシュを温める）。
///
/// 冷えた状態では、最初の英文の解説でタグ付けのプロンプトの読み込みだけに 15 秒ほどかかる。
/// LLM が空いているときだけ行い（Semaphore を待たない）、5 分に 1 回まで。
pub fn warm_up(state: &AppState) {
    if state.config.llm_model.is_empty() {
        return;
    }
    let now = now_secs();
    let last = state.warmed_at.load(Ordering::Relaxed);
    if now - last < WARM_UP_INTERVAL_SECS {
        return;
    }
    let Ok(permit) = state.llm_gate.clone().try_acquire_owned() else {
        return;
    };
    state.warmed_at.store(now, Ordering::Relaxed);
    let state = state.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let started = std::time::Instant::now();
        let cfg = &state.config;
        let grammar = state.grammar();
        Tagger {
            http: &state.llm_stream_http,
            base_url: &cfg.llm_base_url,
            model: &cfg.llm_model,
            disable_thinking: cfg.llm_disable_thinking,
            structured_output: &state.structured_output,
        }
        .warm_up(&grammar)
        .await;
        let url = format!(
            "{}/chat/completions",
            cfg.llm_base_url.trim_end_matches('/')
        );
        let sample = "We propose a simple method.";
        for system in [
            prompts::translation_system_prompt(),
            prompts::system_prompt(Mode::Sentence, true),
            prompts::system_prompt(Mode::Sentence, false),
            prompts::system_prompt(Mode::Word, false),
        ] {
            let mut body =
                chat_stream_body(&cfg.llm_model, &system, sample, 1, cfg.llm_disable_thinking);
            body["stream"] = json!(false);
            let sent = state
                .llm_stream_http
                .post(&url)
                .json(&body)
                .timeout(WARM_UP_TIMEOUT)
                .send()
                .await;
            if let Err(e) = sent.and_then(|r| r.error_for_status()) {
                tracing::debug!(error = %e, "prompt warm-up failed");
                return;
            }
        }
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "llm prompts warmed up"
        );
    });
}

const WARM_UP_TIMEOUT: Duration = Duration::from_secs(60);

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 文字数の上限などを検証する。
pub fn validate(mut req: ExplainRequest) -> AppResult<ExplainRequest> {
    req.text = req.text.trim().to_string();
    req.context = req.context.trim().to_string();
    if req.text.is_empty() {
        return Err(AppError::BadRequest("解説する文字列が空です".into()));
    }
    let text_len = req.text.chars().count();
    if text_len > MAX_TEXT_CHARS {
        return Err(AppError::BadRequest(format!(
            "選択が長すぎます（{text_len} 文字。上限 {MAX_TEXT_CHARS} 文字）"
        )));
    }
    let ctx_len = req.context.chars().count();
    if ctx_len > MAX_CONTEXT_CHARS {
        return Err(AppError::BadRequest(format!(
            "文脈が長すぎます（{ctx_len} 文字。上限 {MAX_CONTEXT_CHARS} 文字）"
        )));
    }
    Ok(req)
}

/// この解説のキャッシュキー。英文なら grammar_tags.yaml の内容のハッシュも含める。
pub fn key_for(state: &AppState, req: &ExplainRequest) -> String {
    // 英文は文法タグの定義と、スラッシュリーディングの有無もキーに含める
    let grammar = match req.mode {
        Mode::Sentence if req.slash => format!("{}+slash", state.grammar().tags_hash),
        Mode::Sentence => state.grammar().tags_hash.clone(),
        Mode::Word => String::new(),
    };
    cache_key(
        &state.config.llm_model,
        req.mode,
        &req.text,
        &req.context,
        prompts::PROMPT_VERSION,
        &grammar,
    )
}

/// tags イベントの data（名前と説明を付けたタグの JSON 配列）。
fn tags_event(g: &GrammarData, detected: &[DetectedTag]) -> String {
    serde_json::to_string(&g.resolve(detected)).unwrap_or_else(|_| "[]".into())
}

/// 生成を止めた理由。
#[derive(Debug)]
pub(crate) enum StreamError {
    /// クライアントが切断した（何も送らずに終わる）
    ClientGone,
    /// LLM 側の失敗（error イベントで知らせる）
    Llm(String),
}

/// LLM の生成結果。
pub(crate) struct Completion {
    pub text: String,
    /// `[DONE]` まで受け取り、長さの上限で打ち切られてもいない
    pub complete: bool,
}

async fn run(state: AppState, req: ExplainRequest, key: String, tx: mpsc::Sender<Event>) {
    let Some(_permit) = acquire_gate(&state, &tx).await else {
        return;
    };
    if send(&tx, sse_event::STARTED, "").await.is_err() {
        return;
    }

    let result = match tokio::time::timeout(GENERATION_TIMEOUT, generate(&state, &req, &tx)).await {
        Ok(r) => r,
        Err(_) => Err(StreamError::Llm("解説の生成がタイムアウトしました".into())),
    };
    match result {
        Ok((c, detected)) => {
            tracing::info!(
                mode = req.mode.as_str(),
                chars = c.text.chars().count(),
                complete = c.complete,
                "explain done"
            );
            // クライアントへの完了通知より先に保存する（直後に同じ解説を頼まれてもヒットするように）
            if c.complete {
                let tags_json = detected
                    .as_ref()
                    .and_then(|d| serde_json::to_string(d).ok());
                let entry = db::NewExplanation {
                    key: &key,
                    model: &state.config.llm_model,
                    mode: req.mode,
                    text: &req.text,
                    markdown: &c.text,
                    prompt_version: prompts::PROMPT_VERSION,
                    tags: tags_json.as_deref(),
                };
                if let Err(e) = db::put_explanation(&state.db, &entry).await {
                    tracing::warn!(error = %e, "explain cache write failed");
                }
            }
            let _ = send(&tx, sse_event::DONE, "").await;
        }
        Err(StreamError::ClientGone) => tracing::info!("client disconnected; generation aborted"),
        Err(StreamError::Llm(msg)) => {
            tracing::warn!(error = %msg, "explain failed");
            let _ = send(&tx, sse_event::ERROR, &msg).await;
        }
    }
}

/// LLM の順番を取る。空いていなければ `queued` を送って待つ。待つ間に切断されたら None。
pub(crate) async fn acquire_gate(
    state: &AppState,
    tx: &mpsc::Sender<Event>,
) -> Option<OwnedSemaphorePermit> {
    let gate = state.llm_gate.clone();
    if let Ok(p) = gate.clone().try_acquire_owned() {
        return Some(p);
    }
    send(tx, sse_event::QUEUED, "").await.ok()?;
    tokio::select! {
        p = gate.acquire_owned() => p.ok(),
        // 待っている間に切断されたら並ばない
        _ = tx.closed() => {
            tracing::debug!("client left while queued");
            None
        }
    }
}

pub(crate) async fn send(
    tx: &mpsc::Sender<Event>,
    event: &str,
    data: &str,
) -> Result<(), StreamError> {
    tx.send(Event::default().event(event).data(data))
        .await
        .map_err(|_| StreamError::ClientGone)
}

/// 解説を生成して流す。英文は ① 和訳 → ② 文法タグ付け → ③ 文構造・文法ポイント・語句 の順に行う。
///
/// 和訳はタグに依らないので、短いプロンプトで先に流して最初の文字が出るまでを短くする
/// （タグ付けは 100 トークンほどの JSON を生成するので数秒かかる）。③ には和訳と検出したタグを渡す。
/// 3 回の問い合わせは同じ Semaphore の中で順に行う（同時には投げない）。
async fn generate(
    state: &AppState,
    req: &ExplainRequest,
    tx: &mpsc::Sender<Event>,
) -> Result<(Completion, Option<Vec<DetectedTag>>), StreamError> {
    let cfg = &state.config;
    if cfg.llm_model.is_empty() {
        return Err(StreamError::Llm(
            "LLM_MODEL が設定されていません（.env を確認してください）".into(),
        ));
    }
    let body = |system: String, user: String, max_tokens: u32| {
        chat_stream_body(
            &cfg.llm_model,
            &system,
            &user,
            max_tokens,
            cfg.llm_disable_thinking,
        )
    };
    if req.mode == Mode::Word {
        let b = body(
            prompts::system_prompt(Mode::Word, false),
            prompts::user_message(req, &[], None),
            MAX_TOKENS,
        );
        return Ok((stream_llm(state, &b, tx).await?, None));
    }

    // ① 和訳
    send(tx, sse_event::TOKEN, prompts::TRANSLATION_HEADING).await?;
    let b = body(
        prompts::translation_system_prompt(),
        prompts::translation_user_message(req),
        TRANSLATION_MAX_TOKENS,
    );
    let translation = stream_llm(state, &b, tx).await?;
    send(tx, sse_event::TOKEN, SECTION_BREAK).await?;

    // ② 文法タグ付け（失敗してもタグ無しで続ける）
    let grammar = state.grammar();
    let tagger = Tagger {
        http: &state.llm_stream_http,
        base_url: &cfg.llm_base_url,
        model: &cfg.llm_model,
        disable_thinking: cfg.llm_disable_thinking,
        structured_output: &state.structured_output,
    };
    let tags = tokio::select! {
        t = tagger.tag(&grammar, &req.text) => t,
        _ = tx.closed() => return Err(StreamError::ClientGone),
    };
    send(tx, sse_event::TAGS, &tags_event(&grammar, &tags)).await?;
    let notes: Vec<GrammarNote<'_>> = tags
        .iter()
        .filter_map(|d| {
            grammar.tag(&d.tag_id).map(|t| GrammarNote {
                name: &t.name,
                span: &d.span,
            })
        })
        .collect();

    // ③ 文構造・文法ポイント・語句
    let b = body(
        prompts::system_prompt(Mode::Sentence, req.slash),
        prompts::user_message(req, &notes, Some(&translation.text)),
        MAX_TOKENS,
    );
    let rest = stream_llm(state, &b, tx).await?;
    let complete = translation.complete && rest.complete;
    Ok((
        Completion {
            // クライアントに送った順のとおりにつなぐ（キャッシュから返すときも同じ表示になる）
            text: format!(
                "{}{}{SECTION_BREAK}{}",
                prompts::TRANSLATION_HEADING,
                translation.text,
                rest.text
            ),
            complete,
        },
        Some(tags),
    ))
}

/// LLM にストリーミングで問い合わせ、断片を token イベントとして送る。生成した全文を返す。
pub(crate) async fn stream_llm(
    state: &AppState,
    body: &Value,
    tx: &mpsc::Sender<Event>,
) -> Result<Completion, StreamError> {
    let cfg = &state.config;
    let url = format!(
        "{}/chat/completions",
        cfg.llm_base_url.trim_end_matches('/')
    );
    let send_req = state.llm_stream_http.post(&url).json(body).send();
    let mut resp = tokio::select! {
        r = send_req => r.map_err(|e| StreamError::Llm(describe_reqwest(&e, &cfg.llm_base_url)))?,
        _ = tx.closed() => return Err(StreamError::ClientGone),
    };
    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(StreamError::Llm(describe_status(
            status.as_u16(),
            &detail,
            &cfg.llm_model,
        )));
    }

    let mut parser = SseParser::default();
    let mut full = String::new();
    let mut truncated = false;
    loop {
        let chunk = tokio::select! {
            c = resp.chunk() => c.map_err(|e| StreamError::Llm(describe_reqwest(&e, &cfg.llm_base_url)))?,
            _ = tx.closed() => return Err(StreamError::ClientGone),
        };
        let Some(chunk) = chunk else {
            // [DONE] が来ずに閉じられた
            return if full.is_empty() {
                Err(StreamError::Llm("LLM の応答が途中で終了しました".into()))
            } else {
                Ok(Completion {
                    text: full,
                    complete: false,
                })
            };
        };
        for ev in parser.push(&String::from_utf8_lossy(&chunk)) {
            match parse_upstream(&ev.data)? {
                Upstream::Done => {
                    return Ok(Completion {
                        complete: !truncated && !full.trim().is_empty(),
                        text: full,
                    });
                }
                Upstream::Delta { content, finish } => {
                    if finish.as_deref() == Some("length") {
                        truncated = true;
                    }
                    if !content.is_empty() {
                        full.push_str(&content);
                        send(tx, sse_event::TOKEN, &content).await?;
                    }
                }
                Upstream::Skip => {}
            }
        }
    }
}

/// OpenAI 互換 /chat/completions のストリーミングのリクエスト body。
pub fn chat_stream_body(
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
    disable_thinking: bool,
) -> Value {
    let mut body = json!({
        "model": model,
        "stream": true,
        "temperature": 0.3,
        "max_tokens": max_tokens,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    if disable_thinking {
        // 推論（thinking）を行うモデルで、最初の文字が出るまでの待ちをなくす
        body["reasoning_effort"] = json!("none");
    }
    body
}

#[derive(Debug, PartialEq, Eq)]
enum Upstream {
    /// 本文の断片と、最後の断片なら終了理由（"stop", "length" など）
    Delta {
        content: String,
        finish: Option<String>,
    },
    Done,
    Skip,
}

/// 上流の SSE の data 1 件を解釈する。
fn parse_upstream(data: &str) -> Result<Upstream, StreamError> {
    let data = data.trim();
    if data.is_empty() {
        return Ok(Upstream::Skip);
    }
    if data == "[DONE]" {
        return Ok(Upstream::Done);
    }
    let v: Value = serde_json::from_str(data)
        .map_err(|_| StreamError::Llm("LLM の応答を解釈できません".into()))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| err.to_string());
        return Err(StreamError::Llm(format!("LLM がエラーを返しました: {msg}")));
    }
    let content = v
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .unwrap_or("");
    let finish = v
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if content.is_empty() && finish.is_none() {
        Ok(Upstream::Skip)
    } else {
        Ok(Upstream::Delta {
            content: content.to_string(),
            finish,
        })
    }
}

fn describe_reqwest(e: &reqwest::Error, base_url: &str) -> String {
    if e.is_connect() {
        format!(
            "LLM ランタイム（{base_url}）に接続できません。Ollama などが起動しているか確認してください。"
        )
    } else if e.is_timeout() {
        "LLM ランタイムからの応答がタイムアウトしました".into()
    } else {
        format!("LLM ランタイムとの通信に失敗しました: {e}")
    }
}

fn describe_status(status: u16, body: &str, model: &str) -> String {
    let detail: String = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("error"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    if status == 404 {
        format!(
            "モデル「{model}」が見つかりません。`ollama pull {model}` を実行するか LLM_MODEL を確認してください。（{detail}）"
        )
    } else {
        format!("LLM ランタイムがエラーを返しました（HTTP {status}）: {detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paper_reader_shared::Mode;

    fn req(text: &str, context: &str) -> ExplainRequest {
        ExplainRequest {
            mode: Mode::Sentence,
            text: text.into(),
            context: context.into(),
            regenerate: false,
            slash: false,
        }
    }

    #[test]
    fn validate_limits() {
        assert!(validate(req("  ", "")).is_err());
        assert!(validate(req(&"a".repeat(MAX_TEXT_CHARS), "")).is_ok());
        assert!(validate(req(&"a".repeat(MAX_TEXT_CHARS + 1), "")).is_err());
        // 文字数（バイト数ではない）で数える
        assert!(validate(req("x", &"あ".repeat(MAX_CONTEXT_CHARS))).is_ok());
        assert!(validate(req("x", &"あ".repeat(MAX_CONTEXT_CHARS + 1))).is_err());
        let r = validate(req("  We propose.\n", " ctx ")).unwrap();
        assert_eq!(
            (r.text.as_str(), r.context.as_str()),
            ("We propose.", "ctx")
        );
    }

    #[test]
    fn upstream_parsing() {
        let tok = r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"和訳"}}]}"#;
        assert_eq!(
            parse_upstream(tok).unwrap(),
            Upstream::Delta {
                content: "和訳".into(),
                finish: None
            }
        );
        let last = r#"{"choices":[{"delta":{"content":""},"finish_reason":"length"}]}"#;
        assert_eq!(
            parse_upstream(last).unwrap(),
            Upstream::Delta {
                content: String::new(),
                finish: Some("length".into())
            }
        );
        let reasoning = r#"{"choices":[{"delta":{"reasoning":"thinking..."}}]}"#;
        assert_eq!(parse_upstream(reasoning).unwrap(), Upstream::Skip);
        assert_eq!(parse_upstream("[DONE]").unwrap(), Upstream::Done);
        assert_eq!(parse_upstream("").unwrap(), Upstream::Skip);
        let err = r#"{"error":{"message":"model not found"}}"#;
        assert!(
            matches!(parse_upstream(err), Err(StreamError::Llm(m)) if m.contains("model not found"))
        );
        assert!(parse_upstream("{broken").is_err());
    }

    #[test]
    fn body_shape() {
        let b = chat_stream_body("m", "sys", "We propose.", 99, true);
        assert_eq!(b["model"], "m");
        assert_eq!(b["max_tokens"], 99);
        assert_eq!(b["stream"], true);
        assert_eq!(b["messages"][0]["role"], "system");
        assert!(
            b["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("We propose.")
        );
        assert_eq!(b["reasoning_effort"], "none");
        assert!(
            chat_stream_body("m", "s", "x", 1, false)
                .get("reasoning_effort")
                .is_none()
        );
    }

    #[test]
    fn status_messages() {
        let m = describe_status(404, r#"{"error":{"message":"model \"x\" not found"}}"#, "x");
        assert!(m.contains("ollama pull x"));
        let m = describe_status(500, "boom", "x");
        assert!(m.contains("HTTP 500") && m.contains("boom"));
    }
}
