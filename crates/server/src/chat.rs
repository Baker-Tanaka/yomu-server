//! `POST /api/chat` — 解説のあとに続けて質問する。LLM の回答を SSE で中継する。
//!
//! - 順番待ち（queued）・切断時の中止・タイムアウトは解説（explain.rs）と同じで、同じ Semaphore の 1 件の枠を使う
//! - 会話はサーバーに保存しない（画面が毎回これまでのやり取りを送る）。キャッシュもしない
//! - LLM のコンテキスト長（Ollama の既定は 4096 トークン）を超えないよう、`fit` で資料と古いやり取りを切り詰める。
//!   超えるとランタイムがプロンプトの先頭（指示）から黙って捨てるため

use std::convert::Infallible;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use paper_reader_shared::{
    ChatMessage, ChatRequest, ChatRole, MAX_CHAT_ANSWER_CHARS, MAX_CHAT_MESSAGES,
    MAX_CHAT_QUESTION_CHARS, MAX_CONTEXT_CHARS, MAX_TEXT_CHARS, clip_context, sse_event,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::explain::{
    GENERATION_TIMEOUT, StreamError, acquire_gate, chat_stream_body, send, stream_llm,
};
use crate::knowledge;
use crate::prompts;

/// 回答の最大トークン数
const CHAT_MAX_TOKENS: u32 = 900;
/// プロンプトに使うトークン数の目安（4096 から回答の分と余裕を引く）
const PROMPT_BUDGET: usize = 3_000;
/// 解説と文脈に使うトークン数の上限（残りは会話に回す）
const EXPLANATION_BUDGET: usize = 1_100;
const CONTEXT_BUDGET: usize = 500;

pub async fn post_chat(
    State(state): State<AppState>,
    body: Result<Json<ChatRequest>, JsonRejection>,
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

async fn run(state: AppState, req: ChatRequest, tx: mpsc::Sender<Event>) {
    let Some(_permit) = acquire_gate(&state, &tx).await else {
        return;
    };
    if send(&tx, sse_event::STARTED, "").await.is_err() {
        return;
    }
    let result = match tokio::time::timeout(GENERATION_TIMEOUT, generate(&state, &req, &tx)).await {
        Ok(r) => r,
        Err(_) => Err(StreamError::Llm("回答の生成がタイムアウトしました".into())),
    };
    match result {
        Ok(()) => {
            let _ = send(&tx, sse_event::DONE, "").await;
        }
        Err(StreamError::ClientGone) => tracing::info!("client disconnected; chat aborted"),
        Err(StreamError::Llm(msg)) => {
            tracing::warn!(error = %msg, "chat failed");
            let _ = send(&tx, sse_event::ERROR, &msg).await;
        }
    }
}

async fn generate(
    state: &AppState,
    req: &ChatRequest,
    tx: &mpsc::Sender<Event>,
) -> Result<(), StreamError> {
    let cfg = &state.config;
    if cfg.llm_model.is_empty() {
        return Err(StreamError::Llm(
            "LLM_MODEL が設定されていません（.env を確認してください）".into(),
        ));
    }
    // 対象と今回の質問に近い知識を添える（知識が無ければ問い合わせない）
    let question = req.messages.last().map_or("", |m| m.content.as_str());
    let notes = knowledge::retrieve(
        state,
        &format!("{}\n{question}", req.text),
        knowledge::CHAT_TOP_K,
        knowledge::CHAT_NOTE_CHARS,
    )
    .await;
    let (system, history) = fit(req, &notes.prompt_section());
    let question = history.last().map(|m| m.content.as_str()).unwrap_or("");
    let mut body = chat_stream_body(
        &cfg.llm_model,
        &system,
        question,
        CHAT_MAX_TOKENS,
        cfg.llm_disable_thinking,
    );
    body["messages"] = messages_json(&system, &history);
    let c = stream_llm(state, &body, tx).await?;
    tracing::info!(
        turns = history.len(),
        chars = c.text.chars().count(),
        complete = c.complete,
        "chat done"
    );
    Ok(())
}

fn messages_json(system: &str, history: &[ChatMessage]) -> Value {
    let mut out = vec![json!({ "role": "system", "content": system })];
    out.extend(
        history
            .iter()
            .map(|m| json!({ "role": m.role, "content": m.content })),
    );
    Value::Array(out)
}

/// 文字数の上限と、やり取りの形（質問と回答が交互で、最初と最後が質問）を検証する。
fn validate(mut req: ChatRequest) -> AppResult<ChatRequest> {
    let bad = |m: &str| Err(AppError::BadRequest(m.into()));
    req.text = req.text.trim().to_string();
    req.context = req.context.trim().to_string();
    req.explanation = req.explanation.trim().to_string();
    if req.text.is_empty() {
        return bad("解説の対象が空です");
    }
    if req.text.chars().count() > MAX_TEXT_CHARS
        || req.context.chars().count() > MAX_CONTEXT_CHARS
        || req.explanation.chars().count() > MAX_CHAT_ANSWER_CHARS
    {
        return bad("解説の対象・文脈・解説が長すぎます");
    }
    if req.messages.len() > MAX_CHAT_MESSAGES {
        return bad("会話が長すぎます");
    }
    if req.messages.len().is_multiple_of(2) {
        return bad("質問がありません");
    }
    for (i, m) in req.messages.iter_mut().enumerate() {
        let (role, max) = if i % 2 == 0 {
            (ChatRole::User, MAX_CHAT_QUESTION_CHARS)
        } else {
            (ChatRole::Assistant, MAX_CHAT_ANSWER_CHARS)
        };
        if m.role != role {
            return bad("会話の順番が正しくありません");
        }
        m.content = m.content.trim().to_string();
        if m.content.is_empty() {
            return bad("空の発言があります");
        }
        let len = m.content.chars().count();
        if len > max {
            return Err(AppError::BadRequest(format!(
                "発言が長すぎます（{len} 文字。上限 {max} 文字）"
            )));
        }
    }
    Ok(req)
}

/// トークン数の多めの見積もり（英数字は 3 文字で 1、日本語などは 1 文字で 1）。
fn estimate_tokens(s: &str) -> usize {
    let (ascii, other) = s.chars().fold((0usize, 0usize), |(a, o), c| {
        if c.is_ascii() { (a + 1, o) } else { (a, o + 1) }
    });
    ascii.div_ceil(3) + other
}

/// 先頭から `budget` トークンに収まる分だけ残す（切ったら「…」を付ける）。
fn head_within(s: &str, budget: usize) -> String {
    if estimate_tokens(s) <= budget {
        return s.to_string();
    }
    // 英数字 1 文字を 1、ほかを 3 として数え、予算の 3 倍まで
    let mut used = 0;
    let mut out: String = s
        .chars()
        .take_while(|c| {
            used += if c.is_ascii() { 1 } else { 3 };
            used <= budget.saturating_sub(1) * 3
        })
        .collect();
    out.push('…');
    out
}

/// システムプロンプトと、送るやり取りをトークンの予算に収める。
///
/// 指示・解説の対象・今回の質問は必ず入れる。解説と文脈は上限まで切り詰め、
/// 残りに新しいやり取りから順に（質問と回答の組で）入れる。
/// `knowledge` は添える知識の節（空なら無し）で、指示と同じく必ず入れる。
fn fit(req: &ChatRequest, knowledge: &str) -> (String, Vec<ChatMessage>) {
    let fixed = estimate_tokens(&prompts::chat_system_prompt(
        req.mode, req.style, &req.text, "", "",
    )) + estimate_tokens(knowledge);
    let last = req.messages.len().saturating_sub(1);
    let question = req
        .messages
        .get(last)
        .map(|m| estimate_tokens(&m.content))
        .unwrap_or(0);
    let mut rest = PROMPT_BUDGET.saturating_sub(fixed + question);

    // 直前のやり取りが入るように、解説と文脈は残りの一部までにする
    let explanation = head_within(&req.explanation, EXPLANATION_BUDGET.min(rest * 2 / 3));
    rest = rest.saturating_sub(estimate_tokens(&explanation));
    let context_budget = CONTEXT_BUDGET.min(rest / 2);
    // 対象の周りを残す（英語なので 1 トークン 3 文字で数える）
    let context = if context_budget < 50 {
        String::new()
    } else {
        head_within(
            &clip_context(&req.context, &req.text, context_budget * 3),
            context_budget,
        )
    };
    rest = rest.saturating_sub(estimate_tokens(&context));

    let mut start = last;
    while start >= 2 {
        let pair: usize = req.messages[start - 2..start]
            .iter()
            .map(|m| estimate_tokens(&m.content))
            .sum();
        if pair > rest {
            break;
        }
        rest -= pair;
        start -= 2;
    }
    let mut history = req.messages[start..].to_vec();
    // 直前の回答が長くて入らないときも、話がつながるように先頭だけは送る
    if start == last && last >= 2 && rest >= 200 {
        let mut previous = req.messages[last - 2..last].to_vec();
        let asked = estimate_tokens(&previous[0].content);
        if asked + 100 <= rest {
            previous[1].content = head_within(&previous[1].content, rest - asked);
            previous.append(&mut history);
            history = previous;
        }
    }
    let mut system =
        prompts::chat_system_prompt(req.mode, req.style, &req.text, &context, &explanation);
    system.push_str(knowledge);
    (system, history)
}

#[cfg(test)]
mod tests {
    use super::*;
    use paper_reader_shared::Mode;

    fn msg(role: ChatRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.into(),
        }
    }

    fn req(messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            mode: Mode::Sentence,
            text: "We propose a method.".into(),
            context: "Intro. We propose a method. Outro.".into(),
            explanation: "## 文構造\n- 主語: We".into(),
            style: Default::default(),
            messages,
        }
    }

    /// 質問と回答を `pairs` 組と、最後の質問。
    fn talk(pairs: usize, answer: &str) -> Vec<ChatMessage> {
        let mut m = Vec::new();
        for i in 0..pairs {
            m.push(msg(ChatRole::User, &format!("質問{i}")));
            m.push(msg(ChatRole::Assistant, answer));
        }
        m.push(msg(ChatRole::User, "今の質問"));
        m
    }

    #[test]
    fn validate_shape_and_limits() {
        assert!(validate(req(talk(0, ""))).is_ok());
        assert!(validate(req(talk(2, "回答"))).is_ok());
        assert!(validate(req(vec![])).is_err());
        // 最後が回答
        assert!(validate(req(talk(1, "回答")[..2].to_vec())).is_err());
        // system などの順番違い
        assert!(validate(req(vec![msg(ChatRole::Assistant, "x")])).is_err());
        assert!(validate(req(vec![msg(ChatRole::User, "  ")])).is_err());
        let long = "あ".repeat(MAX_CHAT_QUESTION_CHARS + 1);
        assert!(validate(req(vec![msg(ChatRole::User, &long)])).is_err());
        assert!(validate(req(talk(MAX_CHAT_MESSAGES / 2, "回答"))).is_ok());
        assert!(validate(req(talk(MAX_CHAT_MESSAGES / 2 + 1, "回答"))).is_err());
        let mut r = req(talk(0, ""));
        r.text = " ".into();
        assert!(validate(r).is_err());
        // 知らない発言者（system）は受け付けない
        assert!(serde_json::from_str::<ChatMessage>(r#"{"role":"system","content":"x"}"#).is_err());
    }

    #[test]
    fn fit_keeps_everything_when_small() {
        let r = req(talk(2, "短い回答"));
        let (system, history) = fit(&r, "");
        assert_eq!(history, r.messages);
        assert!(system.contains("We propose a method.") && system.contains("主語: We"));
        assert!(system.contains("Intro."));
    }

    #[test]
    fn fit_drops_old_turns_and_stays_within_budget() {
        let mut r = req(talk(8, &"長い回答です。".repeat(80)));
        r.explanation = "解説".repeat(3_000);
        r.context = "context ".repeat(500);
        let (system, history) = fit(&r, "");
        let total: usize = estimate_tokens(&system)
            + history
                .iter()
                .map(|m| estimate_tokens(&m.content))
                .sum::<usize>();
        assert!(total <= PROMPT_BUDGET, "{total}");
        assert!(history.len() < r.messages.len() && history.len() >= 3);
        assert_eq!(history[0].role, ChatRole::User);
        assert_eq!(history.last().unwrap().content, "今の質問");
        // 新しいやり取りが残る
        assert_eq!(history[history.len() - 3].content, "質問7");
        assert!(system.contains('…'));
    }

    #[test]
    fn fit_counts_knowledge_within_budget() {
        let mut r = req(talk(8, &"長い回答です。".repeat(80)));
        r.explanation = "解説".repeat(3_000);
        let knowledge = format!("\n\n参考知識:\n- t: {}", "あ".repeat(500));
        let (system, history) = fit(&r, &knowledge);
        assert!(system.ends_with(&knowledge));
        let total: usize = estimate_tokens(&system)
            + history
                .iter()
                .map(|m| estimate_tokens(&m.content))
                .sum::<usize>();
        assert!(total <= PROMPT_BUDGET, "{total}");
    }

    #[test]
    fn fit_keeps_head_of_a_huge_previous_answer() {
        let r = req(talk(1, &"とても長い回答。".repeat(1_000)));
        let (_, history) = fit(&r, "");
        assert_eq!(history.len(), 3);
        assert!(history[1].content.ends_with('…'));
    }

    #[test]
    fn token_estimate_and_head() {
        assert_eq!(estimate_tokens("abcdef"), 2);
        assert_eq!(estimate_tokens("あいう"), 3);
        assert_eq!(head_within("abc", 10), "abc");
        let h = head_within(&"あ".repeat(100), 10);
        assert!(h.ends_with('…') && estimate_tokens(&h) <= 10, "{h}");
    }

    #[test]
    fn llm_messages_start_with_system() {
        let v = messages_json("sys", &talk(1, "a"));
        assert_eq!(v[0]["role"], "system");
        assert_eq!(v[1]["role"], "user");
        assert_eq!(v[2]["role"], "assistant");
        assert_eq!(v[3]["content"], "今の質問");
    }
}
