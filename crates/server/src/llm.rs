//! LLM ランタイム（OpenAI 互換 /chat/completions）への、ストリーミングなしで JSON を返させる問い合わせ。
//!
//! 文法タグ付けとフィードバックのラベル付けで使う。
//! - 構造化出力（response_format: json_schema）を付けて送る
//! - ランタイムが json_schema を受け付けなければ（HTTP 400 / 415 / 422 / 501）、以後は付けずに送り直す
//!   （プロンプトで「JSON だけを出力」させ、呼び出し側で検証する）

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

/// JSON を返させる問い合わせの設定。
pub struct JsonChat<'a> {
    pub http: &'a reqwest::Client,
    pub base_url: &'a str,
    pub model: &'a str,
    pub disable_thinking: bool,
    /// json_schema を使えるか（使えないと分かったら false にする。AppState で共有）
    pub structured_output: &'a AtomicBool,
}

/// 1 回分の問い合わせ。
pub struct JsonRequest<'a> {
    pub system: &'a str,
    pub user: &'a str,
    /// response_format の値（`{"type": "json_schema", ...}`）
    pub response_format: Value,
    pub max_tokens: u32,
    pub timeout: Duration,
}

#[derive(Debug)]
enum ChatError {
    /// json_schema を受け付けなかった
    SchemaRejected(String),
    Other(String),
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaRejected(m) => write!(f, "json_schema を受け付けませんでした: {m}"),
            Self::Other(m) => f.write_str(m),
        }
    }
}

impl JsonChat<'_> {
    /// 応答の本文（JSON のはずの文字列）を返す。失敗・タイムアウトは Err（理由）。
    pub async fn complete(&self, req: &JsonRequest<'_>) -> Result<String, String> {
        let structured = self.structured_output.load(Ordering::Relaxed);
        match self.attempt(req, structured).await {
            Err(ChatError::SchemaRejected(msg)) if structured => {
                tracing::warn!(error = %msg, "json_schema is not supported; falling back to plain JSON");
                self.structured_output.store(false, Ordering::Relaxed);
                self.attempt(req, false).await.map_err(|e| e.to_string())
            }
            other => other.map_err(|e| e.to_string()),
        }
    }

    async fn attempt(&self, req: &JsonRequest<'_>, structured: bool) -> Result<String, ChatError> {
        let fut = self.request(req, structured);
        match tokio::time::timeout(req.timeout, fut).await {
            Ok(r) => r,
            Err(_) => Err(ChatError::Other("LLM の応答がタイムアウトしました".into())),
        }
    }

    async fn request(&self, req: &JsonRequest<'_>, structured: bool) -> Result<String, ChatError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let format = structured.then(|| req.response_format.clone());
        let body = chat_body(
            self.model,
            req.system,
            req.user,
            format,
            req.max_tokens,
            self.disable_thinking,
        );
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ChatError::Other(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ChatError::Other(e.to_string()))?;
        if !status.is_success() {
            let msg = format!(
                "HTTP {}: {}",
                status.as_u16(),
                text.chars().take(200).collect::<String>()
            );
            return Err(
                if structured && matches!(status.as_u16(), 400 | 415 | 422 | 501) {
                    ChatError::SchemaRejected(msg)
                } else {
                    ChatError::Other(msg)
                },
            );
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| ChatError::Other(e.to_string()))?;
        Ok(v.pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }
}

/// /chat/completions の body（ストリーミングなし、temperature 0）。
pub fn chat_body(
    model: &str,
    system: &str,
    user: &str,
    response_format: Option<Value>,
    max_tokens: u32,
    disable_thinking: bool,
) -> Value {
    let mut body = json!({
        "model": model,
        "stream": false,
        "temperature": 0.0,
        "max_tokens": max_tokens,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    if let Some(format) = response_format {
        body["response_format"] = format;
    }
    if disable_thinking {
        body["reasoning_effort"] = json!("none");
    }
    body
}

/// 前後に文章やコードブロックが付いていても、最初の `{` から最後の `}` までを取り出す。
pub fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    (end > start).then(|| &s[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_shape() {
        let b = chat_body(
            "m",
            "sys",
            "usr",
            Some(json!({"type": "json_schema"})),
            99,
            true,
        );
        assert_eq!(b["stream"], false);
        assert_eq!(b["max_tokens"], 99);
        assert_eq!(b["messages"][1]["content"], "usr");
        assert_eq!(b["response_format"]["type"], "json_schema");
        assert_eq!(b["reasoning_effort"], "none");
        let b = chat_body("m", "s", "u", None, 1, false);
        assert!(b.get("response_format").is_none() && b.get("reasoning_effort").is_none());
    }

    #[test]
    fn json_extraction() {
        assert_eq!(
            extract_json_object("x ```json\n{\"a\":{}}\n``` y"),
            Some("{\"a\":{}}")
        );
        assert_eq!(extract_json_object("no json"), None);
        assert_eq!(extract_json_object("} {"), None);
    }
}
