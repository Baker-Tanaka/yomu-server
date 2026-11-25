//! 英文の文法タグ付け（LLM へのストリーミングなしの問い合わせ）。
//!
//! - 構造化出力（response_format: json_schema、tag_id は enum）を使う
//! - ランタイムが json_schema を受け付けなければ（HTTP 400 / 422 など）、以後は付けずに「JSON だけを出力」させる
//! - 失敗・タイムアウト・壊れた JSON のときはタグ無しで続ける（解説は止めない）

use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::time::Duration;

#[cfg(test)]
use serde_json::Value;

use crate::grammar::{
    DetectedTag, GrammarData, parse_tagging_response, tagging_schema, tagging_system_prompt,
    tagging_user_message,
};
#[cfg(test)]
use crate::llm::chat_body;
use crate::llm::{JsonChat, JsonRequest};

/// タグ付けの上限時間
const TAGGING_TIMEOUT: Duration = Duration::from_secs(60);
const TAGGING_MAX_TOKENS: u32 = 400;

pub struct Tagger<'a> {
    pub http: &'a reqwest::Client,
    pub base_url: &'a str,
    pub model: &'a str,
    pub disable_thinking: bool,
    /// json_schema を使えるか（使えないと分かったら false にする）
    pub structured_output: &'a AtomicBool,
}

impl Tagger<'_> {
    /// 英文の文法タグを返す。失敗したら空（理由はログに出す）。
    pub async fn tag(&self, g: &GrammarData, sentence: &str) -> Vec<DetectedTag> {
        let chat = JsonChat {
            http: self.http,
            base_url: self.base_url,
            model: self.model,
            disable_thinking: self.disable_thinking,
            structured_output: self.structured_output,
        };
        let system = tagging_system_prompt(g);
        let user = tagging_user_message(sentence);
        let req = JsonRequest {
            system: &system,
            user: &user,
            response_format: tagging_schema(g),
            max_tokens: TAGGING_MAX_TOKENS,
            timeout: TAGGING_TIMEOUT,
        };
        let content = match chat.complete(&req).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "grammar tagging failed");
                return Vec::new();
            }
        };
        let tags = parse_tagging_response(&content, g, sentence);
        tracing::info!(
            tags = ?tags.iter().map(|t| t.tag_id.as_str()).collect::<Vec<_>>(),
            "grammar tagged"
        );
        tags
    }
}

impl Tagger<'_> {
    /// タグ付けのプロンプト（3,000 トークンほど）を LLM に読み込ませておく。結果は使わない。
    /// プロンプトのキャッシュが冷えていると、最初のタグ付けだけで 15 秒ほどかかるため。
    pub async fn warm_up(&self, g: &GrammarData) {
        let chat = JsonChat {
            http: self.http,
            base_url: self.base_url,
            model: self.model,
            disable_thinking: self.disable_thinking,
            structured_output: self.structured_output,
        };
        let system = tagging_system_prompt(g);
        let user = tagging_user_message("We propose a simple method.");
        let req = JsonRequest {
            system: &system,
            user: &user,
            response_format: tagging_schema(g),
            max_tokens: 1,
            timeout: TAGGING_TIMEOUT,
        };
        if let Err(e) = chat.complete(&req).await {
            tracing::debug!(error = %e, "tagging warm-up failed");
        }
    }
}

/// タグ付けのリクエスト body（テスト用に構造を確かめる）。
#[cfg(test)]
fn request_body(
    model: &str,
    g: &GrammarData,
    sentence: &str,
    structured: bool,
    disable_thinking: bool,
) -> Value {
    chat_body(
        model,
        &tagging_system_prompt(g),
        &tagging_user_message(sentence),
        structured.then(|| tagging_schema(g)),
        TAGGING_MAX_TOKENS,
        disable_thinking,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::grammar::{GrammarPaths, tests::sample};

    #[test]
    fn body_shape() {
        let g = sample();
        let b = request_body("m", &g, "We propose X.", true, true);
        assert_eq!(b["stream"], false);
        assert_eq!(b["max_tokens"], TAGGING_MAX_TOKENS);
        assert_eq!(b["response_format"]["type"], "json_schema");
        assert_eq!(b["reasoning_effort"], "none");
        let b = request_body("m", &g, "We propose X.", false, false);
        assert!(b.get("response_format").is_none());
        assert!(
            b["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("JSON")
        );
    }

    /// LLM が落ちていてもタグ無しで返る（解説を止めない）。
    #[tokio::test]
    async fn unreachable_llm_gives_no_tags() {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let flag = AtomicBool::new(true);
        let tagger = Tagger {
            http: &http,
            // 何も待ち受けていないポート
            base_url: "http://127.0.0.1:9/v1",
            model: "m",
            disable_thinking: true,
            structured_output: &flag,
        };
        assert!(tagger.tag(&sample(), "We propose X.").await.is_empty());
        // 接続できないだけなら json_schema の可否は変えない
        assert!(flag.load(Ordering::Relaxed));
    }

    /// 実際の LLM で例文にタグが付くかを確かめる（Ollama などが必要）。
    ///
    /// cargo test -p paper-reader-server -- --ignored --nocapture tagging_examples
    #[tokio::test]
    #[ignore = "LLM ランタイムが必要"]
    async fn tagging_examples() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let g = GrammarData::load(&GrammarPaths {
            tags: data.join("grammar_tags.yaml"),
            map: data.join("grammar_map.local.yaml"),
            template: data.join("grammar_map.template.yaml"),
        })
        .unwrap();
        let base_url =
            std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into());
        let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| "gemma4:e4b-it-qat".into());
        let http = reqwest::Client::new();
        let flag = AtomicBool::new(true);
        let tagger = Tagger {
            http: &http,
            base_url: &base_url,
            model: &model,
            disable_thinking: true,
            structured_output: &flag,
        };

        let cases: &[(&str, &[&str])] = &[
            (
                "The method we propose outperforms existing approaches.",
                &["relative-pronoun"],
            ),
            (
                "Having trained the model, we evaluated it on three datasets.",
                &["participial-construction", "perfect-participle"],
            ),
            (
                "This finding suggests that the effect is not limited to English.",
                &["noun-clause-that-whether"],
            ),
            (
                "The fact that the loss converges indicates stability.",
                &["appositive-that", "inanimate-subject"],
            ),
        ];
        let mut failures = Vec::new();
        for (sentence, expected) in cases {
            let tags = tagger.tag(&g, sentence).await;
            let ids: Vec<&str> = tags.iter().map(|t| t.tag_id.as_str()).collect();
            println!("{sentence}\n  → {tags:?}");
            for e in *expected {
                if !ids.contains(e) {
                    failures.push(format!("{sentence}: {e} がない（{ids:?}）"));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
