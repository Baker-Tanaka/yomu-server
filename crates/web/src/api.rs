//! サーバー API の呼び出し（fetch）。

use paper_reader_shared::{DictResponse, ErrorResponse, PaperResponse};
use serde::de::DeserializeOwned;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::Response;

pub async fn fetch_paper(url: &str) -> Result<PaperResponse, String> {
    let encoded = String::from(js_sys::encode_uri_component(url));
    get_json(&format!("/api/paper?url={encoded}")).await
}

pub async fn fetch_dict(word: &str) -> Result<DictResponse, String> {
    let encoded = String::from(js_sys::encode_uri_component(word));
    get_json(&format!("/api/dict?word={encoded}")).await
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
