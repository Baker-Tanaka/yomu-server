//! 本文の上の 1 行に出す、直近に引いた単語のメモ（`#recent-words`）。
//! 新しい順に横へ並べ、横スクロールで前の語を見る。押すとその語をもう一度引く。

use std::cell::RefCell;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, MouseEvent};

use crate::recent::RecentWord;

const BAR_ID: &str = "recent-words";
/// 押された語を持つ属性
const WORD_ATTR: &str = "data-word";

thread_local! {
    /// いま出している内容（変わらなければ DOM を作り直さない）
    static SHOWN: RefCell<(Vec<RecentWord>, Option<String>)> = RefCell::default();
}

fn document() -> Option<Document> {
    web_sys::window()?.document()
}

fn bar() -> Option<Element> {
    document()?.get_element_by_id(BAR_ID)
}

/// 単語が押されたときに `on_pick` を呼ぶ。
/// リスナーはページの寿命と同じなので、クロージャは意図的に解放しない。
pub fn watch(on_pick: impl Fn(String) + 'static) -> Result<(), String> {
    let bar = bar().ok_or("#recent-words が見つかりません")?;
    let on_click = Closure::<dyn FnMut(MouseEvent)>::new(move |ev: MouseEvent| {
        let word = ev
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|el| el.closest(&format!("[{WORD_ATTR}]")).ok().flatten())
            .and_then(|el| el.get_attribute(WORD_ATTR));
        if let Some(word) = word {
            on_pick(word);
        }
    });
    bar.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())
        .map_err(|_| "click を購読できません")?;
    on_click.forget();
    Ok(())
}

/// `items`（新しい順）を出す。空なら行ごと隠す。`active` は選択中の単語。
/// 毎フレーム呼んでよい（内容が変わったときだけ描き直す）。
pub fn render<'a>(items: impl Iterator<Item = &'a RecentWord> + Clone, active: Option<&str>) {
    let unchanged = SHOWN.with(|s| {
        let s = s.borrow();
        s.1.as_deref() == active && s.0.iter().eq(items.clone())
    });
    if unchanged {
        return;
    }
    let items: Vec<RecentWord> = items.cloned().collect();
    let words_changed = SHOWN.with(|s| s.borrow().0 != items);
    if build(&items, active, words_changed).is_some() {
        SHOWN.with(|s| *s.borrow_mut() = (items, active.map(str::to_owned)));
    }
}

/// `tag` の要素を作る。文字は text として入れる（HTML として解釈しない）。
fn element(doc: &Document, tag: &str, class: &str, text: &str) -> Option<Element> {
    let el = doc.create_element(tag).ok()?;
    el.set_class_name(class);
    el.set_text_content(Some(text));
    Some(el)
}

fn build(items: &[RecentWord], active: Option<&str>, words_changed: bool) -> Option<()> {
    let doc = document()?;
    let bar = bar()?;
    if items.is_empty() {
        bar.set_text_content(None);
        return bar.set_attribute("hidden", "").ok();
    }
    let scroll = bar.scroll_left();
    bar.set_text_content(None);
    let label = element(&doc, "span", "recent-label", "最近")?;
    let _ = label.set_attribute(
        "title",
        "最近引いた単語（このブラウザにだけ保存。⚙ で非表示・消去）",
    );
    bar.append_child(&label).ok()?;
    for item in items {
        let current = active.is_some_and(|a| a.eq_ignore_ascii_case(&item.word));
        let button = element(&doc, "button", if current { "active" } else { "" }, "")?;
        let _ = button.set_attribute("type", "button");
        let _ = button.set_attribute(WORD_ATTR, &item.word);
        let _ = button.set_attribute("title", &format!("{}  {}", item.word, item.meaning));
        let word = element(&doc, "span", "recent-word", &item.word)?;
        let _ = word.set_attribute("lang", "en");
        button.append_child(&word).ok()?;
        let meaning = element(&doc, "span", "recent-meaning", &item.meaning)?;
        button.append_child(&meaning).ok()?;
        bar.append_child(&button).ok()?;
    }
    bar.remove_attribute("hidden").ok()?;
    // 新しい語が加わったら先頭（新しい方）を見せる。選択の印が変わっただけなら位置を保つ
    bar.set_scroll_left(if words_changed { 0 } else { scroll });
    Some(())
}
