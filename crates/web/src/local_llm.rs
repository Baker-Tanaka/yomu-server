//! 実験モード: 端末（iPhone など）の GPU（WebGPU）で小さな LLM を動かし、Mac の LLM と比べる。
//!
//! - 既定はオフ。オフのあいだは assets/local_llm.js も WebLLM も読み込まない（最初の表示を遅くしない）
//! - オンにしたら、選んだモデルを Mac のサーバー（/webllm/）からダウンロードして読み込む
//! - 結果には、最初の文字までの時間・トークン/秒などの計測値を付ける
//! - 設定（オン/オフ・モデル）は localStorage に保存する

use std::cell::RefCell;

use eframe::egui;
use js_sys::{Array, Function, Promise, Reflect};
use paper_reader_shared::{Mode, clip_context};
use serde::Deserialize;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::dom;
use crate::state::SharedRef;

/// 実験モードのオン/オフを保存するキー
const ENABLED_KEY: &str = "paper-reader.local-llm";
/// 選んだモデルを保存するキー
const MODEL_KEY: &str = "paper-reader.local-llm-model";
/// 橋渡しの JS（trunk が dist にコピーする）
const GLUE_URL: &str = "/local_llm.js";
/// 既定のモデル（manifest に無ければ先頭）
const DEFAULT_MODEL: &str = "Qwen2.5-1.5B-Instruct-q4f16_1-MLC";
/// 単語の文脈として渡す最大文字数（端末の LLM はプロンプトの読み込みも遅いので短くする）
const WORD_CONTEXT_CHARS: usize = 300;

thread_local! {
    /// import() した local_llm.js のモジュール
    static GLUE: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// 端末の LLM の状態。
#[derive(Debug, Clone, PartialEq, Default)]
pub enum LocalPhase {
    #[default]
    Off,
    /// WebGPU が使えない
    Unsupported,
    /// モデルの一覧を読んでいる
    Preparing,
    /// ダウンロード・読み込み中（0〜1 と、WebLLM の説明）
    Loading {
        progress: f32,
        text: String,
    },
    /// 使える（読み込みにかかった秒数）
    Ready {
        load_secs: f64,
    },
    Error(String),
}

/// サーバーにある端末用のモデル（manifest.json）。
#[derive(Debug, Clone, Deserialize)]
pub struct LocalModel {
    pub id: String,
    pub label: String,
    pub note: String,
    pub vram_mb: u32,
    pub bytes: u64,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    models: Vec<LocalModel>,
}

/// 1 回の生成の計測値。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LocalStats {
    pub ttft_ms: f64,
    pub total_ms: f64,
    pub tokens: u32,
    pub decode_tps: f64,
    pub prefill_tps: f64,
}

/// 端末の LLM で訳した結果。
#[derive(Debug, Clone, Default)]
pub struct LocalRun {
    /// 訳した対象（モードと文字列）
    pub target: Option<(Mode, String)>,
    pub text: String,
    pub running: bool,
    pub stats: Option<LocalStats>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LocalLlm {
    pub enabled: bool,
    pub phase: LocalPhase,
    pub models: Vec<LocalModel>,
    pub model_id: String,
    pub run: LocalRun,
    /// オン/オフやモデルの切り替えごとに増える番号（古い読み込みの結果を捨てる）
    seq: u64,
}

impl LocalLlm {
    pub fn load_settings() -> Self {
        Self {
            enabled: dom::load_bool(ENABLED_KEY, false),
            model_id: load_string(MODEL_KEY).unwrap_or_default(),
            ..Self::default()
        }
    }

    pub fn current_model(&self) -> Option<&LocalModel> {
        self.models.iter().find(|m| m.id == self.model_id)
    }
}

/// 起動時: 前回オンにしていれば読み込みを始める（モデルはダウンロード済みならキャッシュから読む）。
pub fn resume(this: &SharedRef, ctx: &egui::Context) {
    if this.borrow().local.enabled {
        start(this, ctx);
    }
}

/// 実験モードを切り替える。オンにしたらモデルのダウンロード・読み込みを始め、オフならメモリから外す。
pub fn set_enabled(this: &SharedRef, ctx: &egui::Context, on: bool) {
    this.borrow_mut().local.enabled = on;
    dom::save_bool(ENABLED_KEY, on);
    if on {
        start(this, ctx);
    } else {
        stop(this);
    }
}

/// モデルを選び直す（読み込み直す）。
pub fn select_model(this: &SharedRef, ctx: &egui::Context, id: &str) {
    {
        let mut s = this.borrow_mut();
        if s.local.model_id == id {
            return;
        }
        s.local.model_id = id.to_string();
    }
    save_string(MODEL_KEY, id);
    start(this, ctx);
}

/// 選んでいるモデルを端末から消し、実験モードをオフにする。
pub fn delete_model(this: &SharedRef, ctx: &egui::Context) {
    let id = this.borrow().local.model_id.clone();
    set_enabled(this, ctx, false);
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = call("deleteModel", &[JsValue::from_str(&id)]).await;
        let mut s = this.borrow_mut();
        s.local.phase = match result {
            Ok(_) => LocalPhase::Off,
            Err(e) => LocalPhase::Error(format!("モデルを削除できません: {e}")),
        };
        ctx.request_repaint();
    });
}

fn stop(this: &SharedRef) {
    {
        let mut s = this.borrow_mut();
        s.local.seq += 1;
        s.local.phase = LocalPhase::Off;
        s.local.run = LocalRun::default();
    }
    wasm_bindgen_futures::spawn_local(async move {
        let _ = call("unload", &[]).await;
    });
}

fn start(this: &SharedRef, ctx: &egui::Context) {
    let seq = {
        let mut s = this.borrow_mut();
        s.local.seq += 1;
        s.local.phase = LocalPhase::Preparing;
        s.local.seq
    };
    ctx.request_repaint();
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = prepare_and_load(&this, &ctx, seq).await;
        let mut s = this.borrow_mut();
        if s.local.seq != seq {
            return;
        }
        s.local.phase = match result {
            Ok(phase) => phase,
            Err(e) => LocalPhase::Error(e),
        };
        ctx.request_repaint();
    });
}

async fn prepare_and_load(
    this: &SharedRef,
    ctx: &egui::Context,
    seq: u64,
) -> Result<LocalPhase, String> {
    if !call("supported", &[]).await?.is_truthy() {
        return Ok(LocalPhase::Unsupported);
    }
    let manifest: Manifest = from_js(&call("manifest", &[]).await?)?;
    if manifest.models.is_empty() {
        return Err("サーバーに端末用のモデルがありません".into());
    }
    let id = {
        let mut s = this.borrow_mut();
        if s.local.seq != seq {
            return Ok(LocalPhase::Off);
        }
        let saved = s.local.model_id.clone();
        let id = [saved.as_str(), DEFAULT_MODEL]
            .into_iter()
            .find(|id| manifest.models.iter().any(|m| m.id == *id))
            .map(str::to_string)
            .unwrap_or_else(|| manifest.models[0].id.clone());
        s.local.models = manifest.models;
        s.local.model_id = id.clone();
        s.local.phase = LocalPhase::Loading {
            progress: 0.0,
            text: String::new(),
        };
        id
    };
    ctx.request_repaint();

    let on_progress = {
        let this = this.clone();
        let ctx = ctx.clone();
        Closure::<dyn FnMut(f64, String)>::new(move |progress: f64, text: String| {
            let mut s = this.borrow_mut();
            if s.local.seq == seq {
                s.local.phase = LocalPhase::Loading {
                    progress: progress.clamp(0.0, 1.0) as f32,
                    text,
                };
                ctx.request_repaint();
            }
        })
    };
    let load_ms = call(
        "load",
        &[JsValue::from_str(&id), on_progress.as_ref().clone()],
    )
    .await?;
    // 読み込みが終わるまでコールバックを生かしておく
    drop(on_progress);
    Ok(LocalPhase::Ready {
        load_secs: load_ms.as_f64().unwrap_or(0.0) / 1000.0,
    })
}

/// 選択中の単語・英文を端末の LLM で訳す。
pub fn translate(this: &SharedRef, ctx: &egui::Context) {
    let (system, user, max_tokens, target) = {
        let mut s = this.borrow_mut();
        if !matches!(s.local.phase, LocalPhase::Ready { .. }) || s.local.run.running {
            return;
        }
        let Some(sel) = s.selection.clone() else {
            return;
        };
        let (system, user, max_tokens) = prompt(sel.mode, &sel.text, &sel.context);
        s.local.run = LocalRun {
            target: Some((sel.mode, sel.text.clone())),
            running: true,
            ..LocalRun::default()
        };
        (system, user, max_tokens, (sel.mode, sel.text))
    };
    ctx.request_repaint();
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let on_token = {
            let this = this.clone();
            let ctx = ctx.clone();
            let target = target.clone();
            Closure::<dyn FnMut(String)>::new(move |piece: String| {
                let mut s = this.borrow_mut();
                if s.local.run.target.as_ref() == Some(&target) {
                    s.local.run.text.push_str(&piece);
                    ctx.request_repaint();
                }
            })
        };
        let result = call(
            "generate",
            &[
                JsValue::from_str(&system),
                JsValue::from_str(&user),
                JsValue::from_f64(f64::from(max_tokens)),
                on_token.as_ref().clone(),
            ],
        )
        .await;
        drop(on_token);
        let mut s = this.borrow_mut();
        if s.local.run.target.as_ref() != Some(&target) {
            return;
        }
        s.local.run.running = false;
        match result.and_then(|v| from_js::<LocalStats>(&v)) {
            Ok(stats) => s.local.run.stats = Some(stats),
            Err(e) => s.local.run.error = Some(e),
        }
        ctx.request_repaint();
    });
}

/// 生成を止める。
pub fn interrupt() {
    wasm_bindgen_futures::spawn_local(async {
        let _ = call("interrupt", &[]).await;
    });
}

/// 端末の LLM に渡すプロンプト（system, user, 最大トークン数）。小さなモデル向けに短く、訳だけを出させる。
fn prompt(mode: Mode, text: &str, context: &str) -> (String, String, u32) {
    match mode {
        Mode::Word => (
            "あなたは英和辞書です。英語論文の文脈で、指定した英単語に合う日本語の訳語を1〜3個、「、」で区切って出力してください。訳語だけを書き、説明は書かないでください。".into(),
            format!(
                "単語: {text}\n文脈: {}",
                clip_context(context, text, WORD_CONTEXT_CHARS)
            ),
            40,
        ),
        Mode::Sentence => (
            "あなたは翻訳者です。英語論文の一文を、自然で正確な日本語に訳してください。訳文だけを出力してください。".into(),
            format!("英文: {text}"),
            400,
        ),
    }
}

/// local_llm.js の関数を呼ぶ（Promise なら待つ）。初回は import() でモジュールを読む。
async fn call(name: &str, args: &[JsValue]) -> Result<JsValue, String> {
    let module = glue().await?;
    let f: Function = Reflect::get(&module, &JsValue::from_str(name))
        .map_err(|e| js_error(&e))?
        .dyn_into()
        .map_err(|_| format!("local_llm.js に {name} がありません"))?;
    let args: Array = args.iter().collect();
    let value = f.apply(&JsValue::NULL, &args).map_err(|e| js_error(&e))?;
    match value.dyn_into::<Promise>() {
        Ok(p) => JsFuture::from(p).await.map_err(|e| js_error(&e)),
        Err(v) => Ok(v),
    }
}

async fn glue() -> Result<JsValue, String> {
    if let Some(m) = GLUE.with(|g| g.borrow().clone()) {
        return Ok(m);
    }
    let import = Function::new_with_args("u", "return import(u)");
    let promise: Promise = import
        .call1(&JsValue::NULL, &JsValue::from_str(GLUE_URL))
        .map_err(|e| js_error(&e))?
        .dyn_into()
        .map_err(|_| "import() が Promise を返しません".to_string())?;
    let module = JsFuture::from(promise)
        .await
        .map_err(|e| format!("実験モードの部品を読めません: {}", js_error(&e)))?;
    GLUE.with(|g| *g.borrow_mut() = Some(module.clone()));
    Ok(module)
}

fn from_js<T: for<'de> Deserialize<'de>>(v: &JsValue) -> Result<T, String> {
    let json = js_sys::JSON::stringify(v).map_err(|e| js_error(&e))?;
    serde_json::from_str(&String::from(json)).map_err(|e| e.to_string())
}

fn js_error(e: &JsValue) -> String {
    e.as_string()
        .or_else(|| {
            e.dyn_ref::<js_sys::Error>()
                .map(|err| String::from(err.message()))
        })
        .unwrap_or_else(|| format!("{e:?}"))
}

fn load_string(key: &str) -> Option<String> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(key).ok().flatten())
        .filter(|v| !v.is_empty())
}

fn save_string(key: &str, value: &str) {
    if let Some(s) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = s.set_item(key, value);
    }
}
