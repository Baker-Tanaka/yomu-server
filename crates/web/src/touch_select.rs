//! タッチ端末で、本文の選択をアプリで行う。
//!
//! iOS の標準の選択を使うかぎり「コピー／調べる」のメニューは Web ページから消せないので、
//! 本文の標準の選択を止め（`user-select: none`）、長押しで単語を選び、そのままのドラッグか
//! 両端のつまみで範囲を広げる。出るのはアプリの操作メニュー（`selection_menu.rs`）だけになる。
//!
//! 文字の位置は画面上の矩形から自分で調べ（選択できない文字でも確実に引けるように）、
//! 選択の色は本文に重ねた要素で描く。範囲は DOM の Range で持つので、本文の要素が分かれたり
//! つながったりしても位置はずれない。
//!
//! 設定でオフにしたときと、マウスで使う端末では、ブラウザの標準の選択のまま。

use std::cell::{Cell, RefCell};

use paper_reader_shared::word_at;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{
    AddEventListenerOptions, Document, DomRect, Element, Event, HtmlElement, Node, Range,
    ResizeObserver, Text, Touch, TouchEvent,
};

use crate::dom;

const SETTING_KEY: &str = "paper-reader.app-select";
/// 有効なあいだ <body> に付けるクラス（paper.css が標準の選択を止める）
const BODY_CLASS: &str = "app-select";
const LAYER_ID: &str = "app-select-layer";
const MARKS_ID: &str = "app-select-marks";
const HANDLE_CLASS: &str = "app-select-handle";
/// 長押しとみなすまでの時間
const LONG_PRESS_MS: i32 = 450;
/// 長押しを待つあいだ、これより動けばスクロールとみなす（px）
const MOVE_SLOP_PX: f64 = 10.0;
/// 行間を押しても拾えるように、文字の矩形の上下に足す幅（px）
const LINE_SLACK_PX: f64 = 5.0;
/// 範囲を広げながら本文の端に来たら、この幅の中では少しずつ送る（px）
const SCROLL_EDGE_PX: f64 = 36.0;
const SCROLL_STEP_PX: f64 = 10.0;
/// DOM の Range.compareBoundaryPoints の比べ方
const START_TO_START: u16 = 0;
const END_TO_END: u16 = 2;

/// 指の操作の状態。
enum Gesture {
    Idle,
    /// 触れたまま。動かなければ長押しになる
    Pending {
        x: f64,
        y: f64,
    },
    /// 長押しのまま動かして範囲を広げている（`anchor` は最初の単語）
    Extending {
        anchor: Range,
    },
    /// つまみを動かしている。`fixed` は動かさない側の端、(dx, dy) は指から文字までのずれ
    Handle {
        fixed: Range,
        dx: f64,
        dy: f64,
    },
}

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static SELECTION: RefCell<Option<Range>> = const { RefCell::new(None) };
    static GESTURE: RefCell<Gesture> = const { RefCell::new(Gesture::Idle) };
    static PRESS_TIMER: Cell<Option<i32>> = const { Cell::new(None) };
}

fn document() -> Option<Document> {
    web_sys::window()?.document()
}

fn paper() -> Option<Element> {
    document()?.get_element_by_id("paper")
}

/// 指で操作する端末か。
pub fn is_touch_device() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media("(pointer: coarse)").ok().flatten())
        .is_some_and(|m| m.matches())
}

/// 設定の値（既定はオン）。
pub fn setting() -> bool {
    dom::load_bool(SETTING_KEY, true)
}

pub fn set_setting(on: bool) {
    dom::save_bool(SETTING_KEY, on);
    apply(on);
}

fn apply(on: bool) {
    let active = on && is_touch_device();
    ACTIVE.set(active);
    // 切り替えの前の選択は残さない
    dom::clear_selection();
    if let Some(body) = document().and_then(|d| d.body()) {
        let _ = body.class_list().toggle_with_force(BODY_CLASS, active);
    }
}

/// アプリで選んでいる範囲。
pub fn range() -> Option<Range> {
    SELECTION
        .with(|s| s.borrow().clone())
        .filter(|r| !r.collapsed())
        .filter(|r| {
            r.common_ancestor_container()
                .is_ok_and(|n| n.is_connected())
        })
}

/// 選択をやめて、色とつまみを消す。
pub fn remove() {
    SELECTION.with(|s| *s.borrow_mut() = None);
    if let Some(layer) = document().and_then(|d| d.get_element_by_id(LAYER_ID)) {
        layer.remove();
    }
}

/// 範囲に含まれるテキストノードと、その中の範囲 `[start, end)`（表示されない注釈は除く）。
fn text_parts(range: &Range) -> Vec<(Text, u32, u32)> {
    let (Ok(root), Ok(start_node), Ok(end_node), Ok(start), Ok(end)) = (
        range.common_ancestor_container(),
        range.start_container(),
        range.end_container(),
        range.start_offset(),
        range.end_offset(),
    ) else {
        return Vec::new();
    };
    let mut nodes = Vec::new();
    match root.dyn_ref::<Text>() {
        Some(text) => nodes.push(text.clone()),
        None => dom::collect_text_nodes(&root, &mut nodes),
    }
    nodes
        .into_iter()
        .filter(|node| range.intersects_node(node).unwrap_or(false))
        .filter(|node| !in_hidden_note(node))
        .filter_map(|node| {
            let s = if start_node.is_same_node(Some(&node)) {
                start
            } else {
                0
            };
            let e = if end_node.is_same_node(Some(&node)) {
                end
            } else {
                node.length()
            };
            (s < e).then_some((node, s, e))
        })
        .collect()
}

/// 開いていない脚注の中の文字か。
fn in_hidden_note(node: &Node) -> bool {
    node.parent_element()
        .and_then(|el| el.closest(".ltx_note_outer").ok().flatten())
        .is_some()
}

/// 範囲の文字列。表や段落をまたぐときは、区切りに空白を入れる。
pub fn range_text(range: &Range) -> String {
    let mut out = String::new();
    let mut last_block: Option<Element> = None;
    for (node, s, e) in text_parts(range) {
        let block = node.parent_element().and_then(|el| {
            el.closest("p, li, td, th, h1, h2, h3, h4, h5, h6, figcaption, div")
                .ok()
                .flatten()
        });
        if !out.is_empty() && block != last_block {
            out.push(' ');
        }
        last_block = block;
        if let Ok(part) = node.substring_data(s, e - s) {
            out.push_str(&part);
        }
    }
    out
}

/// 範囲の文字が画面に占める矩形（文書順）。
fn text_rects(range: &Range) -> Vec<DomRect> {
    let Some(probe) = document().and_then(|d| d.create_range().ok()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (node, s, e) in text_parts(range) {
        if probe.set_start(&node, s).is_err() || probe.set_end(&node, e).is_err() {
            continue;
        }
        let Some(rects) = probe.get_client_rects() else {
            continue;
        };
        out.extend(
            (0..rects.length())
                .filter_map(|i| rects.item(i))
                .filter(|r| r.width() > 0.5 && r.height() > 0.5),
        );
    }
    out
}

/// 選択全体を囲む矩形（左, 上, 右, 下。画面の座標）。
pub fn bounds() -> Option<(f64, f64, f64, f64)> {
    text_rects(&range()?)
        .iter()
        .map(|r| (r.left(), r.top(), r.right(), r.bottom()))
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

fn contains_point(r: &DomRect, x: f64, y: f64) -> bool {
    r.left() <= x
        && x <= r.right()
        && r.top() - LINE_SLACK_PX <= y
        && y <= r.bottom() + LINE_SLACK_PX
}

/// 画面の (x, y) にある単語の範囲。数式の上なら数式の全体。
fn word_range_at(x: f64, y: f64) -> Option<Range> {
    let doc = document()?;
    let paper = paper()?;
    // 重ねている選択の色やつまみは飛ばして、本文の要素を探す
    let element = doc
        .elements_from_point(x as f32, y as f32)
        .iter()
        .filter_map(|v| v.dyn_into::<Element>().ok())
        .find(|el| {
            paper.contains(Some(el)) && el.closest(&format!("#{LAYER_ID}")).ok().flatten().is_none()
        })?;
    if element == paper {
        return None;
    }
    let range = doc.create_range().ok()?;
    if let Ok(Some(math)) = element.closest("math") {
        let mut nodes = Vec::new();
        dom::collect_text_nodes(&math, &mut nodes);
        let (first, last) = (nodes.first()?, nodes.last()?);
        range.set_start(first, 0).ok()?;
        range.set_end(last, last.length()).ok()?;
        return Some(range);
    }
    let mut nodes = Vec::new();
    dom::collect_text_nodes(&element, &mut nodes);
    for node in nodes {
        let data = node.data();
        if data.trim().is_empty() || !node_covers(&range, &node, x, y) {
            continue;
        }
        let mut offset = 0u32;
        for c in data.chars() {
            let width = c.len_utf16() as u32;
            range.set_start(&node, offset).ok()?;
            range.set_end(&node, offset + width).ok()?;
            if contains_point(&range.get_bounding_client_rect(), x, y)
                && let Some((s, e)) = word_at(&data, offset)
            {
                range.set_start(&node, s).ok()?;
                range.set_end(&node, e).ok()?;
                return Some(range);
            }
            offset += width;
        }
    }
    None
}

/// テキストノードのどれかの行が (x, y) を含むか（1 文字ずつ調べる前の絞り込み）。
fn node_covers(range: &Range, node: &Text, x: f64, y: f64) -> bool {
    if range.select_node_contents(node).is_err() {
        return false;
    }
    range.get_client_rects().is_some_and(|rects| {
        (0..rects.length())
            .filter_map(|i| rects.item(i))
            .any(|r| contains_point(&r, x, y))
    })
}

/// 2 つの範囲の両方を含む範囲。
fn union(a: &Range, b: &Range) -> Option<Range> {
    let out = a.clone_range();
    if b.compare_boundary_points(START_TO_START, a).ok()? < 0 {
        out.set_start(&b.start_container().ok()?, b.start_offset().ok()?)
            .ok()?;
    }
    if b.compare_boundary_points(END_TO_END, a).ok()? > 0 {
        out.set_end(&b.end_container().ok()?, b.end_offset().ok()?)
            .ok()?;
    }
    Some(out)
}

fn set_selection(range: Range) {
    SELECTION.with(|s| *s.borrow_mut() = Some(range));
    draw();
}

fn create_div(doc: &Document, parent: &Element, id: Option<&str>, class: &str) -> Option<Element> {
    let div = doc.create_element("div").ok()?;
    if let Some(id) = id {
        div.set_id(id);
    }
    div.set_class_name(class);
    parent.append_child(&div).ok()?;
    Some(div)
}

/// 本文の中に重ねる層（本文と一緒にスクロールする）。
/// つまみは触れている間に作り直すと指の動きが届かなくなるので、一度作って位置だけ変える。
fn layer() -> Option<Element> {
    let doc = document()?;
    if let Some(layer) = doc.get_element_by_id(LAYER_ID) {
        return Some(layer);
    }
    let layer = create_div(&doc, &paper()?, Some(LAYER_ID), "")?;
    let _ = layer.set_attribute("aria-hidden", "true");
    create_div(&doc, &layer, Some(MARKS_ID), "")?;
    for end in ["start", "end"] {
        let handle = create_div(&doc, &layer, None, HANDLE_CLASS)?;
        let _ = handle.set_attribute("data-end", end);
    }
    Some(layer)
}

fn place(el: &Element, left: f64, top: f64, width: Option<f64>, height: f64) {
    let Some(el) = el.dyn_ref::<HtmlElement>() else {
        return;
    };
    let style = el.style();
    let _ = style.set_property("left", &format!("{left:.1}px"));
    let _ = style.set_property("top", &format!("{top:.1}px"));
    if let Some(width) = width {
        let _ = style.set_property("width", &format!("{width:.1}px"));
    }
    let _ = style.set_property("height", &format!("{height:.1}px"));
}

/// 選択の色とつまみを描き直す。
pub fn draw() {
    let Some(range) = range() else {
        return;
    };
    let rects = text_rects(&range);
    let (Some(first), Some(last), Some(paper), Some(layer), Some(doc)) =
        (rects.first(), rects.last(), paper(), layer(), document())
    else {
        return;
    };
    let area = paper.get_bounding_client_rect();
    // 画面の座標から、本文の中身の座標へ
    let ox = f64::from(paper.scroll_left()) - area.left();
    let oy = f64::from(paper.scroll_top()) - area.top();
    let Ok(Some(marks)) = layer.query_selector(&format!("#{MARKS_ID}")) else {
        return;
    };
    marks.set_inner_html("");
    for r in &rects {
        if let Some(mark) = create_div(&doc, &marks, None, "app-select-mark") {
            place(
                &mark,
                r.left() + ox,
                r.top() + oy,
                Some(r.width()),
                r.height(),
            );
        }
    }
    for (end, x, rect) in [("start", first.left(), first), ("end", last.right(), last)] {
        if let Ok(Some(handle)) = layer.query_selector(&format!("[data-end=\"{end}\"]")) {
            place(&handle, x + ox, rect.top() + oy, None, rect.height());
        }
    }
}

fn cancel_press_timer() {
    if let (Some(handle), Some(window)) = (PRESS_TIMER.take(), web_sys::window()) {
        window.clear_timeout_with_handle(handle);
    }
}

fn reset_gesture() {
    cancel_press_timer();
    GESTURE.with(|g| *g.borrow_mut() = Gesture::Idle);
}

fn long_press() {
    PRESS_TIMER.set(None);
    let Some((x, y)) = GESTURE.with(|g| match *g.borrow() {
        Gesture::Pending { x, y } => Some((x, y)),
        _ => None,
    }) else {
        return;
    };
    let Some(word) = word_range_at(x, y) else {
        return;
    };
    crate::selection_menu::hide();
    set_selection(word.clone_range());
    GESTURE.with(|g| *g.borrow_mut() = Gesture::Extending { anchor: word });
}

fn start_press(x: f64, y: f64) {
    GESTURE.with(|g| *g.borrow_mut() = Gesture::Pending { x, y });
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once_into_js(long_press);
    if let Ok(handle) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.unchecked_ref(),
        LONG_PRESS_MS,
    ) {
        PRESS_TIMER.set(Some(handle));
    }
}

/// つまみに触れた。動かさない側の端と、指から文字までのずれを覚える。
fn start_handle(end: &str, x: f64, y: f64) -> Option<()> {
    let current = range()?;
    let rects = text_rects(&current);
    let moving_start = end == "start";
    let rect = if moving_start {
        rects.first()?
    } else {
        rects.last()?
    };
    let target_x = if moving_start {
        rect.left() + 2.0
    } else {
        rect.right() - 2.0
    };
    let fixed = current.clone_range();
    // 開始側を動かすときは終わりの位置、終了側を動かすときは始めの位置を残す
    fixed.collapse_with_to_start(!moving_start);
    let (dx, dy) = (target_x - x, rect.top() + rect.height() / 2.0 - y);
    crate::selection_menu::hide();
    GESTURE.with(|g| *g.borrow_mut() = Gesture::Handle { fixed, dx, dy });
    Some(())
}

fn scroll_near_edge(y: f64) {
    let Some(paper) = paper() else {
        return;
    };
    let area = paper.get_bounding_client_rect();
    let step = if y > area.bottom() - SCROLL_EDGE_PX {
        SCROLL_STEP_PX
    } else if y < area.top() + SCROLL_EDGE_PX {
        -SCROLL_STEP_PX
    } else {
        return;
    };
    paper.scroll_by_with_x_and_y(0.0, step);
}

fn first_touch(ev: &TouchEvent) -> Option<Touch> {
    ev.touches().get(0)
}

fn on_touch_start(ev: &TouchEvent) {
    if !ACTIVE.get() {
        return;
    }
    reset_gesture();
    // 2 本指（拡大など）はブラウザに任せる
    if ev.touches().length() != 1 {
        return;
    }
    let Some(touch) = first_touch(ev) else {
        return;
    };
    let (x, y) = (f64::from(touch.client_x()), f64::from(touch.client_y()));
    let handle_end = ev
        .target()
        .and_then(|t| t.dyn_into::<Element>().ok())
        .and_then(|el| el.closest(&format!(".{HANDLE_CLASS}")).ok().flatten())
        .and_then(|el| el.get_attribute("data-end"));
    match handle_end {
        Some(end) => {
            ev.prevent_default();
            let _ = start_handle(&end, x, y);
        }
        None => start_press(x, y),
    }
}

fn on_touch_move(ev: &TouchEvent) {
    let Some(touch) = first_touch(ev) else {
        return;
    };
    let (x, y) = (f64::from(touch.client_x()), f64::from(touch.client_y()));
    let next = GESTURE.with(|g| match &*g.borrow() {
        Gesture::Idle => None,
        Gesture::Pending { x: px, y: py } => {
            if (x - px).hypot(y - py) > MOVE_SLOP_PX {
                Some(None)
            } else {
                None
            }
        }
        Gesture::Extending { anchor } => Some(Some((anchor.clone(), x, y))),
        Gesture::Handle { fixed, dx, dy } => Some(Some((fixed.clone(), x + dx, y + dy))),
    });
    match next {
        None => {}
        // 動いたのでスクロール
        Some(None) => reset_gesture(),
        Some(Some((base, tx, ty))) => {
            // 範囲を広げているあいだは本文を指で動かさない
            ev.prevent_default();
            scroll_near_edge(ty);
            if let Some(selection) = word_range_at(tx, ty).and_then(|w| union(&base, &w))
                && !selection.collapsed()
            {
                set_selection(selection);
            }
        }
    }
}

fn on_touch_end(ev: &TouchEvent) {
    cancel_press_timer();
    let gesture = GESTURE.with(|g| std::mem::replace(&mut *g.borrow_mut(), Gesture::Idle));
    match gesture {
        Gesture::Idle => {}
        // ふつうのタップ: 選択の外なら選択をやめる（リンクなどはそのまま働く）
        Gesture::Pending { x, y } => {
            if ev.type_() == "touchend"
                && range().is_some_and(|r| !text_rects(&r).iter().any(|b| contains_point(b, x, y)))
            {
                dom::clear_selection();
            }
        }
        Gesture::Extending { .. } | Gesture::Handle { .. } => {
            // 長押しのあとの click（リンクを開くなど）を起こさない
            if ev.cancelable() {
                ev.prevent_default();
            }
            dom::commit_app_selection();
        }
    }
}

fn listen(target: &Element, kind: &str, handler: fn(&TouchEvent)) -> Result<(), String> {
    let closure = Closure::<dyn FnMut(TouchEvent)>::new(move |ev: TouchEvent| handler(&ev));
    // touchmove と touchstart で preventDefault するので passive にしない
    let options = AddEventListenerOptions::new();
    options.set_passive(false);
    target
        .add_event_listener_with_callback_and_add_event_listener_options(
            kind,
            closure.as_ref().unchecked_ref(),
            &options,
        )
        .map_err(|_| format!("{kind} を購読できません"))?;
    closure.forget();
    Ok(())
}

/// 本文の指の操作を監視し、保存した設定を反映する。
/// リスナーはページの寿命と同じなので、クロージャは意図的に解放しない。
pub fn init() -> Result<(), String> {
    let paper = paper().ok_or("#paper が見つかりません")?;
    listen(&paper, "touchstart", on_touch_start)?;
    listen(&paper, "touchmove", on_touch_move)?;
    listen(&paper, "touchend", on_touch_end)?;
    listen(&paper, "touchcancel", on_touch_end)?;

    // Android などの長押しメニューも出さない
    let on_contextmenu = Closure::<dyn FnMut(Event)>::new(|ev: Event| {
        if ACTIVE.get() {
            ev.prevent_default();
        }
    });
    paper
        .add_event_listener_with_callback("contextmenu", on_contextmenu.as_ref().unchecked_ref())
        .map_err(|_| "contextmenu を購読できません")?;
    on_contextmenu.forget();

    // 横に長い表の中のスクロールや、本文の幅の変化で文字の位置がずれたら描き直す
    let on_inner_scroll = Closure::<dyn FnMut(Event)>::new(|ev: Event| {
        let paper_itself = ev
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .is_some_and(|el| el.id() == "paper");
        if !paper_itself {
            draw();
        }
    });
    paper
        .add_event_listener_with_callback_and_bool(
            "scroll",
            on_inner_scroll.as_ref().unchecked_ref(),
            true,
        )
        .map_err(|_| "scroll を購読できません")?;
    on_inner_scroll.forget();
    let on_resize = Closure::<dyn FnMut()>::new(draw);
    let observer = ResizeObserver::new(on_resize.as_ref().unchecked_ref())
        .map_err(|_| "本文の大きさを監視できません")?;
    observer.observe(&paper);
    on_resize.forget();

    apply(setting());
    Ok(())
}
