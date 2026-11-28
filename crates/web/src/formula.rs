//! 本文の数式（MathML）を LaTeX で読む。
//!
//! MathML の文字をそのまま取り出すと「{ a ^ i } i = 1 …」のように崩れるので、
//! 数式の部分は元の LaTeX（`alttext` か `annotation`）に置き換えて、選択した文と文脈を作る。

use paper_reader_shared::{MAX_TEXT_CHARS, clean_tex, inline_tex};
use wasm_bindgen::JsCast as _;
use web_sys::{Element, Node, Range};

/// 別の行に置いた数式の表。複数行の式は左辺と右辺が別の <math> なので、表ごと 1 つの数式とみなす。
const BLOCK_SELECTORS: [&str; 2] = [".ltx_equationgroup", ".ltx_equation"];

/// 数式 1 つの LaTeX。
fn tex_of(math: &Element) -> Option<String> {
    let tex = math
        .get_attribute("alttext")
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            math.query_selector("annotation[encoding=\"application/x-tex\"]")
                .ok()
                .flatten()
                .and_then(|a| a.text_content())
        })?;
    Some(clean_tex(&tex)).filter(|t| !t.is_empty())
}

fn elements(list: Result<web_sys::NodeList, wasm_bindgen::JsValue>) -> Vec<Element> {
    let Ok(list) = list else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|i| list.item(i))
        .filter_map(|n| n.dyn_into::<Element>().ok())
        .collect()
}

/// 本文で押した要素に当たる数式。返すのは (印を付ける要素, LaTeX)。
/// 別の行の数式は、式の番号や余白を押しても、式の全体になる。
pub fn at(target: &Element) -> Option<(Element, String)> {
    for selector in BLOCK_SELECTORS {
        let Ok(Some(block)) = target.closest(selector) else {
            continue;
        };
        let tex = elements(block.query_selector_all("math"))
            .iter()
            .filter_map(tex_of)
            .collect::<Vec<_>>()
            .join(" ");
        if !tex.is_empty() && tex.chars().count() <= MAX_TEXT_CHARS {
            return Some((block, tex));
        }
    }
    let math = target.closest("math").ok().flatten()?;
    let tex = tex_of(&math)?;
    Some((math, tex))
}

/// `root` の中の <math> を、LaTeX の文字に置き換える（複製した要素に使う）。
fn replace_math(maths: Vec<Element>, wrap: impl Fn(&str) -> String) {
    for math in maths {
        let (Some(parent), Some(doc)) = (math.parent_node(), math.owner_document()) else {
            continue;
        };
        let text = tex_of(&math).map(|t| wrap(&t)).unwrap_or_default();
        let _ = parent.replace_child(&doc.create_text_node(&text), &math);
    }
}

fn remove_all(list: Vec<Element>) {
    for el in list {
        el.remove();
    }
}

/// 数式を含む要素の文字列（数式は `$…$` の LaTeX）。数式が無ければ None。
pub fn element_text(el: &Element) -> Option<String> {
    el.query_selector("math").ok().flatten()?;
    let clone: Element = el.clone_node_with_deep(true).ok()?.dyn_into().ok()?;
    remove_all(elements(clone.query_selector_all(".ltx_note_outer")));
    replace_math(elements(clone.query_selector_all("math")), inline_tex);
    clone.text_content()
}

fn math_ancestor(node: &Node) -> Option<Element> {
    let el = match node.dyn_ref::<Element>() {
        Some(el) => el.clone(),
        None => node.parent_element()?,
    };
    el.closest("math").ok().flatten()
}

/// 数式にかかる選択の内容。
pub enum Selected {
    /// 選んだのは数式 1 つだけ（LaTeX）
    Formula(String),
    /// 数式を含む文（数式は `$…$` の LaTeX）
    Text(String),
}

/// 選択が数式にかかっていれば、数式を LaTeX にした内容。かかっていなければ None。
/// 数式の途中で切れた選択は、数式の全体まで広げて読む。
pub fn selected(range: &Range) -> Option<Selected> {
    let range = range.clone_range();
    if let Some(math) = math_ancestor(&range.start_container().ok()?) {
        range.set_start_before(&math).ok()?;
    }
    if let Some(math) = math_ancestor(&range.end_container().ok()?) {
        range.set_end_after(&math).ok()?;
    }
    let fragment = range.clone_contents().ok()?;
    let maths = elements(fragment.query_selector_all("math"));
    if maths.is_empty() {
        return None;
    }
    remove_all(elements(fragment.query_selector_all(".ltx_note_outer")));
    let single = (maths.len() == 1).then(|| tex_of(&maths[0])).flatten();
    if let Some(tex) = single {
        // 数式のほかに、句読点と空白しか選んでいなければ、数式の解説にする
        let probe: web_sys::DocumentFragment =
            fragment.clone_node_with_deep(true).ok()?.dyn_into().ok()?;
        remove_all(elements(probe.query_selector_all("math")));
        let rest = probe.text_content().unwrap_or_default();
        if rest
            .chars()
            .all(|c| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '\u{200b}'))
        {
            return Some(Selected::Formula(tex));
        }
    }
    replace_math(maths, inline_tex);
    fragment.text_content().map(Selected::Text)
}
