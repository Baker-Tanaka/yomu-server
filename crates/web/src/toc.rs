//! 本文の目次と、読んだ割合の表示。
//!
//! 本文の下端の細い線（`#reading-progress`）と右下のボタン（`#toc-button`）で今の位置を示し、
//! ボタンを押すと章・節の一覧（`#toc-panel`）を開いて移れる。

use std::cell::RefCell;

use paper_reader_shared::normalize_whitespace;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, Event, HtmlElement, KeyboardEvent, Node, ResizeObserver};

use crate::dom;

const COLUMN_ID: &str = "paper-column";
const PROGRESS_ID: &str = "reading-progress";
const BUTTON_ID: &str = "toc-button";
const PANEL_ID: &str = "toc-panel";
/// 目次に出す見出し（章・節と、アブストラクトの見出し）
const HEADING_SELECTOR: &str = "h2, h3, h6.ltx_title_abstract";
/// 見出しの番号を持つ属性
const INDEX_ATTR: &str = "data-index";
/// 本文の上端からこの高さまでに入った見出しを、今読んでいる節とする（px）
const CURRENT_SLACK_PX: f64 = 80.0;

/// 目次の 1 項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    /// 章なら 2、節なら 3
    pub level: u8,
}

thread_local! {
    /// 表示中の本文の見出し
    static HEADINGS: RefCell<Vec<(Element, Section)>> = const { RefCell::new(Vec::new()) };
}

fn document() -> Option<Document> {
    web_sys::window()?.document()
}

fn by_id(id: &str) -> Option<HtmlElement> {
    document()?.get_element_by_id(id)?.dyn_into().ok()
}

/// 表示中の論文の目次。
pub fn sections() -> Vec<Section> {
    HEADINGS.with(|h| h.borrow().iter().map(|(_, s)| s.clone()).collect())
}

/// 本文を入れ替えたあとに呼ぶ。見出しを集め直し、論文でなければ目次を隠す。
pub fn refresh() {
    close();
    let headings: Vec<(Element, Section)> = by_id("paper")
        .and_then(|paper| paper.query_selector_all(HEADING_SELECTOR).ok())
        .map(|list| {
            (0..list.length())
                .filter_map(|i| list.item(i))
                .filter_map(|n| n.dyn_into::<Element>().ok())
                .filter_map(|el| {
                    let title = normalize_whitespace(&dom::visible_text(&el));
                    let level = if el.tag_name().eq_ignore_ascii_case("h3") {
                        3
                    } else {
                        2
                    };
                    (!title.is_empty()).then_some((el, Section { title, level }))
                })
                .collect()
        })
        .unwrap_or_default();
    let empty = headings.is_empty();
    HEADINGS.with(|h| *h.borrow_mut() = headings);
    for id in [BUTTON_ID, PROGRESS_ID] {
        if let Some(el) = by_id(id) {
            el.set_hidden(empty);
        }
    }
    update();
}

/// 読んだ割合（0〜100）。本文がスクロールできなければ 100。
fn percent(paper: &Element) -> i32 {
    let range = paper.scroll_height() - paper.client_height();
    if range <= 0 {
        return 100;
    }
    (f64::from(paper.scroll_top()) / f64::from(range) * 100.0)
        .round()
        .clamp(0.0, 100.0) as i32
}

/// 今読んでいる節の番号。
fn current_index(paper: &Element) -> Option<usize> {
    let line = paper.get_bounding_client_rect().top() + CURRENT_SLACK_PX;
    HEADINGS.with(|h| {
        h.borrow()
            .iter()
            .rposition(|(el, _)| el.get_bounding_client_rect().top() <= line)
    })
}

/// 割合の表示と、開いている目次の「今の節」を合わせる。
fn update() {
    let Some(paper) = by_id("paper") else {
        return;
    };
    let percent = percent(&paper);
    if let Some(bar) = by_id(PROGRESS_ID) {
        let _ = bar
            .style()
            .set_property("--progress", &format!("{percent}%"));
        let _ = bar.set_attribute("aria-valuenow", &percent.to_string());
    }
    if let Some(button) = by_id(BUTTON_ID) {
        let label = format!("☰ 目次 {percent}%");
        if button.text_content().as_deref() != Some(label.as_str()) {
            button.set_text_content(Some(&label));
        }
    }
    let Some(panel) = by_id(PANEL_ID).filter(|p| !p.hidden()) else {
        return;
    };
    let current = current_index(&paper);
    let Ok(items) = panel.query_selector_all(&format!("[{INDEX_ATTR}]")) else {
        return;
    };
    for i in 0..items.length() {
        let Some(item) = items.item(i).and_then(|n| n.dyn_into::<Element>().ok()) else {
            continue;
        };
        if Some(i as usize) == current {
            let _ = item.set_attribute("aria-current", "true");
        } else {
            let _ = item.remove_attribute("aria-current");
        }
    }
}

/// `index` 番目の見出しまで移る。
pub fn jump(index: usize) {
    let target = HEADINGS.with(|h| h.borrow().get(index).map(|(el, _)| el.clone()));
    if let Some(el) = target {
        el.scroll_into_view();
    }
}

pub fn close() {
    if let Some(panel) = by_id(PANEL_ID) {
        panel.set_hidden(true);
    }
    if let Some(button) = by_id(BUTTON_ID) {
        let _ = button.set_attribute("aria-expanded", "false");
    }
}

/// 目次を開く（論文を開いていなければ何もしない）。
pub fn open() {
    if fill().is_none() {
        return;
    }
    if let Some(panel) = by_id(PANEL_ID) {
        panel.set_hidden(false);
        update();
        // 今の節が見える位置まで一覧を動かす
        if let Ok(Some(current)) = panel.query_selector("[aria-current]")
            && let Ok(current) = current.dyn_into::<HtmlElement>()
        {
            panel.set_scroll_top(current.offset_top() - panel.client_height() / 2);
        }
    }
    if let Some(button) = by_id(BUTTON_ID) {
        let _ = button.set_attribute("aria-expanded", "true");
    }
}

fn fill() -> Option<()> {
    let doc = document()?;
    let panel = by_id(PANEL_ID)?;
    let sections = sections();
    if sections.is_empty() {
        return None;
    }
    panel.set_text_content(None);
    for (i, section) in sections.iter().enumerate() {
        let button = doc.create_element("button").ok()?;
        let _ = button.set_attribute("type", "button");
        let _ = button.set_attribute(INDEX_ATTR, &i.to_string());
        button.set_class_name(if section.level == 3 {
            "toc-sub"
        } else {
            "toc-top"
        });
        button.set_text_content(Some(&section.title));
        panel.append_child(&button).ok()?;
    }
    Some(())
}

fn create(doc: &Document, tag: &str, id: &str) -> Option<HtmlElement> {
    let el: HtmlElement = doc.create_element(tag).ok()?.dyn_into().ok()?;
    el.set_id(id);
    el.set_hidden(true);
    let _ = el.set_attribute("lang", "ja");
    Some(el)
}

/// 目次の部品を作り、スクロールを監視する。
/// リスナーはページの寿命と同じなので、クロージャは意図的に解放しない。
pub fn init() -> Result<(), String> {
    let doc = document().ok_or("document が取得できません")?;
    let column = by_id(COLUMN_ID).ok_or("#paper-column が見つかりません")?;
    let paper = by_id("paper").ok_or("#paper が見つかりません")?;
    let progress = create(&doc, "div", PROGRESS_ID).ok_or("目次を作れません")?;
    let _ = progress.set_attribute("role", "progressbar");
    let _ = progress.set_attribute("aria-label", "読んだ割合");
    let button = create(&doc, "button", BUTTON_ID).ok_or("目次を作れません")?;
    let _ = button.set_attribute("type", "button");
    let _ = button.set_attribute("aria-controls", PANEL_ID);
    let panel = create(&doc, "nav", PANEL_ID).ok_or("目次を作れません")?;
    let _ = panel.set_attribute("aria-label", "目次");
    for el in [&progress, &button, &panel] {
        column
            .append_child(el)
            .map_err(|_| "目次を追加できません")?;
    }

    let on_button = Closure::<dyn FnMut()>::new(|| {
        if by_id(PANEL_ID).is_some_and(|p| p.hidden()) {
            open();
        } else {
            close();
        }
    });
    button
        .add_event_listener_with_callback("click", on_button.as_ref().unchecked_ref())
        .map_err(|_| "click を購読できません")?;
    on_button.forget();

    let on_item = Closure::<dyn FnMut(Event)>::new(|ev: Event| {
        let index = ev
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|el| el.closest(&format!("[{INDEX_ATTR}]")).ok().flatten())
            .and_then(|el| el.get_attribute(INDEX_ATTR))
            .and_then(|v| v.parse().ok());
        if let Some(index) = index {
            close();
            jump(index);
        }
    });
    panel
        .add_event_listener_with_callback("click", on_item.as_ref().unchecked_ref())
        .map_err(|_| "click を購読できません")?;
    on_item.forget();

    // 目次の外を押す・Esc で閉じる
    let on_pointerdown = Closure::<dyn FnMut(Event)>::new(|ev: Event| {
        let target = ev.target().and_then(|t| t.dyn_into::<Node>().ok());
        let inside = [PANEL_ID, BUTTON_ID]
            .iter()
            .filter_map(|id| by_id(id))
            .any(|el| el.contains(target.as_ref()));
        if !inside {
            close();
        }
    });
    doc.add_event_listener_with_callback("pointerdown", on_pointerdown.as_ref().unchecked_ref())
        .map_err(|_| "pointerdown を購読できません")?;
    on_pointerdown.forget();
    let on_keydown = Closure::<dyn FnMut(KeyboardEvent)>::new(|ev: KeyboardEvent| {
        if ev.key() == "Escape" {
            close();
        }
    });
    doc.add_event_listener_with_callback("keydown", on_keydown.as_ref().unchecked_ref())
        .map_err(|_| "keydown を購読できません")?;
    on_keydown.forget();

    // 図の読み込みで本文の長さが変わる分は、次のスクロールで合う
    let on_scroll = Closure::<dyn FnMut()>::new(update);
    paper
        .add_event_listener_with_callback("scroll", on_scroll.as_ref().unchecked_ref())
        .map_err(|_| "scroll を購読できません")?;
    let observer = ResizeObserver::new(on_scroll.as_ref().unchecked_ref())
        .map_err(|_| "本文の大きさを監視できません")?;
    observer.observe(&paper);
    on_scroll.forget();
    Ok(())
}
