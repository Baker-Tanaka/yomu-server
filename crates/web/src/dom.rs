//! 左ペイン（#paper）の DOM 操作、テキスト選択の取得、ペイン境界のドラッグ、キーボードショートカット。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use paper_reader_shared::{Mode, normalize_whitespace};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, HtmlElement, KeyboardEvent, Node, PointerEvent, Text};

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
    /// 最後の選択を含む段落の要素（文法項目のハイライトで検索する範囲）
    static CONTEXT_ELEMENT: RefCell<Option<Element>> = const { RefCell::new(None) };
}

/// 文法項目のハイライトに使う要素のクラス
const HIGHLIGHT_CLASS: &str = "grammar-hl";

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
    CONTEXT_ELEMENT.with(|c| *c.borrow_mut() = None);
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
    let context_el = element_of(&start)
        .and_then(|el| closest_in(&el, &paper, CONTEXT_SELECTOR))
        .or_else(|| {
            element_of(&start).and_then(|el| closest_in(&el, &paper, FALLBACK_CONTEXT_SELECTOR))
        });
    CONTEXT_ELEMENT.with(|c| *c.borrow_mut() = context_el.clone());
    let context = context_el
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

/// スマホ用レイアウトの解説シートの高さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sheet {
    /// 見出しと入力欄だけ（本文を広く読む）
    Peek,
    /// 画面の半分
    Half,
    /// 上に本文を少し残して全体
    Full,
}

impl Sheet {
    fn as_str(self) -> &'static str {
        match self {
            Self::Peek => "peek",
            Self::Half => "half",
            Self::Full => "full",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "peek" => Some(Self::Peek),
            "half" => Some(Self::Half),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// シートの高さ（px、ホームインジケーターの余白は含まない）。
    fn height(self, layout_h: f64) -> f64 {
        let h = match self {
            Self::Peek => SHEET_PEEK_PX,
            Self::Half => layout_h * 0.5,
            Self::Full => sheet_max(layout_h),
        };
        h.clamp(SHEET_PEEK_PX, sheet_max(layout_h))
    }
}

/// シートを閉じたときの高さ（解説ペインの見出し + 入力欄が見える）
const SHEET_PEEK_PX: f64 = 96.0;
/// シートを全体にしたときに上に残す本文の高さ
const SHEET_FULL_GAP_PX: f64 = 64.0;
/// シートのつまみの高さ（paper.css の #splitter と合わせる）
const SPLITTER_PX: f64 = 26.0;

/// シートの高さの上限（つまみと、上に残す本文の分を引く）。
fn sheet_max(layout_h: f64) -> f64 {
    (layout_h - SPLITTER_PX - SHEET_FULL_GAP_PX).max(SHEET_PEEK_PX)
}
/// シートの状態を保存する localStorage のキー（PC の幅とは別に保存する）
const SHEET_STORAGE_KEY: &str = "paper-reader.sheet";
/// これより動かなければ「タップ」とみなす
const TAP_SLOP_PX: f64 = 6.0;

thread_local! {
    static SHEET: Cell<Sheet> = const { Cell::new(Sheet::Half) };
}

/// スマホ用レイアウト（上下に並ぶ）か。paper.css の @media と同じ条件。
pub fn is_mobile() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media(STACKED_QUERY).ok().flatten())
        .is_some_and(|m| m.matches())
}

pub fn sheet() -> Sheet {
    SHEET.with(Cell::get)
}

/// シートの高さを変えて保存する（スマホ用レイアウトのときだけ見た目が変わる）。
pub fn set_sheet(state: Sheet) {
    SHEET.with(|c| c.set(state));
    apply_sheet_px(None);
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(SHEET_STORAGE_KEY, state.as_str());
    }
}

/// スマホでシートが `min` より低ければ `min` まで上げる。
pub fn raise_sheet(min: Sheet) {
    if is_mobile() && sheet() < min {
        set_sheet(min);
    }
}

/// シートの高さを CSS 変数 `--sheet-h` に入れる。`px` が None なら今の状態の高さ。
fn apply_sheet_px(px: Option<f64>) {
    let Some(layout) = document()
        .and_then(|d| d.get_element_by_id(LAYOUT_ID))
        .and_then(|e| e.dyn_into::<HtmlElement>().ok())
    else {
        return;
    };
    let layout_h = f64::from(layout.client_height());
    if layout_h <= 0.0 {
        return;
    }
    let h = px
        .unwrap_or_else(|| sheet().height(layout_h))
        .clamp(SHEET_PEEK_PX, sheet_max(layout_h));
    let _ = layout
        .style()
        .set_property("--sheet-h", &format!("{h:.0}px"));
    if let Some(body) = document().and_then(|d| d.body()) {
        let _ = body.set_attribute("data-sheet", sheet().as_str());
    }
}

/// 指を離したときの高さに一番近い状態。
fn nearest_sheet(h: f64, layout_h: f64) -> Sheet {
    [Sheet::Peek, Sheet::Half, Sheet::Full]
        .into_iter()
        .min_by(|a, b| {
            (a.height(layout_h) - h)
                .abs()
                .total_cmp(&(b.height(layout_h) - h).abs())
        })
        .unwrap_or(Sheet::Half)
}

/// ペイン境界（#splitter）のドラッグでペインの大きさを変えられるようにする。
///
/// - PC 用（左右に並ぶ）: 論文ペインの幅を CSS 変数 `--paper-size`（%）で指定する
/// - スマホ用（上下に並ぶ）: 解説ペインを下からのシートにし、高さを `--sheet-h`（px）で指定する。
///   指を離すと「少し・半分・全体」の近い方にそろい、タップで「少し ⇄ 半分」を切り替える
///
/// どちらの大きさも別々に保存する。egui の canvas は ResizeObserver で追従する。
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
    if let Some(state) = storage
        .as_ref()
        .and_then(|s| s.get_item(SHEET_STORAGE_KEY).ok().flatten())
        .and_then(|v| Sheet::parse(&v))
    {
        SHEET.with(|c| c.set(state));
    }
    apply_sheet_px(None);

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

    // ドラッグ中の状態: 押した位置と、スマホでは押したときのシートの高さ
    let drag: Rc<Cell<Option<(f64, f64)>>> = Rc::default();
    let moved = Rc::new(Cell::new(0.0f64));

    let on_down = {
        let drag = drag.clone();
        let moved = moved.clone();
        let splitter = splitter.clone();
        let layout = layout.clone();
        let doc = doc.clone();
        Closure::<dyn FnMut(PointerEvent)>::new(move |ev: PointerEvent| {
            if ev.button() != 0 {
                return;
            }
            ev.prevent_default();
            let sheet_h = f64::from(layout.client_height())
                - f64::from(splitter.offset_top())
                - f64::from(splitter.offset_height());
            drag.set(Some((f64::from(ev.client_y()), sheet_h)));
            moved.set(0.0);
            // ポインターが canvas や iframe の上に出てもイベントを受け取り続ける
            let _ = splitter.set_pointer_capture(ev.pointer_id());
            if let Some(body) = doc.body() {
                let _ = body.class_list().add_1("resizing");
            }
        })
    };
    let on_move = {
        let drag = drag.clone();
        let moved = moved.clone();
        let layout = layout.clone();
        let set = set.clone();
        Closure::<dyn FnMut(PointerEvent)>::new(move |ev: PointerEvent| {
            let Some((start_y, start_h)) = drag.get() else {
                return;
            };
            let y = f64::from(ev.client_y());
            moved.set(moved.get().max((y - start_y).abs()));
            if is_mobile() {
                if moved.get() >= TAP_SLOP_PX {
                    apply_sheet_px(Some(start_h - (y - start_y)));
                }
            } else {
                let rect = layout.get_bounding_client_rect();
                let pct = (f64::from(ev.client_x()) - rect.left()) / rect.width() * 100.0;
                if pct.is_finite() {
                    set(pct);
                }
            }
        })
    };
    let on_up = {
        let drag = drag.clone();
        let moved = moved.clone();
        let save = save.clone();
        let layout = layout.clone();
        let doc = doc.clone();
        Closure::<dyn FnMut(PointerEvent)>::new(move |ev: PointerEvent| {
            let Some((start_y, start_h)) = drag.take() else {
                return;
            };
            if let Some(body) = doc.body() {
                let _ = body.class_list().remove_1("resizing");
            }
            if !is_mobile() {
                save();
                return;
            }
            if moved.get() < TAP_SLOP_PX {
                // タップ: 少し ⇄ 半分
                set_sheet(if sheet() == Sheet::Peek {
                    Sheet::Half
                } else {
                    Sheet::Peek
                });
                return;
            }
            let layout_h = f64::from(layout.client_height());
            let h = start_h - (f64::from(ev.client_y()) - start_y);
            set_sheet(nearest_sheet(h, layout_h));
        })
    };
    let on_dblclick = {
        let set = set.clone();
        let save = save.clone();
        Closure::<dyn FnMut()>::new(move || {
            if is_mobile() {
                return;
            }
            set(SPLIT_DEFAULT);
            save();
        })
    };
    // キーボードでも動かせるようにする（role="separator" と tabindex は index.html）
    let on_key = {
        let current = current.clone();
        Closure::<dyn FnMut(KeyboardEvent)>::new(move |ev: KeyboardEvent| {
            let key = ev.key();
            if is_mobile() {
                let next = match (key.as_str(), sheet()) {
                    ("ArrowUp", Sheet::Peek) => Sheet::Half,
                    ("ArrowUp", _) => Sheet::Full,
                    ("ArrowDown", Sheet::Full) => Sheet::Half,
                    ("ArrowDown", _) => Sheet::Peek,
                    _ => return,
                };
                ev.prevent_default();
                set_sheet(next);
                return;
            }
            let delta = match key.as_str() {
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
    // 画面の回転や PC 用 ⇄ スマホ用の切り替えで、シートの高さを測り直す
    let on_resize = Closure::<dyn FnMut()>::new(move || apply_sheet_px(None));
    window
        .add_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref())
        .map_err(|_| "resize を購読できません")?;
    on_resize.forget();

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

/// 文法項目のハイライト（<mark class="grammar-hl">）をすべて外す。
pub fn clear_highlight() {
    let Some(paper) = paper_element() else {
        return;
    };
    let Ok(marks) = paper.query_selector_all(&format!("mark.{HIGHLIGHT_CLASS}")) else {
        return;
    };
    for i in 0..marks.length() {
        let Some(mark) = marks.item(i) else {
            continue;
        };
        let Some(parent) = mark.parent_node() else {
            continue;
        };
        while let Some(child) = mark.first_child() {
            let _ = parent.insert_before(&child, Some(&mark));
        }
        let _ = parent.remove_child(&mark);
        // 分けたテキストノードをつなぎ直す（選択や再検索が正しく動くように）
        parent.normalize();
    }
}

/// 選択した英文の中から `span` を探して <mark> で囲み、見える位置までスクロールする。
/// 見つかれば true。
///
/// 探す範囲は最後に選択した段落（無ければ #paper 全体）。`anchor`（選択した英文）が見つかれば、
/// その位置から後ろを優先して探す。空白の違いと大文字小文字は無視する。
pub fn highlight_span(span: &str, anchor: &str) -> bool {
    clear_highlight();
    // ブラウザの選択の色がハイライトを覆うので、選択だけ外す（右ペインの選択内容はそのまま）
    if let Some(Ok(Some(sel))) = web_sys::window().map(|w| w.get_selection()) {
        let _ = sel.remove_all_ranges();
    }
    if span.trim().is_empty() {
        return false;
    }
    let root = CONTEXT_ELEMENT
        .with(|c| c.borrow().clone())
        .filter(|el| el.is_connected())
        .or_else(paper_element);
    let Some(root) = root else {
        return false;
    };

    let mut nodes = Vec::new();
    collect_text_nodes(&root, &mut nodes);
    // 空白をまとめ、小文字にした文字列と、その各文字がどのテキストノードの何文字目（UTF-16）かの対応
    let mut norm: Vec<char> = Vec::new();
    let mut map: Vec<(usize, u32, u32)> = Vec::new();
    let mut prev_space = true;
    for (i, node) in nodes.iter().enumerate() {
        let mut offset = 0u32;
        for c in node.data().chars() {
            let width = c.len_utf16() as u32;
            if c.is_whitespace() {
                if !prev_space {
                    norm.push(' ');
                    map.push((i, offset, width));
                    prev_space = true;
                }
            } else {
                norm.push(c.to_lowercase().next().unwrap_or(c));
                map.push((i, offset, width));
                prev_space = false;
            }
            offset += width;
        }
    }
    let needle = normalize_chars(span);
    let anchor = normalize_chars(anchor);
    let from = find_chars(&norm, &anchor, 0).unwrap_or(0);
    let Some(pos) = find_chars(&norm, &needle, from).or_else(|| find_chars(&norm, &needle, 0))
    else {
        return false;
    };
    let (start_node, start_off, _) = map[pos];
    let (end_node, last_off, last_width) = map[pos + needle.len() - 1];
    let end_off = last_off + last_width;

    let Some(doc) = document() else {
        return false;
    };
    let mut first_mark: Option<Element> = None;
    for (i, node) in nodes.iter().enumerate().take(end_node + 1).skip(start_node) {
        let s = if i == start_node { start_off } else { 0 };
        let e = if i == end_node {
            end_off
        } else {
            node.length()
        };
        if e <= s {
            continue;
        }
        if let Some(mark) = wrap_text(&doc, node, s, e) {
            first_mark.get_or_insert(mark);
        }
    }
    let Some(mark) = first_mark else {
        return false;
    };
    let opts = web_sys::ScrollIntoViewOptions::new();
    opts.set_block(web_sys::ScrollLogicalPosition::Center);
    opts.set_behavior(web_sys::ScrollBehavior::Smooth);
    mark.scroll_into_view_with_scroll_into_view_options(&opts);
    true
}

/// テキストノードの [start, end)（UTF-16）を <mark> で囲む。
fn wrap_text(doc: &Document, node: &Text, start: u32, end: u32) -> Option<Element> {
    let target = if start > 0 {
        node.split_text(start).ok()?
    } else {
        node.clone()
    };
    if end - start < target.length() {
        target.split_text(end - start).ok()?;
    }
    if target.data().trim().is_empty() {
        return None;
    }
    let mark = doc.create_element("mark").ok()?;
    mark.set_class_name(HIGHLIGHT_CLASS);
    let parent = target.parent_node()?;
    parent.replace_child(&mark, &target).ok()?;
    mark.append_child(&target).ok()?;
    Some(mark)
}

/// 要素の中のテキストノードを文書順に集める（数式の注釈など、表示されないものは除く）。
fn collect_text_nodes(node: &Node, out: &mut Vec<Text>) {
    let children = node.child_nodes();
    for i in 0..children.length() {
        let Some(child) = children.item(i) else {
            continue;
        };
        match child.node_type() {
            Node::TEXT_NODE => {
                if let Ok(text) = child.dyn_into::<Text>() {
                    out.push(text);
                }
            }
            Node::ELEMENT_NODE => {
                let skip = child.dyn_ref::<Element>().is_some_and(|el| {
                    matches!(
                        el.tag_name().to_ascii_lowercase().as_str(),
                        "annotation" | "annotation-xml" | "script" | "style"
                    )
                });
                if !skip {
                    collect_text_nodes(&child, out);
                }
            }
            _ => {}
        }
    }
}

fn normalize_chars(s: &str) -> Vec<char> {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

fn find_chars(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == *needle)
}

/// localStorage に保存した真偽値の設定。
pub fn load_bool(key: &str, default: bool) -> bool {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(key).ok().flatten())
        .map(|v| v == "true")
        .unwrap_or(default)
}

pub fn save_bool(key: &str, value: bool) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(key, if value { "true" } else { "false" });
    }
}

/// 発音の読み上げ（Web Speech API）に対応しているか。
pub fn can_speak() -> bool {
    web_sys::window().is_some_and(|w| w.speech_synthesis().is_ok())
}

/// 英単語・英文をブラウザの音声合成で読み上げる。前の読み上げは止める。
///
/// iOS Safari ではユーザー操作の中で呼ぶ必要がある（eframe は pointerup の中で
/// アプリの処理を走らせるので、ボタンの clicked() から呼べばよい）。
/// 端末内の音声を使うのでネットワークを待たずにすぐ鳴る。
pub fn speak(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let Some(synth) = web_sys::window().and_then(|w| w.speech_synthesis().ok()) else {
        return false;
    };
    let Ok(utterance) = web_sys::SpeechSynthesisUtterance::new_with_text(text) else {
        return false;
    };
    utterance.set_lang("en-US");
    // 単語を聞き取りやすいよう少しゆっくり
    utterance.set_rate(0.9);
    synth.cancel();
    synth.speak(&utterance);
    true
}
// ───────────── 付箋（どこまで読んだか） ─────────────

/// 付箋を貼る単位にする本文のブロック要素。サーバーで無害化した HTML は同じ論文なら同じなので、
/// この順の通し番号で位置を表せる（版が変わってずれたら書き出しで探し直す）。
const BLOCK_SELECTOR: &str = "p, li, h1, h2, h3, h4, h5, h6, figcaption, blockquote, dd";
const HEADING_SELECTOR: &str = "h1, h2, h3, h4, h5, h6";
const BOOKMARK_CLASS: &str = "pr-bookmark";
/// 付箋の書き出しの照合に使う文字数
const SNIPPET_MATCH_CHARS: usize = 40;

fn blocks(paper: &Element) -> Vec<Element> {
    let Ok(list) = paper.query_selector_all(BLOCK_SELECTOR) else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|i| list.item(i))
        .filter_map(|n| n.dyn_into::<Element>().ok())
        .collect()
}

fn block_snippet(el: &Element) -> String {
    normalize_whitespace(&el.text_content().unwrap_or_default())
        .chars()
        .take(paper_reader_shared::MAX_BOOKMARK_SNIPPET_CHARS)
        .collect()
}

fn snippet_key(s: &str) -> String {
    s.chars().take(SNIPPET_MATCH_CHARS).collect()
}

/// 付箋を貼る位置（ブロックの番号と書き出し）。
/// 本文で選択していて、その段落が見えていればそこ。無ければ、いま画面のいちばん上に見えている段落
/// （章・節の見出しは読んだ位置が分かりにくいので、ほかに見えている段落があれば選ばない）。
pub fn reading_block() -> Option<(u32, String)> {
    let paper = paper_element()?;
    let list = blocks(&paper);
    if list.is_empty() {
        return None;
    }
    let view = paper.get_bounding_client_rect();
    let visible = |el: &Element| {
        let r = el.get_bounding_client_rect();
        r.height() > 0.0 && r.bottom() > view.top() + 4.0 && r.top() < view.bottom()
    };

    let selected = web_sys::window()
        .and_then(|w| w.get_selection().ok().flatten())
        .filter(|s| s.range_count() > 0 && !s.is_collapsed())
        .and_then(|s| s.anchor_node())
        .and_then(|n| element_of(&n))
        .and_then(|el| closest_in(&el, &paper, BLOCK_SELECTOR))
        .filter(|el| visible(el))
        .and_then(|el| list.iter().position(|b| *b == el));
    let heading = |el: &Element| el.matches(HEADING_SELECTOR).unwrap_or(false);
    let index = selected
        .or_else(|| list.iter().position(|b| visible(b) && !heading(b)))
        .or_else(|| list.iter().position(visible))?;
    let el = &list[index];
    Some((u32::try_from(index).ok()?, block_snippet(el)))
}

/// 付箋の位置へのスクロールのしかた。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkScroll {
    /// 印を付けるだけ
    None,
    /// すぐに移る（論文を開いたとき）
    Instant,
    /// なめらかに移る（「付箋へ移動」）
    Smooth,
}

/// 付箋の印を本文に付ける（前の印は外す）。見つからなければ false。
pub fn show_bookmark(block: u32, snippet: &str, scroll: BookmarkScroll) -> bool {
    clear_bookmark();
    let Some(paper) = paper_element() else {
        return false;
    };
    let list = blocks(&paper);
    let key = snippet_key(snippet);
    let matches = |el: &Element| key.is_empty() || snippet_key(&block_snippet(el)) == key;
    let target = list
        .get(block as usize)
        .filter(|el| matches(el))
        .or_else(|| list.iter().find(|el| matches(el)));
    let Some(el) = target else {
        return false;
    };
    let _ = el.class_list().add_1(BOOKMARK_CLASS);
    let behavior = match scroll {
        BookmarkScroll::None => return true,
        BookmarkScroll::Instant => web_sys::ScrollBehavior::Instant,
        BookmarkScroll::Smooth => web_sys::ScrollBehavior::Smooth,
    };
    let opts = web_sys::ScrollIntoViewOptions::new();
    opts.set_block(web_sys::ScrollLogicalPosition::Start);
    opts.set_behavior(behavior);
    el.scroll_into_view_with_scroll_into_view_options(&opts);
    true
}

/// 付箋の印を外す。
pub fn clear_bookmark() {
    let Some(paper) = paper_element() else {
        return;
    };
    if let Ok(list) = paper.query_selector_all(&format!(".{BOOKMARK_CLASS}")) {
        for i in 0..list.length() {
            if let Some(el) = list.item(i).and_then(|n| n.dyn_into::<Element>().ok()) {
                let _ = el.class_list().remove_1(BOOKMARK_CLASS);
            }
        }
    }
}
