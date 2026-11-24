//! 左ペイン（#paper）の DOM 操作、テキスト選択の取得、ペイン境界のドラッグ、キーボードショートカット。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use paper_reader_shared::{Mode, normalize_whitespace};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, HtmlElement, KeyboardEvent, Node, PointerEvent};

const PAPER_ID: &str = "paper";
const LAYOUT_ID: &str = "layout";
const SPLITTER_ID: &str = "splitter";
const EXPLAIN_PANE_ID: &str = "explain-pane";
/// 論文ペインの幅（縦並びのときは高さ）を保存する localStorage のキー
const SPLIT_STORAGE_KEY: &str = "paper-reader.split";
/// 論文ペインの大きさの既定値と範囲（%）
const SPLIT_DEFAULT: f64 = 60.0;
const SPLIT_MIN: f64 = 20.0;
const SPLIT_MAX: f64 = 80.0;
/// 配色の設定を保存する localStorage のキー（index.html のスクリプトと合わせる）
const THEME_STORAGE_KEY: &str = "paper-reader.theme";
/// 縦並びに切り替わる画面幅（paper.css の @media と合わせる）
const STACKED_QUERY: &str = "(max-width: 800px)";
/// selectionchange のデバウンス時間
const DEBOUNCE_MS: i32 = 200;
/// 文脈として使う要素
const CONTEXT_SELECTOR: &str = "p, li, td";
/// p / li / td が無いときの代替（見出しや図のキャプションなど）
const FALLBACK_CONTEXT_SELECTOR: &str =
    "figcaption, h1, h2, h3, h4, h5, h6, blockquote, dd, th, .ltx_para";

/// 選択内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionInfo {
    pub text: String,
    /// 選択を含む段落のテキスト
    pub context: String,
    pub mode: Mode,
}

fn document() -> Option<Document> {
    web_sys::window()?.document()
}

fn paper_element() -> Option<Element> {
    document()?.get_element_by_id(PAPER_ID)
}

thread_local! {
    /// 最後に通知した選択。selectionchange と mouseup の重複を除くのに使う
    static LAST_SELECTION: RefCell<Option<SelectionInfo>> = const { RefCell::new(None) };
}

/// ブラウザの選択を解除する。同じ箇所をもう一度選んだときも通知されるようにする。
pub fn clear_selection() {
    LAST_SELECTION.with(|l| *l.borrow_mut() = None);
    if let Some(Ok(Some(sel))) = web_sys::window().map(|w| w.get_selection()) {
        let _ = sel.remove_all_ranges();
    }
}

/// 無害化済みの論文 HTML を #paper に入れる。
pub fn set_paper_html(html: &str, title: &str) {
    LAST_SELECTION.with(|l| *l.borrow_mut() = None);
    if let Some(paper) = paper_element() {
        paper.set_inner_html(html);
        paper.set_scroll_top(0);
    }
    if let Some(doc) = document() {
        doc.set_title(&format!("{title} - 論文リーダー"));
    }
}

/// eframe が起動できなかったときに #paper の上部にメッセージを出す。
pub fn show_fatal_error(message: &str) {
    if let (Some(doc), Some(paper)) = (document(), paper_element())
        && let Ok(div) = doc.create_element("div")
    {
        div.set_class_name("fatal-error");
        div.set_text_content(Some(message));
        let _ = paper.prepend_with_node_1(&div);
    }
}

/// 現在の選択が #paper 内にあれば、その内容を返す。
fn capture_selection() -> Option<SelectionInfo> {
    let paper = paper_element()?;
    let selection = web_sys::window()?.get_selection().ok()??;
    if selection.is_collapsed() || selection.range_count() == 0 {
        return None;
    }
    let range = selection.get_range_at(0).ok()?;
    let common = range.common_ancestor_container().ok()?;
    if !paper.contains(Some(&common)) {
        return None;
    }

    let text = normalize_whitespace(&String::from(selection.to_string()));
    if text.is_empty() {
        return None;
    }

    let start = range.start_container().ok()?;
    let context = element_of(&start)
        .and_then(|el| closest_in(&el, &paper, CONTEXT_SELECTOR))
        .or_else(|| {
            element_of(&start).and_then(|el| closest_in(&el, &paper, FALLBACK_CONTEXT_SELECTOR))
        })
        .and_then(|el| el.dyn_into::<HtmlElement>().ok())
        .map(|el| normalize_whitespace(&el.inner_text()))
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| text.clone());

    Some(SelectionInfo {
        mode: Mode::detect(&text),
        text,
        context,
    })
}

/// テキストノードなら親要素、要素ならそのもの。
fn element_of(node: &Node) -> Option<Element> {
    if node.node_type() == Node::ELEMENT_NODE {
        node.clone().dyn_into().ok()
    } else {
        node.parent_element()
    }
}

/// `selector` に一致する最も近い祖先のうち、#paper の内側にあるもの。
fn closest_in(el: &Element, paper: &Element, selector: &str) -> Option<Element> {
    el.closest(selector)
        .ok()
        .flatten()
        .filter(|found| found != paper && paper.contains(Some(found)))
}

/// 選択の監視を始める。新しい選択が確定するたびに `on_select` を呼ぶ。
/// リスナーはページの寿命と同じなので、クロージャは意図的に解放しない。
pub fn watch_selection(on_select: impl Fn(SelectionInfo) + 'static) -> Result<(), String> {
    let window = web_sys::window().ok_or("window が取得できません")?;
    let doc = window.document().ok_or("document が取得できません")?;
    let paper = paper_element().ok_or("#paper が見つかりません")?;

    let commit: Rc<dyn Fn()> = Rc::new(move || {
        let Some(info) = capture_selection() else {
            return;
        };
        // selectionchange と mouseup の両方から同じ選択が来るので重複を除く
        let is_new = LAST_SELECTION.with(|l| {
            let mut last = l.borrow_mut();
            if last.as_ref() == Some(&info) {
                false
            } else {
                *last = Some(info.clone());
                true
            }
        });
        if is_new {
            on_select(info);
        }
    });

    let pending: Rc<Cell<Option<i32>>> = Rc::default();

    let on_timeout = {
        let commit = commit.clone();
        let pending = pending.clone();
        Closure::<dyn FnMut()>::new(move || {
            pending.set(None);
            commit();
        })
    };

    let on_selectionchange = {
        let window = window.clone();
        let pending = pending.clone();
        Closure::<dyn FnMut()>::new(move || {
            if let Some(handle) = pending.take() {
                window.clear_timeout_with_handle(handle);
            }
            if let Ok(handle) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                on_timeout.as_ref().unchecked_ref(),
                DEBOUNCE_MS,
            ) {
                pending.set(Some(handle));
            }
        })
    };
    doc.add_event_listener_with_callback(
        "selectionchange",
        on_selectionchange.as_ref().unchecked_ref(),
    )
    .map_err(|_| "selectionchange を購読できません")?;
    on_selectionchange.forget();

    let on_mouseup = {
        let window = window.clone();
        Closure::<dyn FnMut()>::new(move || {
            if let Some(handle) = pending.take() {
                window.clear_timeout_with_handle(handle);
            }
            commit();
        })
    };
    paper
        .add_event_listener_with_callback("mouseup", on_mouseup.as_ref().unchecked_ref())
        .map_err(|_| "mouseup を購読できません")?;
    on_mouseup.forget();

    Ok(())
}

/// キーボードショートカット。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// E: 選択中のテキストを解説する
    Explain,
    /// Esc: 選択を解除する
    ClearSelection,
}

impl Shortcut {
    fn from_key(key: &str) -> Option<Self> {
        match key {
            "e" | "E" => Some(Self::Explain),
            "Escape" => Some(Self::ClearSelection),
            _ => None,
        }
    }
}

/// 論文ペイン側でのキー入力を監視する。
/// 右ペイン（egui）にフォーカスがあるときは egui 側で処理するので、ここでは無視する。
pub fn watch_shortcuts(on_key: impl Fn(Shortcut) + 'static) -> Result<(), String> {
    let doc = document().ok_or("document が取得できません")?;
    let on_keydown = {
        let doc = doc.clone();
        Closure::<dyn FnMut(KeyboardEvent)>::new(move |ev: KeyboardEvent| {
            if ev.ctrl_key() || ev.meta_key() || ev.alt_key() || ev.repeat() || ev.is_composing() {
                return;
            }
            let Some(shortcut) = Shortcut::from_key(&ev.key()) else {
                return;
            };
            if let Some(active) = doc.active_element() {
                let tag = active.tag_name();
                let in_explain_pane = active
                    .closest(&format!("#{EXPLAIN_PANE_ID}"))
                    .ok()
                    .flatten()
                    .is_some();
                let editable = active
                    .dyn_ref::<HtmlElement>()
                    .is_some_and(|el| el.is_content_editable());
                if in_explain_pane
                    || editable
                    || tag == "INPUT"
                    || tag == "TEXTAREA"
                    || tag == "SELECT"
                {
                    return;
                }
            }
            ev.prevent_default();
            on_key(shortcut);
        })
    };
    doc.add_event_listener_with_callback("keydown", on_keydown.as_ref().unchecked_ref())
        .map_err(|_| "keydown を購読できません")?;
    on_keydown.forget();
    Ok(())
}

/// ペイン境界（#splitter）のドラッグで論文ペインの大きさを変えられるようにする。
/// 大きさは CSS 変数 `--paper-size` で指定し、egui の canvas は ResizeObserver で追従する。
pub fn init_splitter() -> Result<(), String> {
    let window = web_sys::window().ok_or("window が取得できません")?;
    let doc = window.document().ok_or("document が取得できません")?;
    let layout: HtmlElement = doc
        .get_element_by_id(LAYOUT_ID)
        .and_then(|e| e.dyn_into().ok())
        .ok_or("#layout が見つかりません")?;
    let splitter: HtmlElement = doc
        .get_element_by_id(SPLITTER_ID)
        .and_then(|e| e.dyn_into().ok())
        .ok_or("#splitter が見つかりません")?;

    let storage = window.local_storage().ok().flatten();
    let saved = storage
        .as_ref()
        .and_then(|s| s.get_item(SPLIT_STORAGE_KEY).ok().flatten())
        .and_then(|v| v.parse::<f64>().ok());
    let current = Rc::new(Cell::new(clamp_split(saved.unwrap_or(SPLIT_DEFAULT))));
    apply_split(&layout, &splitter, current.get());

    let save = {
        let current = current.clone();
        move || {
            if let Some(s) = &storage {
                let _ = s.set_item(SPLIT_STORAGE_KEY, &format!("{:.1}", current.get()));
            }
        }
    };
    let set = {
        let layout = layout.clone();
        let splitter = splitter.clone();
        let current = current.clone();
        move |pct: f64| {
            let pct = clamp_split(pct);
            current.set(pct);
            apply_split(&layout, &splitter, pct);
        }
    };
    let stacked = {
        let window = window.clone();
        move || {
            window
                .match_media(STACKED_QUERY)
                .ok()
                .flatten()
                .is_some_and(|m| m.matches())
        }
    };

    let dragging = Rc::new(Cell::new(false));

    let on_down = {
        let dragging = dragging.clone();
        let splitter = splitter.clone();
        let doc = doc.clone();
        Closure::<dyn FnMut(PointerEvent)>::new(move |ev: PointerEvent| {
            if ev.button() != 0 {
                return;
            }
            ev.prevent_default();
            dragging.set(true);
            // ポインターが canvas や iframe の上に出てもイベントを受け取り続ける
            let _ = splitter.set_pointer_capture(ev.pointer_id());
            if let Some(body) = doc.body() {
                let _ = body.class_list().add_1("resizing");
            }
        })
    };
    let on_move = {
        let dragging = dragging.clone();
        let layout = layout.clone();
        let set = set.clone();
        let stacked = stacked.clone();
        Closure::<dyn FnMut(PointerEvent)>::new(move |ev: PointerEvent| {
            if !dragging.get() {
                return;
            }
            let rect = layout.get_bounding_client_rect();
            let pct = if stacked() {
                (f64::from(ev.client_y()) - rect.top()) / rect.height() * 100.0
            } else {
                (f64::from(ev.client_x()) - rect.left()) / rect.width() * 100.0
            };
            if pct.is_finite() {
                set(pct);
            }
        })
    };
    let on_up = {
        let dragging = dragging.clone();
        let save = save.clone();
        let doc = doc.clone();
        Closure::<dyn FnMut(PointerEvent)>::new(move |_ev: PointerEvent| {
            if !dragging.replace(false) {
                return;
            }
            if let Some(body) = doc.body() {
                let _ = body.class_list().remove_1("resizing");
            }
            save();
        })
    };
    let on_dblclick = {
        let set = set.clone();
        let save = save.clone();
        Closure::<dyn FnMut()>::new(move || {
            set(SPLIT_DEFAULT);
            save();
        })
    };
    // キーボードでも動かせるようにする（role="separator" と tabindex は index.html）
    let on_key = {
        let current = current.clone();
        Closure::<dyn FnMut(KeyboardEvent)>::new(move |ev: KeyboardEvent| {
            let delta = match ev.key().as_str() {
                "ArrowLeft" | "ArrowUp" => -2.0,
                "ArrowRight" | "ArrowDown" => 2.0,
                _ => return,
            };
            ev.prevent_default();
            ev.stop_propagation();
            set(current.get() + delta);
            save();
        })
    };

    for (name, cb) in [
        ("pointerdown", on_down.as_ref()),
        ("pointermove", on_move.as_ref()),
        ("pointerup", on_up.as_ref()),
        ("pointercancel", on_up.as_ref()),
        ("dblclick", on_dblclick.as_ref()),
        ("keydown", on_key.as_ref()),
    ] {
        splitter
            .add_event_listener_with_callback(name, cb.unchecked_ref())
            .map_err(|_| format!("{name} を購読できません"))?;
    }
    on_down.forget();
    on_move.forget();
    on_up.forget();
    on_dblclick.forget();
    on_key.forget();
    Ok(())
}

fn clamp_split(pct: f64) -> f64 {
    if pct.is_finite() {
        pct.clamp(SPLIT_MIN, SPLIT_MAX)
    } else {
        SPLIT_DEFAULT
    }
}

fn apply_split(layout: &HtmlElement, splitter: &HtmlElement, pct: f64) {
    let _ = layout
        .style()
        .set_property("--paper-size", &format!("{pct:.2}%"));
    let _ = splitter.set_attribute("aria-valuenow", &format!("{pct:.0}"));
}

/// 配色の設定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    /// OS の設定に従う
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// 保存済みの配色の設定（無ければ OS に従う）。
pub fn load_theme() -> ThemeChoice {
    let saved = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(THEME_STORAGE_KEY).ok().flatten());
    match saved.as_deref() {
        Some("light") => ThemeChoice::Light,
        Some("dark") => ThemeChoice::Dark,
        _ => ThemeChoice::System,
    }
}

/// 論文ペインの配色を切り替えて保存する（<html data-theme>。System なら属性を外して OS に従う）。
pub fn apply_theme(choice: ThemeChoice) {
    let Some(window) = web_sys::window() else {
        return;
    };
    if let Some(root) = window.document().and_then(|d| d.document_element()) {
        let _ = match choice {
            ThemeChoice::System => root.remove_attribute("data-theme"),
            ThemeChoice::Light | ThemeChoice::Dark => {
                root.set_attribute("data-theme", choice.as_str())
            }
        };
    }
    if let Some(storage) = window.local_storage().ok().flatten() {
        let _ = storage.set_item(THEME_STORAGE_KEY, choice.as_str());
    }
}
