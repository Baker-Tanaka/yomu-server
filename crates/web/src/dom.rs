//! 左ペイン（#paper）の DOM 操作と、テキスト選択の取得。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use paper_reader_shared::{Mode, normalize_whitespace};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, HtmlElement, Node};

const PAPER_ID: &str = "paper";
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

/// 無害化済みの論文 HTML を #paper に入れる。
pub fn set_paper_html(html: &str, title: &str) {
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

    let last: Rc<RefCell<Option<SelectionInfo>>> = Rc::default();
    let commit: Rc<dyn Fn()> = Rc::new(move || {
        let Some(info) = capture_selection() else {
            return;
        };
        let mut last = last.borrow_mut();
        // selectionchange と mouseup の両方から同じ選択が来るので重複を除く
        if last.as_ref() != Some(&info) {
            *last = Some(info.clone());
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
