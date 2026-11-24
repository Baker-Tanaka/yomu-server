//! `POST /api/explain` — LLM の解説を SSE で中継する。
//!
//! - LLM への同時リクエストは `AppState::llm_gate`（Semaphore, 1 件）で制限し、待つときは `queued` を送る
//! - クライアントが切断したら（SSE の受け手が drop されたら）上流のストリームも drop して生成を止める

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use paper_reader_shared::{
    ExplainRequest, MAX_CONTEXT_CHARS, MAX_TEXT_CHARS, SseParser, sse_event,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::prompts;

/// 1 回の解説の上限時間（順番待ちは含まない）
const GENERATION_TIMEOUT: Duration = Duration::from_secs(300);
/// 生成する最大トークン数
const MAX_TOKENS: u32 = 1_500;

pub async fn post_explain(
    State(state): State<AppState>,
    body: Result<Json<ExplainRequest>, JsonRejection>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let Json(req) =
        body.map_err(|e| AppError::BadRequest(format!("リクエストを解釈できません: {e}")))?;
    let req = validate(req)?;

    let (tx, rx) = mpsc::channel::<Event>(64);
    tokio::spawn(run(state, req, tx));

    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| (Ok(ev), rx))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
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

/// 生成を止めた理由。
#[derive(Debug)]
enum StreamError {
    /// クライアントが切断した（何も送らずに終わる）
    ClientGone,
    /// LLM 側の失敗（error イベントで知らせる）
    Llm(String),
}

async fn run(state: AppState, req: ExplainRequest, tx: mpsc::Sender<Event>) {
    let gate = state.llm_gate.clone();
    let _permit = match gate.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            if send(&tx, sse_event::QUEUED, "").await.is_err() {
                return;
            }
            tokio::select! {
                p = gate.acquire_owned() => match p {
                    Ok(p) => p,
                    Err(_) => return,
                },
                // 待っている間に切断されたら並ばない
                _ = tx.closed() => {
                    tracing::debug!("client left while queued");
                    return;
                }
            }
        }
    };
    if send(&tx, sse_event::STARTED, "").await.is_err() {
        return;
    }

    let result = match tokio::time::timeout(GENERATION_TIMEOUT, stream_llm(&state, &req, &tx)).await
    {
        Ok(r) => r,
        Err(_) => Err(StreamError::Llm("解説の生成がタイムアウトしました".into())),
    };
    match result {
        Ok(text) => {
            tracing::info!(
                mode = req.mode.as_str(),
                chars = text.chars().count(),
                "explain done"
            );
            let _ = send(&tx, sse_event::DONE, "").await;
        }
        Err(StreamError::ClientGone) => tracing::info!("client disconnected; generation aborted"),
        Err(StreamError::Llm(msg)) => {
            tracing::warn!(error = %msg, "explain failed");
            let _ = send(&tx, sse_event::ERROR, &msg).await;
        }
    }
}

async fn send(tx: &mpsc::Sender<Event>, event: &str, data: &str) -> Result<(), StreamError> {
    tx.send(Event::default().event(event).data(data))
        .await
        .map_err(|_| StreamError::ClientGone)
}

/// LLM にストリーミングで問い合わせ、断片を token イベントとして送る。生成した全文を返す。
async fn stream_llm(
    state: &AppState,
    req: &ExplainRequest,
    tx: &mpsc::Sender<Event>,
) -> Result<String, StreamError> {
    let cfg = &state.config;
    if cfg.llm_model.is_empty() {
        return Err(StreamError::Llm(
            "LLM_MODEL が設定されていません（.env を確認してください）".into(),
        ));
    }
    let url = format!(
        "{}/chat/completions",
        cfg.llm_base_url.trim_end_matches('/')
    );
    let body = request_body(&cfg.llm_model, req, cfg.llm_disable_thinking);

    let send_req = state.llm_stream_http.post(&url).json(&body).send();
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
                Ok(full)
            };
        };
        for ev in parser.push(&String::from_utf8_lossy(&chunk)) {
            match parse_upstream(&ev.data)? {
                Upstream::Done => return Ok(full),
                Upstream::Token(t) => {
                    full.push_str(&t);
                    send(tx, sse_event::TOKEN, &t).await?;
                }
                Upstream::Skip => {}
            }
        }
    }
}

/// OpenAI 互換 /chat/completions のリクエスト body。
pub fn request_body(model: &str, req: &ExplainRequest, disable_thinking: bool) -> Value {
    let mut body = json!({
        "model": model,
        "stream": true,
        "temperature": 0.3,
        "max_tokens": MAX_TOKENS,
        "messages": [
            { "role": "system", "content": prompts::system_prompt(req.mode) },
            { "role": "user", "content": prompts::user_message(req) },
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
    Token(String),
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
    if content.is_empty() {
        Ok(Upstream::Skip)
    } else {
        Ok(Upstream::Token(content.to_string()))
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
        assert_eq!(parse_upstream(tok).unwrap(), Upstream::Token("和訳".into()));
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
        let b = request_body("m", &req("We propose.", ""), true);
        assert_eq!(b["model"], "m");
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
            request_body("m", &req("x", ""), false)
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
