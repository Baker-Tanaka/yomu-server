//! サーバー API の呼び出し（fetch）。

use paper_reader_shared::{
    ChatRequest, DictResponse, ErrorResponse, ExplainRequest, FeedbackDraft, FeedbackItem,
    FeedbackListResponse, FeedbackPreview, FeedbackSubmit, GrammarTagsResponse, HealthResponse,
    HistoryRecord, HistoryResponse, PaperResponse, SearchResponse, SearchSort, SseEvent, SseParser,
    WordDelete, WordDeleteResult, WordListResponse, WordNote, WordPrune, WordSort, WordUpsert,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AbortSignal, Headers, ReadableStreamDefaultReader, Request, RequestInit, Response,
    TextDecodeOptions, TextDecoder,
};

pub async fn fetch_paper(url: &str) -> Result<PaperResponse, String> {
    let encoded = String::from(js_sys::encode_uri_component(url));
    get_json(&format!("/api/paper?url={encoded}")).await
}

pub async fn fetch_dict(word: &str) -> Result<DictResponse, String> {
    let encoded = String::from(js_sys::encode_uri_component(word));
    get_json(&format!("/api/dict?word={encoded}")).await
}

/// 辞書に無い語の訳と、カタカナの読みを AI に推定させる（単語帳に保存される）。
pub async fn fetch_dict_ai(
    req: &paper_reader_shared::DictAiRequest,
    signal: &AbortSignal,
) -> Result<DictResponse, String> {
    post_json_with_signal("/api/dict/ai", Some(req), Some(signal)).await
}

pub async fn fetch_words(q: &str, sort: WordSort, offset: i64) -> Result<WordListResponse, String> {
    let encoded = String::from(js_sys::encode_uri_component(q));
    let sort = match sort {
        WordSort::Count => "count",
        WordSort::Recent => "recent",
        WordSort::Alpha => "alpha",
    };
    get_json(&format!(
        "/api/words?q={encoded}&sort={sort}&offset={offset}"
    ))
    .await
}

pub async fn save_word(u: &WordUpsert) -> Result<WordNote, String> {
    post_json("/api/words", Some(u)).await
}

pub async fn delete_words(words: Vec<String>) -> Result<WordDeleteResult, String> {
    post_json("/api/words/delete", Some(&WordDelete { words })).await
}

pub async fn prune_words(count: i64) -> Result<WordDeleteResult, String> {
    post_json("/api/words/prune", Some(&WordPrune { count })).await
}

pub async fn fetch_search(q: &str, start: u32, sort: SearchSort) -> Result<SearchResponse, String> {
    let encoded = String::from(js_sys::encode_uri_component(q));
    let sort = match sort {
        SearchSort::Relevance => "relevance",
        SearchSort::Newest => "newest",
    };
    get_json(&format!(
        "/api/search?q={encoded}&start={start}&sort={sort}"
    ))
    .await
}

pub async fn fetch_feedback() -> Result<FeedbackListResponse, String> {
    get_json("/api/feedback").await
}

pub async fn preview_feedback(draft: &FeedbackDraft) -> Result<FeedbackPreview, String> {
    post_json("/api/feedback/preview", Some(draft)).await
}

pub async fn submit_feedback(sub: &FeedbackSubmit) -> Result<FeedbackItem, String> {
    post_json("/api/feedback", Some(sub)).await
}

pub async fn retry_feedback(id: i64) -> Result<FeedbackItem, String> {
    post_json::<FeedbackItem, ()>(&format!("/api/feedback/{id}/retry"), None).await
}

pub async fn fetch_knowledge() -> Result<paper_reader_shared::KnowledgeListResponse, String> {
    get_json("/api/knowledge").await
}

pub async fn add_knowledge(
    req: &paper_reader_shared::KnowledgeAdd,
) -> Result<paper_reader_shared::KnowledgeItem, String> {
    post_json("/api/knowledge", Some(req)).await
}

pub async fn delete_knowledge(id: i64) -> Result<(), String> {
    send_without_body("DELETE", &format!("/api/knowledge?id={id}"), None).await
}

/// JSON を POST して JSON を受け取る。
async fn post_json<T: DeserializeOwned, B: Serialize>(
    path: &str,
    body: Option<&B>,
) -> Result<T, String> {
    post_json_with_signal(path, body, None).await
}

/// JSON を POST して JSON を受け取る（`signal` で中止できる）。
async fn post_json_with_signal<T: DeserializeOwned, B: Serialize>(
    path: &str,
    body: Option<&B>,
    signal: Option<&AbortSignal>,
) -> Result<T, String> {
    let window = web_sys::window().ok_or("window が取得できません")?;
    let init = RequestInit::new();
    init.set_method("POST");
    if let Some(signal) = signal {
        init.set_signal(Some(signal));
    }
    if let Some(body) = body {
        let json = serde_json::to_string(body).map_err(|e| e.to_string())?;
        let headers = Headers::new().map_err(|e| js_error(&e))?;
        headers
            .set("Content-Type", "application/json")
            .map_err(|e| js_error(&e))?;
        init.set_headers(&headers);
        init.set_body(&JsValue::from_str(&json));
    }
    let request = Request::new_with_str_and_init(path, &init).map_err(|e| js_error(&e))?;
    let resp: Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("サーバーに接続できません（{}）", js_error(&e)))?
        .dyn_into()
        .map_err(|_| "不正な応答です".to_string())?;
    let text = JsFuture::from(resp.text().map_err(|e| js_error(&e))?)
        .await
        .map_err(|e| js_error(&e))?
        .as_string()
        .unwrap_or_default();
    if resp.ok() {
        serde_json::from_str(&text).map_err(|e| format!("応答を解釈できません: {e}"))
    } else {
        match serde_json::from_str::<ErrorResponse>(&text) {
            Ok(err) => Err(err.error),
            Err(_) => Err(format!("サーバーエラー（HTTP {}）", resp.status())),
        }
    }
}

pub async fn fetch_papers() -> Result<paper_reader_shared::RecentPapersResponse, String> {
    get_json("/api/papers").await
}

/// 見た論文から外す。`purge` なら本文と図のキャッシュも消す。
pub async fn delete_paper(id: &str, purge: bool) -> Result<(), String> {
    let encoded = String::from(js_sys::encode_uri_component(id));
    send_without_body(
        "DELETE",
        &format!("/api/papers?id={encoded}&purge={purge}"),
        None,
    )
    .await
}

/// 付箋を貼る（貼り直したら置き換える）。
pub async fn put_bookmark(req: &paper_reader_shared::BookmarkPut) -> Result<(), String> {
    let body = serde_json::to_string(req).map_err(|e| e.to_string())?;
    send_without_body("PUT", "/api/bookmark", Some(&body)).await
}

pub async fn delete_bookmark(paper_id: &str) -> Result<(), String> {
    let encoded = String::from(js_sys::encode_uri_component(paper_id));
    send_without_body("DELETE", &format!("/api/bookmark?paper_id={encoded}"), None).await
}

pub async fn fetch_read_later() -> Result<paper_reader_shared::ReadLaterResponse, String> {
    get_json("/api/read-later").await
}

/// 後で読むに積む（すでにあれば一番上に移す）。
pub async fn put_read_later(req: &paper_reader_shared::ReadLaterPut) -> Result<(), String> {
    let body = serde_json::to_string(req).map_err(|e| e.to_string())?;
    send_without_body("PUT", "/api/read-later", Some(&body)).await
}

pub async fn delete_read_later(paper_id: &str) -> Result<(), String> {
    let encoded = String::from(js_sys::encode_uri_component(paper_id));
    send_without_body(
        "DELETE",
        &format!("/api/read-later?paper_id={encoded}"),
        None,
    )
    .await
}

pub async fn fetch_grammar_tags() -> Result<GrammarTagsResponse, String> {
    get_json("/api/grammar-tags").await
}

pub async fn fetch_health() -> Result<HealthResponse, String> {
    get_json("/api/health").await
}

/// 履歴を取得する。`paper` が `None` なら全論文。
pub async fn fetch_history(paper: Option<&str>) -> Result<HistoryResponse, String> {
    match paper {
        Some(p) => {
            let encoded = String::from(js_sys::encode_uri_component(p));
            get_json(&format!("/api/history?paper={encoded}")).await
        }
        None => get_json("/api/history").await,
    }
}

pub async fn post_history(record: &HistoryRecord) -> Result<(), String> {
    let body = serde_json::to_string(record).map_err(|e| e.to_string())?;
    send_without_body("POST", "/api/history", Some(&body)).await
}

pub async fn delete_history(id: i64) -> Result<(), String> {
    send_without_body("DELETE", &format!("/api/history/{id}"), None).await
}

/// 応答の本文を使わないリクエスト（成功なら `Ok(())`）。
async fn send_without_body(method: &str, path: &str, json: Option<&str>) -> Result<(), String> {
    let window = web_sys::window().ok_or("window が取得できません")?;
    let init = RequestInit::new();
    init.set_method(method);
    if let Some(json) = json {
        let headers = Headers::new().map_err(|e| js_error(&e))?;
        headers
            .set("Content-Type", "application/json")
            .map_err(|e| js_error(&e))?;
        init.set_headers(&headers);
        init.set_body(&JsValue::from_str(json));
    }
    let request = Request::new_with_str_and_init(path, &init).map_err(|e| js_error(&e))?;
    let resp: Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("サーバーに接続できません（{}）", js_error(&e)))?
        .dyn_into()
        .map_err(|_| "不正な応答です".to_string())?;
    if resp.ok() {
        return Ok(());
    }
    let text = match resp.text() {
        Ok(p) => JsFuture::from(p)
            .await
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    Err(match serde_json::from_str::<ErrorResponse>(&text) {
        Ok(err) => err.error,
        Err(_) => format!("サーバーエラー（HTTP {}）", resp.status()),
    })
}

/// `POST /api/explain` を呼び、SSE のイベントを受け取るたびに `on_event` を呼ぶ。
pub async fn stream_explain(
    req: &ExplainRequest,
    signal: &AbortSignal,
    on_event: impl FnMut(SseEvent),
) -> Result<(), String> {
    stream_sse("/api/explain", req, signal, on_event).await
}

/// `POST /api/chat`（解説のあとの質問）。受け取り方は解説と同じ。
pub async fn stream_chat(
    req: &ChatRequest,
    signal: &AbortSignal,
    on_event: impl FnMut(SseEvent),
) -> Result<(), String> {
    stream_sse("/api/chat", req, signal, on_event).await
}

/// JSON を POST して、SSE のイベントを受け取るたびに `on_event` を呼ぶ。
/// EventSource は GET しか使えないため、fetch + ReadableStream で読む。
/// `signal` が中止されると Err で戻る（呼び出し側で `signal.aborted()` を確認すること）。
async fn stream_sse<B: Serialize>(
    path: &str,
    req: &B,
    signal: &AbortSignal,
    mut on_event: impl FnMut(SseEvent),
) -> Result<(), String> {
    let window = web_sys::window().ok_or("window が取得できません")?;
    let body = serde_json::to_string(req).map_err(|e| e.to_string())?;

    let headers = Headers::new().map_err(|e| js_error(&e))?;
    headers
        .set("Content-Type", "application/json")
        .map_err(|e| js_error(&e))?;
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_headers(&headers);
    init.set_body(&JsValue::from_str(&body));
    init.set_signal(Some(signal));
    let request = Request::new_with_str_and_init(path, &init).map_err(|e| js_error(&e))?;

    let resp: Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("サーバーに接続できません（{}）", js_error(&e)))?
        .dyn_into()
        .map_err(|_| "不正な応答です".to_string())?;

    if !resp.ok() {
        let text = match resp.text() {
            Ok(p) => JsFuture::from(p)
                .await
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
        return Err(match serde_json::from_str::<ErrorResponse>(&text) {
            Ok(err) => err.error,
            Err(_) => format!("サーバーエラー（HTTP {}）", resp.status()),
        });
    }

    let stream = resp.body().ok_or("応答の本文がありません")?;
    let reader: ReadableStreamDefaultReader = stream
        .get_reader()
        .dyn_into()
        .map_err(|_| "ストリームを読み込めません".to_string())?;
    let decoder = TextDecoder::new_with_label("utf-8").map_err(|e| js_error(&e))?;
    let opts = TextDecodeOptions::new();
    opts.set_stream(true);
    let mut parser = SseParser::default();

    loop {
        let chunk = JsFuture::from(reader.read())
            .await
            .map_err(|e| format!("受信に失敗しました（{}）", js_error(&e)))?;
        let done = js_sys::Reflect::get(&chunk, &JsValue::from_str("done"))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if done {
            break;
        }
        let value: js_sys::Uint8Array = js_sys::Reflect::get(&chunk, &JsValue::from_str("value"))
            .map_err(|e| js_error(&e))?
            .dyn_into()
            .map_err(|_| "不正なデータを受信しました".to_string())?;
        let text = decoder
            .decode_with_js_u8_array_and_options(&value, &opts)
            .map_err(|e| js_error(&e))?;
        for ev in parser.push(&text) {
            on_event(ev);
        }
    }
    Ok(())
}

async fn get_json<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    let window = web_sys::window().ok_or("window が取得できません")?;
    let resp: Response = JsFuture::from(window.fetch_with_str(path))
        .await
        .map_err(|e| format!("サーバーに接続できません（{}）", js_error(&e)))?
        .dyn_into()
        .map_err(|_| "不正な応答です".to_string())?;
    let text_promise = resp.text().map_err(|e| js_error(&e))?;
    let text = JsFuture::from(text_promise)
        .await
        .map_err(|e| js_error(&e))?
        .as_string()
        .unwrap_or_default();

    if resp.ok() {
        serde_json::from_str(&text).map_err(|e| format!("応答を解釈できません: {e}"))
    } else {
        match serde_json::from_str::<ErrorResponse>(&text) {
            Ok(err) => Err(err.error),
            Err(_) => Err(format!("サーバーエラー（HTTP {}）", resp.status())),
        }
    }
}

fn js_error(e: &JsValue) -> String {
    e.as_string()
        .or_else(|| {
            e.dyn_ref::<js_sys::Error>()
                .map(|err| String::from(err.message()))
        })
        .unwrap_or_else(|| format!("{e:?}"))
}
