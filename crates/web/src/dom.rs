//! 左ペイン（#paper）の DOM 操作、テキスト選択の取得、ペイン境界のドラッグ、キーボードショートカット。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use paper_reader_shared::{Mode, normalize_whitespace};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{
    Document, DomRect, Element, HtmlElement, KeyboardEvent, Node, PointerEvent, Range,
    ResizeObserver, Text,
};

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
    /// 最後の選択の範囲（付箋を貼る行に使う）
    static LAST_RANGE: RefCell<Option<Range>> = const { RefCell::new(None) };
    /// 本文に付けている付箋の印
    static BOOKMARK: RefCell<Option<BookmarkLine>> = const { RefCell::new(None) };
    /// タップ（クリック）で選んでいる数式の要素と LaTeX
    static FORMULA: RefCell<Option<(Element, String)>> = const { RefCell::new(None) };
}

/// タップで選んだ数式に付ける印のクラス
const FORMULA_CLASS: &str = "pr-formula";

/// 文法項目のハイライトに使う要素のクラス
const HIGHLIGHT_CLASS: &str = "grammar-hl";

/// 本文（#paper）の中に、空でない選択があるか。
pub fn has_paper_selection() -> bool {
    paper_selection_range().is_some()
}

/// 本文の選択の画面上の位置と、本文の表示範囲。
pub fn selection_rect() -> Option<(DomRect, DomRect)> {
    let range = paper_selection_range()?;
    let area = paper_element()?.get_bounding_client_rect();
    Some((range.get_bounding_client_rect(), area))
}

/// 本文の選択。ブラウザの選択が無ければ、タップで選んだ数式。
fn paper_selection_range() -> Option<Range> {
    native_selection_range().or_else(formula_range)
}

fn formula_range() -> Option<Range> {
    let (el, _) = FORMULA.with(|f| f.borrow().clone())?;
    if !el.is_connected() {
        return None;
    }
    let range = document()?.create_range().ok()?;
    range.select_node(&el).ok()?;
    Some(range)
}

/// タップで選んだ数式をやめる。選んでいたら true。
fn clear_formula() -> bool {
    let Some((el, _)) = FORMULA.with(|f| f.borrow_mut().take()) else {
        return false;
    };
    let _ = el.class_list().remove_1(FORMULA_CLASS);
    true
}

fn select_formula(el: Element, tex: String) {
    clear_formula();
    let _ = el.class_list().add_1(FORMULA_CLASS);
    FORMULA.with(|f| *f.borrow_mut() = Some((el, tex)));
}

/// 最後に通知した選択の種類。
pub fn selected_mode() -> Option<Mode> {
    LAST_SELECTION.with(|l| l.borrow().as_ref().map(|s| s.mode))
}

fn native_selection_range() -> Option<Range> {
    let selection = web_sys::window()?.get_selection().ok()??;
    if selection.is_collapsed() || selection.range_count() == 0 {
        return None;
    }
    let range = selection.get_range_at(0).ok()?;
    let common = range.common_ancestor_container().ok()?;
    paper_element()?.contains(Some(&common)).then_some(range)
}

/// ブラウザの選択を解除する。同じ箇所をもう一度選んだときも通知されるようにする。
pub fn clear_selection() {
    crate::selection_menu::hide();
    LAST_SELECTION.with(|l| *l.borrow_mut() = None);
    LAST_RANGE.with(|r| *r.borrow_mut() = None);
    clear_formula();
    if let Some(Ok(Some(sel))) = web_sys::window().map(|w| w.get_selection()) {
        let _ = sel.remove_all_ranges();
    }
}

/// 無害化済みの論文 HTML を #paper に入れる。
pub fn set_paper_html(html: &str, title: &str) {
    crate::selection_menu::hide();
    crate::cite_peek::close();
    clear_formula();
    LAST_SELECTION.with(|l| *l.borrow_mut() = None);
    CONTEXT_ELEMENT.with(|c| *c.borrow_mut() = None);
    LAST_RANGE.with(|r| *r.borrow_mut() = None);
    // 前の論文の付箋の印（大きさの監視）を止める
    clear_bookmark();
    if let Some(paper) = paper_element() {
        paper.set_inner_html(html);
        paper.set_scroll_top(0);
    }
    crate::toc::refresh();
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

/// 現在の選択が #paper 内にあれば、その内容を返す。ブラウザの選択が無ければ、タップで選んだ数式。
fn capture_selection() -> Option<SelectionInfo> {
    let paper = paper_element()?;
    let Some(range) = native_selection_range() else {
        let (el, tex) = FORMULA.with(|f| f.borrow().clone())?;
        let range = formula_range()?;
        LAST_RANGE.with(|r| *r.borrow_mut() = Some(range));
        return Some(formula_info(&el, &paper, tex));
    };
    // 本文を選び直したら、タップで選んだ数式はやめる
    clear_formula();
    // 表示されていない文字を含めないよう、Range ではなく Selection の文字列を使う
    let selection = web_sys::window()?.get_selection().ok()??;
    let plain = normalize_whitespace(&String::from(selection.to_string()));
    let start = element_of(&range.start_container().ok()?)?;
    let (text, mode) = match crate::formula::selected(&range) {
        Some(crate::formula::Selected::Formula(tex)) => {
            LAST_RANGE.with(|r| *r.borrow_mut() = Some(range.clone_range()));
            let math = start.closest("math").ok().flatten().unwrap_or(start);
            return Some(formula_info(&math, &paper, tex));
        }
        Some(crate::formula::Selected::Text(with_tex)) => {
            let with_tex = normalize_whitespace(&with_tex);
            match Mode::detect(&with_tex) {
                // 空白の無いひとまとまり（N-gram の N が数式など）は、見たままの語として辞書を引く
                Mode::Word => (plain, Mode::Word),
                mode => (with_tex, mode),
            }
        }
        None => {
            let mode = Mode::detect(&plain);
            (plain, mode)
        }
    };
    if text.is_empty() {
        return None;
    }

    LAST_RANGE.with(|r| *r.borrow_mut() = Some(range.clone_range()));

    let context_el = context_element(&start, &paper);
    CONTEXT_ELEMENT.with(|c| *c.borrow_mut() = context_el.clone());
    let context = context_el
        .and_then(|el| context_text(&el))
        .unwrap_or_else(|| text.clone());

    Some(SelectionInfo {
        text,
        context,
        mode,
    })
}

/// 選択を含む段落の要素。
fn context_element(el: &Element, paper: &Element) -> Option<Element> {
    closest_in(el, paper, CONTEXT_SELECTOR)
        .or_else(|| closest_in(el, paper, FALLBACK_CONTEXT_SELECTOR))
}

/// 段落の文字列（数式は `$…$` の LaTeX）。空なら None。
fn context_text(el: &Element) -> Option<String> {
    let text = crate::formula::element_text(el)
        .or_else(|| el.dyn_ref::<HtmlElement>().map(HtmlElement::inner_text))?;
    Some(normalize_whitespace(&text)).filter(|t| !t.is_empty())
}

/// 数式の解説に渡す内容。記号の定義は式の前後に書かれることが多いので、
/// 別の行の数式は段落（LaTeXML の `.ltx_para`）と、その前後の段落を文脈にする。
fn formula_info(el: &Element, paper: &Element, tex: String) -> SelectionInfo {
    let block = el
        .closest(".ltx_equationgroup, .ltx_equation, math[display=\"block\"]")
        .ok()
        .flatten()
        .is_some();
    let para = block.then(|| closest_in(el, paper, ".ltx_para")).flatten();
    let (context_el, context) = match para {
        Some(para) => {
            let around = [
                para.previous_element_sibling(),
                Some(para.clone()),
                para.next_element_sibling(),
            ];
            let text = around
                .into_iter()
                .flatten()
                .filter(|e| e.class_list().contains("ltx_para"))
                .filter_map(|e| context_text(&e))
                .collect::<Vec<_>>()
                .join(" ");
            (Some(para), text)
        }
        None => {
            // 数式の中の要素は段落とみなさない（表の数式は td が最も近くなるので、親の段落を探す）
            let found =
                closest_in(el, paper, "p, li, figcaption").or_else(|| context_element(el, paper));
            let text = found.as_ref().and_then(context_text).unwrap_or_default();
            (found, text)
        }
    };
    CONTEXT_ELEMENT.with(|c| *c.borrow_mut() = context_el);
    SelectionInfo {
        context: if context.is_empty() {
            tex.clone()
        } else {
            context
        },
        text: tex,
        mode: Mode::Formula,
    }
}

/// 本文の数式を押したら、式の全体を選ぶ。ほかの場所を押したらやめる。
/// ドラッグで文を選んだときと、リンクを押したときは何もしない。
fn watch_formula_taps(paper: &Element, commit: Rc<dyn Fn()>) -> Result<(), String> {
    let on_click = Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
        if native_selection_range().is_some() {
            return;
        }
        let formula = ev
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .filter(|el| el.closest("a, button").ok().flatten().is_none())
            .and_then(|el| crate::formula::at(&el));
        match formula {
            Some((el, tex)) => {
                select_formula(el, tex.clone());
                commit();
                crate::selection_menu::show(&tex);
            }
            None => {
                if clear_formula() {
                    LAST_SELECTION.with(|l| *l.borrow_mut() = None);
                    crate::selection_menu::hide();
                }
            }
        }
    });
    paper
        .add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())
        .map_err(|_| "click を購読できません")?;
    on_click.forget();
    Ok(())
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

    watch_formula_taps(&paper, commit.clone())?;

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

/// 要素の中の表示される文字（数式の注釈などは除く）。
pub fn visible_text(el: &Element) -> String {
    let mut nodes = Vec::new();
    collect_text_nodes(el, &mut nodes);
    nodes.iter().map(|n| n.data()).collect()
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
/// 付箋を付けたブロックと、その中に重ねる 1 行分の印
const BOOKMARK_CLASS: &str = "pr-bookmark";
const BOOKMARK_LINE_CLASS: &str = "pr-bookmark-line";
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

/// 付箋を貼る行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingLine {
    pub block: u32,
    /// ブロックの本文の先頭から数えた、その行の文字の位置（UTF-16）
    pub offset: u32,
    pub snippet: String,
}

/// 画面の上端にこれだけ隠れていても、その行は見えているとみなす（px）
const LINE_SLACK_PX: f64 = 4.0;

/// 付箋を貼る行。
/// 本文で選択していて、選択の先頭の行が見えていればそこ。無ければ、いま画面のいちばん上に見えている行
/// （章・節の見出しは読んだ位置が分かりにくいので、ほかに見えている行があれば選ばない）。
///
/// `use_selection` なら、ブラウザの選択が消えていても最後に選んだ位置を使う
/// （スマホでは解説ペインのボタンを押すと選択が消えることがある）。
pub fn reading_line(use_selection: bool) -> Option<ReadingLine> {
    let paper = paper_element()?;
    let list = blocks(&paper);
    let view = paper.get_bounding_client_rect();
    let range = document()?.create_range().ok()?;
    let visible_line =
        |rect: &DomRect| rect.bottom() > view.top() + LINE_SLACK_PX && rect.top() < view.bottom();

    let live = web_sys::window()
        .and_then(|w| w.get_selection().ok().flatten())
        .filter(|s| s.range_count() > 0 && !s.is_collapsed())
        .and_then(|s| s.get_range_at(0).ok());
    let remembered = use_selection
        .then(|| LAST_RANGE.with(|r| r.borrow().clone()))
        .flatten();
    let selected = live.or(remembered).and_then(|selected| {
        let node = selected
            .start_container()
            .ok()?
            .dyn_into::<Text>()
            .ok()
            .filter(|n| n.is_connected())?;
        let at = selected.start_offset().ok()?;
        let (_, rect) = char_rect_from(&range, &node, at)?;
        if !visible_line(&rect) {
            return None;
        }
        line_at(&paper, &list, &node, at)
    });
    if selected.is_some() {
        return selected;
    }

    let heading = |el: &Element| el.matches(HEADING_SELECTOR).unwrap_or(false);
    let overlaps = |el: &Element| {
        let r = el.get_bounding_client_rect();
        r.height() > 0.0 && visible_line(&r)
    };
    // 上端より下で始まる最初の行。見出しは後回しにする
    [false, true].into_iter().find_map(|allow_heading| {
        list.iter()
            .filter(|el| overlaps(el) && (allow_heading || !heading(el)))
            .find_map(|el| {
                let (node, at) = first_line_below(&range, el, view.top() - LINE_SLACK_PX)?;
                let (_, rect) = char_rect_from(&range, &node, at)?;
                if !visible_line(&rect) {
                    return None;
                }
                line_at(&paper, &list, &node, at)
                    .filter(|line| allow_heading || !heading(&list[line.block as usize]))
            })
    })
}

/// テキストノードの `at` 文字目の位置を、それを含むいちばん内側のブロックからの位置にする。
fn line_at(paper: &Element, list: &[Element], node: &Text, at: u32) -> Option<ReadingLine> {
    let block = closest_in(&node.parent_element()?, paper, BLOCK_SELECTOR)?;
    let index = list.iter().position(|b| *b == block)?;
    let mut nodes = Vec::new();
    collect_text_nodes(&block, &mut nodes);
    let mut offset = 0;
    for n in &nodes {
        if n == node {
            return Some(ReadingLine {
                block: u32::try_from(index).ok()?,
                offset: offset + at,
                snippet: block_snippet(&block),
            });
        }
        offset += n.length();
    }
    None
}

/// 1 文字の表示位置。折りたたまれた空白など、表示されない文字は `None`。
fn char_rect(range: &Range, node: &Text, at: u32) -> Option<DomRect> {
    range.set_start(node, at).ok()?;
    range.set_end(node, at + 1).ok()?;
    let rect = range.get_bounding_client_rect();
    (rect.width() > 0.0 && rect.height() > 0.0).then_some(rect)
}

/// `from` 文字目以降で、最初に表示される文字の位置と表示位置。
fn char_rect_from(range: &Range, node: &Text, from: u32) -> Option<(u32, DomRect)> {
    (from..node.length()).find_map(|i| char_rect(range, node, i).map(|r| (i, r)))
}

/// 要素の中で、上端が `top` より下にある最初の文字。
fn first_line_below(range: &Range, el: &Element, top: f64) -> Option<(Text, u32)> {
    let mut nodes = Vec::new();
    collect_text_nodes(el, &mut nodes);
    for node in nodes {
        if node.data().trim().is_empty() || range.select_node_contents(&node).is_err() {
            continue;
        }
        let rect = range.get_bounding_client_rect();
        if rect.height() <= 0.0 || rect.bottom() <= top {
            continue;
        }
        // 文字は読む順に下へ並ぶので、二分探索で最初の 1 文字を探す
        let (mut lo, mut hi) = (0, node.length());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match char_rect_from(range, &node, mid) {
                Some((i, r)) if r.top() < top => lo = i + 1,
                _ => hi = mid,
            }
        }
        if let Some((at, _)) = char_rect_from(range, &node, lo) {
            return Some((node, at));
        }
    }
    None
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
pub fn show_bookmark(block: u32, offset: u32, snippet: &str, scroll: BookmarkScroll) -> bool {
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
    let Some(line) = BookmarkLine::attach(el, offset) else {
        return false;
    };
    let behavior = match scroll {
        BookmarkScroll::None => None,
        BookmarkScroll::Instant => Some(web_sys::ScrollBehavior::Instant),
        BookmarkScroll::Smooth => Some(web_sys::ScrollBehavior::Smooth),
    };
    if let Some(behavior) = behavior {
        let opts = web_sys::ScrollIntoViewOptions::new();
        opts.set_block(web_sys::ScrollLogicalPosition::Start);
        opts.set_behavior(behavior);
        line.mark
            .scroll_into_view_with_scroll_into_view_options(&opts);
    }
    BOOKMARK.with(|b| *b.borrow_mut() = Some(line));
    true
}

/// 本文に付けている付箋の印（1 行分の下線とアイコン）。
/// ブロックの子として重ねるので、前にある図が読み込まれて本文がずれても一緒に動く。
/// 画面の幅が変わると折り返しが変わるので、ブロックの大きさが変わるたびに位置を測り直す。
struct BookmarkLine {
    block: Element,
    mark: Element,
    observer: ResizeObserver,
    _on_resize: Closure<dyn FnMut()>,
}

impl BookmarkLine {
    fn attach(block: &Element, offset: u32) -> Option<Self> {
        let mark = document()?.create_element("span").ok()?;
        mark.set_class_name(BOOKMARK_LINE_CLASS);
        let _ = mark.set_attribute("aria-label", "付箋");
        block.class_list().add_1(BOOKMARK_CLASS).ok()?;
        block.append_child(&mark).ok()?;
        place_line(block, &mark, offset);

        let on_resize = {
            let (block, mark) = (block.clone(), mark.clone());
            Closure::<dyn FnMut()>::new(move || place_line(&block, &mark, offset))
        };
        let observer = ResizeObserver::new(on_resize.as_ref().unchecked_ref()).ok()?;
        observer.observe(block);
        Some(Self {
            block: block.clone(),
            mark,
            observer,
            _on_resize: on_resize,
        })
    }

    fn detach(self) {
        self.observer.disconnect();
        self.mark.remove();
        let _ = self.block.class_list().remove_1(BOOKMARK_CLASS);
    }
}

/// `offset` の文字がある行の範囲（画面の座標の left, top, right, bottom）。
fn line_rect(block: &Element, offset: u32) -> Option<(f64, f64, f64, f64)> {
    let range = document()?.create_range().ok()?;
    let mut nodes = Vec::new();
    collect_text_nodes(block, &mut nodes);
    // offset の文字（表示されない文字なら、その後で最初に表示される文字）
    let mut start = 0;
    let found = nodes.iter().find_map(|node| {
        let from = offset.saturating_sub(start);
        start += node.length();
        char_rect_from(&range, node, from).map(|(_, r)| r)
    });
    // 版が変わって短くなっていれば、ブロックの 1 行目
    let at = found.or_else(|| {
        nodes
            .iter()
            .find_map(|n| char_rect_from(&range, n, 0).map(|(_, r)| r))
    })?;

    let (mut left, mut top, mut right, mut bottom) = (at.left(), at.top(), at.right(), at.bottom());
    for node in &nodes {
        if range.select_node_contents(node).is_err() {
            continue;
        }
        let Some(rects) = range.get_client_rects() else {
            continue;
        };
        for r in (0..rects.length()).filter_map(|i| rects.item(i)) {
            // 中心の高さがその文字の行にあるものは、同じ行
            let center = r.top() + r.height() / 2.0;
            if r.width() > 0.0 && center > at.top() && center < at.bottom() {
                left = left.min(r.left());
                right = right.max(r.right());
                top = top.min(r.top());
                bottom = bottom.max(r.bottom());
            }
        }
    }
    Some((left, top, right, bottom))
}

/// 印を `offset` の行に重ねる。
fn place_line(block: &Element, mark: &Element, offset: u32) {
    let Some((left, top, right, bottom)) = line_rect(block, offset) else {
        return;
    };
    let Some(style) = mark.dyn_ref::<HtmlElement>().map(HtmlElement::style) else {
        return;
    };
    let origin = block.get_bounding_client_rect();
    for (name, value) in [
        ("left", left - origin.left()),
        ("top", top - origin.top()),
        ("width", right - left),
        ("height", bottom - top),
    ] {
        let _ = style.set_property(name, &format!("{value:.1}px"));
    }
}

/// 付箋の印を外す。
pub fn clear_bookmark() {
    if let Some(line) = BOOKMARK.with(|b| b.borrow_mut().take()) {
        line.detach();
    }
}
