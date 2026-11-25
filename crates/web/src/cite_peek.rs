//! 本文の引用番号（`[14]` など）をタップしたときに、参考文献のその 1 件を小さなカードで見せる。
//!
//! 何もしないと参考文献の一覧まで本文がスクロールして、読んでいた場所が分からなくなるため、
//! 本文は動かさずに、タップした番号の近くにカードを出す。

use std::cell::RefCell;
use std::rc::Rc;

use paper_reader_shared::{cited_arxiv_id, cited_title, normalize_whitespace};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, Event, HtmlElement, KeyboardEvent, MouseEvent, Node};

const CARD_ID: &str = "cite-peek";
/// 本文の中の、参考文献へのリンク（LaTeXML の ID は bib.bib1 の形）
const LINK_SELECTOR: &str = "a[href^=\"#bib.\"]";
/// カードと番号・画面の端とのすき間（px）
const GAP_PX: f64 = 8.0;
const MAX_WIDTH_PX: f64 = 460.0;

/// カードのボタンで、アプリ（解説ペイン）に頼む操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiteAction {
    /// この arXiv ID の論文を開く
    Open(String),
    /// 題名で arXiv を検索する（検索結果で概要を読める）
    Search(String),
}

type Handler = Rc<dyn Fn(CiteAction)>;

thread_local! {
    /// 表示中のカードのボタンのクロージャ（カードを閉じるときに解放する）
    static CALLBACKS: RefCell<Vec<Closure<dyn FnMut()>>> = const { RefCell::new(Vec::new()) };
}

fn document() -> Option<Document> {
    web_sys::window()?.document()
}

/// カードを閉じる。
pub fn close() {
    if let Some(card) = document().and_then(|d| d.get_element_by_id(CARD_ID)) {
        card.remove();
        // 押されたボタン自身のクロージャは実行中なので、解放は次に回す
        let old = CALLBACKS.take();
        wasm_bindgen_futures::spawn_local(async move { drop(old) });
    }
}

/// 引用番号のクリックの監視を始める。
/// リスナーはページの寿命と同じなので、クロージャは意図的に解放しない。
pub fn watch(on_action: impl Fn(CiteAction) + 'static) -> Result<(), String> {
    let doc = document().ok_or("document が取得できません")?;
    let paper = doc
        .get_element_by_id("paper")
        .ok_or("#paper が見つかりません")?;
    let on_action: Handler = Rc::new(on_action);

    let on_click = {
        let paper = paper.clone();
        Closure::<dyn FnMut(MouseEvent)>::new(move |ev: MouseEvent| {
            // 新しいタブで開くなどの操作は、ブラウザに任せる
            if ev.button() != 0 || ev.meta_key() || ev.ctrl_key() || ev.shift_key() {
                return;
            }
            let Some(link) = ev
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|el| el.closest(LINK_SELECTOR).ok().flatten())
            else {
                return;
            };
            let Some(item) = bib_item(&paper, &link) else {
                return;
            };
            ev.prevent_default();
            show(&paper, &link, &item, on_action.clone());
        })
    };
    paper
        .add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())
        .map_err(|_| "click を購読できません")?;
    on_click.forget();

    // カードの外を押す・本文をスクロールする・Esc で閉じる
    let on_pointerdown = Closure::<dyn FnMut(Event)>::new(|ev: Event| {
        let Some(card) = document().and_then(|d| d.get_element_by_id(CARD_ID)) else {
            return;
        };
        let inside = ev
            .target()
            .and_then(|t| t.dyn_into::<Node>().ok())
            .is_some_and(|n| card.contains(Some(&n)));
        if !inside {
            close();
        }
    });
    doc.add_event_listener_with_callback("pointerdown", on_pointerdown.as_ref().unchecked_ref())
        .map_err(|_| "pointerdown を購読できません")?;
    on_pointerdown.forget();

    let on_scroll = Closure::<dyn FnMut()>::new(close);
    paper
        .add_event_listener_with_callback("scroll", on_scroll.as_ref().unchecked_ref())
        .map_err(|_| "scroll を購読できません")?;
    on_scroll.forget();

    let on_keydown = Closure::<dyn FnMut(KeyboardEvent)>::new(|ev: KeyboardEvent| {
        if ev.key() == "Escape" {
            close();
        }
    });
    doc.add_event_listener_with_callback("keydown", on_keydown.as_ref().unchecked_ref())
        .map_err(|_| "keydown を購読できません")?;
    on_keydown.forget();
    Ok(())
}

/// リンク先の参考文献（本文の中にあるものだけ）。
fn bib_item(paper: &Element, link: &Element) -> Option<Element> {
    let href = link.get_attribute("href")?;
    let id = href.strip_prefix('#')?;
    // 属性セレクタに入れるので、引用符などを含むものは扱わない
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return None;
    }
    paper
        .query_selector(&format!("[id=\"{id}\"]"))
        .ok()
        .flatten()
}

fn texts(el: &Element, selector: &str) -> Vec<String> {
    let Ok(list) = el.query_selector_all(selector) else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|i| list.item(i))
        .map(|n| normalize_whitespace(&n.text_content().unwrap_or_default()))
        .filter(|t| !t.is_empty())
        .collect()
}

/// `tag` の要素を作る。文字は text として入れる（HTML として解釈しない）。
fn element(doc: &Document, tag: &str, class: &str, text: &str) -> Option<Element> {
    let el = doc.create_element(tag).ok()?;
    el.set_class_name(class);
    if !text.is_empty() {
        el.set_text_content(Some(text));
    }
    Some(el)
}

fn button(
    doc: &Document,
    class: &str,
    label: &str,
    on_click: impl FnMut() + 'static,
) -> Option<Element> {
    let el = element(doc, "button", class, label)?;
    let _ = el.set_attribute("type", "button");
    let closure = Closure::<dyn FnMut()>::new(on_click);
    el.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())
        .ok()?;
    CALLBACKS.with(|c| c.borrow_mut().push(closure));
    Some(el)
}

fn show(paper: &Element, link: &Element, item: &Element, on_action: Handler) {
    close();
    if build(paper, link, item, on_action).is_none() {
        close();
    }
}

fn build(paper: &Element, link: &Element, item: &Element, on_action: Handler) -> Option<()> {
    let doc = document()?;
    let label = texts(item, ".ltx_tag_bibitem")
        .into_iter()
        .next()
        .unwrap_or_default();
    let mut blocks = texts(item, ".ltx_bibblock");
    if blocks.is_empty() {
        blocks = vec![normalize_whitespace(
            &item.text_content().unwrap_or_default(),
        )];
    }
    // 参考文献の中の arXiv へのリンクか、文字で書かれた arXiv ID
    let linked = item
        .query_selector("a[href*=\"arxiv.org/\"]")
        .ok()
        .flatten()
        .and_then(|a| a.get_attribute("href"))
        .and_then(|href| cited_arxiv_id(&href));
    let id = linked.or_else(|| cited_arxiv_id(&blocks.join(" ")));
    let title = cited_title(&blocks);

    let card = element(&doc, "div", "", "")?;
    card.set_id(CARD_ID);
    let _ = card.set_attribute("role", "dialog");
    let _ = card.set_attribute("aria-label", "引用文献");

    let head = element(&doc, "div", "cite-head", "")?;
    append(
        &head,
        element(&doc, "span", "cite-label", &format!("引用文献 {label}"))?,
    )?;
    let close_button = button(&doc, "cite-close", "✕", close)?;
    let _ = close_button.set_attribute("aria-label", "閉じる");
    head.append_child(&close_button).ok()?;
    card.append_child(&head).ok()?;

    let body = element(&doc, "div", "cite-body", "")?;
    let _ = body.set_attribute("lang", "en");
    for block in &blocks {
        append(&body, element(&doc, "p", "", block)?)?;
    }
    card.append_child(&body).ok()?;

    let actions = element(&doc, "div", "cite-actions", "")?;
    let action_button = |class: &str, label: &str, action: CiteAction| {
        let on_action = on_action.clone();
        button(&doc, class, label, move || {
            close();
            on_action(action.clone());
        })
    };
    if let Some(id) = id {
        append(
            &actions,
            action_button("primary", "この論文を開く", CiteAction::Open(id))?,
        )?;
    }
    if let Some(title) = title {
        append(
            &actions,
            action_button("", "arXiv で探す（概要を見る）", CiteAction::Search(title))?,
        )?;
    }
    let target = item.clone();
    append(
        &actions,
        button(&doc, "", "一覧の位置へ移る", move || {
            close();
            target.scroll_into_view();
        })?,
    )?;
    card.append_child(&actions).ok()?;

    doc.body()?.append_child(&card).ok()?;
    place(paper, link, card.dyn_ref::<HtmlElement>()?);
    Some(())
}

fn append(parent: &Element, child: Element) -> Option<()> {
    parent.append_child(&child).ok().map(|_| ())
}

/// 番号の下（入らなければ上の広い方）に、本文の範囲に収めて置く。
fn place(paper: &Element, link: &Element, card: &HtmlElement) {
    let area = paper.get_bounding_client_rect();
    let at = link.get_bounding_client_rect();
    let style = card.style();
    let width = (area.width() - 2.0 * GAP_PX).min(MAX_WIDTH_PX);
    let left = (at.left() - width / 2.0)
        .min(area.right() - GAP_PX - width)
        .max(area.left() + GAP_PX);
    let _ = style.set_property("width", &format!("{width:.0}px"));
    let _ = style.set_property("left", &format!("{left:.0}px"));

    let below = area.bottom() - at.bottom() - 2.0 * GAP_PX;
    let above = at.top() - area.top() - 2.0 * GAP_PX;
    let height = f64::from(card.offset_height());
    let (top, max_height) = if height <= below || below >= above {
        (at.bottom() + GAP_PX, below)
    } else {
        (at.top() - GAP_PX - height.min(above), above)
    };
    let _ = style.set_property("top", &format!("{top:.0}px"));
    let _ = style.set_property("max-height", &format!("{max_height:.0}px"));
}
