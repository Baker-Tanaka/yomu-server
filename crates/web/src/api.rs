//! サーバー API の呼び出し（fetch）。

use paper_reader_shared::{
    DictResponse, ErrorResponse, ExplainRequest, GrammarMapResponse, HealthResponse, HistoryRecord,
    HistoryResponse, PaperResponse, SearchResponse, SearchSort, SseEvent, SseParser,
};
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

pub async fn fetch_grammar_map() -> Result<GrammarMapResponse, String> {
    get_json("/api/grammar-map").await
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
/// EventSource は GET しか使えないため、fetch + ReadableStream で読む。
/// `signal` が中止されると Err で戻る（呼び出し側で `signal.aborted()` を確認すること）。
pub async fn stream_explain(
    req: &ExplainRequest,
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
    let request =
        Request::new_with_str_and_init("/api/explain", &init).map_err(|e| js_error(&e))?;

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
