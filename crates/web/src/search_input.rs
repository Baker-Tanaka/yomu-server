//! 検索欄（arXiv の URL・ID・キーワード）。egui の入力欄ではなく、canvas の上に重ねた HTML の `<input>` を使う。
//!
//! canvas に描いた入力欄では、iPhone などで長押しのコピー・ペーストのメニューが出ないため。
//! 位置と表示の有無は egui のレイアウトに毎フレーム合わせる（`place` / `hide`）。

use std::cell::{Cell, RefCell};

use eframe::egui;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Event, HtmlInputElement, KeyboardEvent};

use crate::dom;

const ID: &str = "search-input";
/// canvas と同じ左上を原点にする親要素
const PARENT_ID: &str = "explain-pane";
/// 日本語入力の変換中に押したキーの keyCode（Safari は変換を確定する Enter もこの値で送る）
const KEYCODE_COMPOSING: u32 = 229;

thread_local! {
    static INPUT: RefCell<Option<HtmlInputElement>> = const { RefCell::new(None) };
    /// Enter が押された（次の描画で取り出して実行する）
    static SUBMITTED: Cell<bool> = const { Cell::new(false) };
    /// 直前に指定した位置（変わらなければ DOM を触らない）
    static PLACED: Cell<Option<egui::Rect>> = const { Cell::new(None) };
}

fn with_input<R>(f: impl FnOnce(&HtmlInputElement) -> R) -> Option<R> {
    INPUT.with(|i| i.borrow().as_ref().map(f))
}

/// 入力欄を作る（最初は隠しておく）。
/// リスナーはページの寿命と同じなので、クロージャは意図的に解放しない。
pub fn init(ctx: &egui::Context, placeholder: &str) -> Result<(), String> {
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("document が取得できません")?;
    let parent = doc
        .get_element_by_id(PARENT_ID)
        .ok_or("#explain-pane が見つかりません")?;
    let input: HtmlInputElement = doc
        .create_element("input")
        .ok()
        .and_then(|e| e.dyn_into().ok())
        .ok_or("入力欄を作れません")?;
    input.set_id(ID);
    input.set_type("text");
    for (name, value) in [
        ("lang", "ja"),
        ("placeholder", placeholder),
        ("aria-label", placeholder),
        // スマホのキーボードの確定キーを「検索」にし、勝手な大文字化・自動修正をしない
        ("enterkeyhint", "search"),
        ("autocapitalize", "off"),
        ("autocorrect", "off"),
        ("autocomplete", "off"),
        ("spellcheck", "false"),
    ] {
        let _ = input.set_attribute(name, value);
    }
    parent
        .append_child(&input)
        .map_err(|_| "入力欄を追加できません")?;

    let on_keydown = {
        let ctx = ctx.clone();
        let input = input.clone();
        Closure::<dyn FnMut(KeyboardEvent)>::new(move |ev: KeyboardEvent| {
            if ev.is_composing() || ev.key_code() == KEYCODE_COMPOSING {
                return;
            }
            match ev.key().as_str() {
                "Enter" => {
                    ev.prevent_default();
                    SUBMITTED.set(true);
                    let _ = input.blur();
                }
                "Escape" => {
                    let _ = input.blur();
                }
                _ => return,
            }
            ctx.request_repaint();
        })
    };
    input
        .add_event_listener_with_callback("keydown", on_keydown.as_ref().unchecked_ref())
        .map_err(|_| "keydown を購読できません")?;
    on_keydown.forget();

    // 入力に合わせて「開く／検索」ボタンを描き直す
    let on_input = {
        let ctx = ctx.clone();
        Closure::<dyn FnMut(Event)>::new(move |_: Event| ctx.request_repaint())
    };
    input
        .add_event_listener_with_callback("input", on_input.as_ref().unchecked_ref())
        .map_err(|_| "input を購読できません")?;
    on_input.forget();

    // スマホで入力するときは、キーボードに隠れないようシートを全体まで上げる
    let on_focus = {
        let ctx = ctx.clone();
        Closure::<dyn FnMut(Event)>::new(move |_: Event| {
            dom::raise_sheet(dom::Sheet::Full);
            ctx.request_repaint();
        })
    };
    input
        .add_event_listener_with_callback("focus", on_focus.as_ref().unchecked_ref())
        .map_err(|_| "focus を購読できません")?;
    on_focus.forget();

    INPUT.with(|i| *i.borrow_mut() = Some(input));
    Ok(())
}

/// egui の座標（`zoom` は egui の拡大率）の位置に重ねて表示する。
pub fn place(rect: egui::Rect, zoom: f32) {
    let rect = egui::Rect::from_min_max(
        (rect.min.to_vec2() * zoom).to_pos2(),
        (rect.max.to_vec2() * zoom).to_pos2(),
    );
    if PLACED.get() == Some(rect) {
        return;
    }
    PLACED.set(Some(rect));
    with_input(|input| {
        let style = input.style();
        for (name, value) in [
            ("left", rect.left()),
            ("top", rect.top()),
            ("width", rect.width()),
            ("height", rect.height()),
        ] {
            let _ = style.set_property(name, &format!("{value:.1}px"));
        }
        let _ = style.set_property("display", "block");
    });
}

/// 隠す（検索欄を出さない画面、メニューやモーダルが上に重なるとき）。
pub fn hide() {
    if PLACED.take().is_none() {
        return;
    }
    with_input(|input| {
        let _ = input.style().set_property("display", "none");
    });
}

pub fn value() -> String {
    with_input(HtmlInputElement::value).unwrap_or_default()
}

pub fn focus() {
    with_input(|input| {
        let _ = input.focus();
    });
}

/// Enter が押されていたら true（1 回だけ）。
pub fn take_submitted() -> bool {
    SUBMITTED.take()
}
