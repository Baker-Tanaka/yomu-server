//! 本文で選択した語・文のすぐそばに出す操作メニュー（`#selection-menu`）。
//! 解説ペインまで指やマウスを運ばずに、解説・付箋・コピー・発音ができる。

use std::cell::{Cell, RefCell};

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, Event, HtmlElement, PointerEvent, ResizeObserver};

use crate::dom;

const MENU_ID: &str = "selection-menu";
/// ボタンの操作の種類を持つ属性
const ACTION_ATTR: &str = "data-action";
/// 選択とのすき間（px）。下に出すときは、スマホの選択のつまみを避けて広めに空ける
const GAP_ABOVE_PX: f64 = 8.0;
const GAP_BELOW_PX: f64 = 14.0;
const GAP_BELOW_TOUCH_PX: f64 = 28.0;
const EDGE_PX: f64 = 8.0;
/// 選択が消えてから閉じるまでの時間。スマホではボタンを押すと先に選択が消えるので、押せるように待つ
const HIDE_DELAY_MS: i32 = 350;
/// スマホで選択の範囲を調整している間はメニューを隠し、この時間変わらなければ出す
const SETTLE_DELAY_MS: i32 = 600;
/// 「コピーしました」を見せてから閉じるまでの時間
const COPIED_MS: i32 = 900;

/// メニューのボタンで、アプリ（解説ペイン）に頼む操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Explain,
    Bookmark,
}

thread_local! {
    /// 表示中のメニューが対象にしている語・文
    static TEXT: RefCell<String> = const { RefCell::new(String::new()) };
    /// 閉じる予定のタイマー
    static HIDE_TIMER: Cell<Option<i32>> = const { Cell::new(None) };
    /// 範囲の調整が落ち着いたら出し直すタイマー（Some の間は隠しておく）
    static SETTLE_TIMER: Cell<Option<i32>> = const { Cell::new(None) };
}

fn document() -> Option<Document> {
    web_sys::window()?.document()
}

fn menu() -> Option<HtmlElement> {
    document()?
        .get_element_by_id(MENU_ID)
        .and_then(|e| e.dyn_into().ok())
}

pub fn hide() {
    cancel_hide();
    cancel_settle();
    if let Some(menu) = menu() {
        let _ = menu.set_attribute("hidden", "");
    }
}

fn cancel_hide() {
    if let (Some(handle), Some(window)) = (HIDE_TIMER.take(), web_sys::window()) {
        window.clear_timeout_with_handle(handle);
    }
}

fn hide_after(ms: i32) {
    cancel_hide();
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once_into_js(|| {
        HIDE_TIMER.set(None);
        hide();
    });
    if let Ok(handle) =
        window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), ms)
    {
        HIDE_TIMER.set(Some(handle));
    }
}

fn cancel_settle() {
    if let (Some(handle), Some(window)) = (SETTLE_TIMER.take(), web_sys::window()) {
        window.clear_timeout_with_handle(handle);
    }
}

/// 選択が変わっている間は隠し、落ち着いてから出し直す。
/// つまみを動かして範囲を調整するとき、つまみの近くにメニューがあると邪魔になるため。
fn settle() {
    cancel_settle();
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once_into_js(|| {
        SETTLE_TIMER.set(None);
        place();
    });
    if let Ok(handle) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.unchecked_ref(),
        SETTLE_DELAY_MS,
    ) {
        SETTLE_TIMER.set(Some(handle));
        if let Some(menu) = menu() {
            let _ = menu.style().set_property("visibility", "hidden");
        }
    }
}

/// 選択が確定した語・文のそばに出す。
pub fn show(text: &str) {
    cancel_hide();
    TEXT.with(|t| *t.borrow_mut() = text.to_string());
    let Some(menu) = menu() else {
        return;
    };
    set_copy_label(&menu, "コピー");
    // 数式（LaTeX）は読み上げない
    if let Ok(Some(button)) = menu.query_selector(&format!("[{ACTION_ATTR}=\"speak\"]")) {
        let formula = dom::selected_mode() == Some(paper_reader_shared::Mode::Formula);
        let _ = button.toggle_attribute_with_force("hidden", formula);
    }
    let _ = menu.remove_attribute("hidden");
    place();
}

/// 本文の選択の位置に合わせる（スクロールしたとき）。選択が画面の外なら隠したままにする。
fn place() {
    let Some(menu) = menu().filter(|m| !m.has_attribute("hidden")) else {
        return;
    };
    let style = menu.style();
    let Some((selection, area)) = dom::selection_rect() else {
        return;
    };
    let visible = SETTLE_TIMER.get().is_none()
        && selection.bottom() > area.top()
        && selection.top() < area.bottom();
    let _ = style.set_property("visibility", if visible { "visible" } else { "hidden" });

    let (width, height) = (
        f64::from(menu.offset_width()),
        f64::from(menu.offset_height()),
    );
    let center = selection.left() + selection.width() / 2.0;
    let left = (center - width / 2.0)
        .min(area.right() - EDGE_PX - width)
        .max(area.left() + EDGE_PX);
    // スマホは選択の上に OS のメニューが出るので、下に出す。PC も下に出し、入らなければ上
    let gap = if dom::is_mobile() {
        GAP_BELOW_TOUCH_PX
    } else {
        GAP_BELOW_PX
    };
    let below = selection.bottom() + gap;
    let top = if below + height <= area.bottom() - EDGE_PX {
        below
    } else {
        (selection.top() - GAP_ABOVE_PX - height).max(area.top() + EDGE_PX)
    };
    let _ = style.set_property("left", &format!("{left:.0}px"));
    let _ = style.set_property("top", &format!("{top:.0}px"));
}

fn set_copy_label(menu: &Element, label: &str) {
    if let Ok(Some(button)) = menu.query_selector(&format!("[{ACTION_ATTR}=\"copy\"]")) {
        button.set_text_content(Some(label));
    }
}

/// 選択中の語・文をクリップボードに入れる（ボタンを押した処理の中で呼ぶ）。
fn copy(text: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    if !window.is_secure_context() {
        if let Some(menu) = menu() {
            set_copy_label(&menu, "コピーできません");
        }
        return;
    }
    let promise = window.navigator().clipboard().write_text(text);
    wasm_bindgen_futures::spawn_local(async move {
        let ok = wasm_bindgen_futures::JsFuture::from(promise).await.is_ok();
        if let Some(menu) = menu() {
            set_copy_label(
                &menu,
                if ok {
                    "✓ コピーしました"
                } else {
                    "コピーできません"
                },
            );
        }
        hide_after(COPIED_MS);
    });
}

fn build(doc: &Document) -> Option<Element> {
    let menu = doc.create_element("div").ok()?;
    menu.set_id(MENU_ID);
    let _ = menu.set_attribute("role", "toolbar");
    let _ = menu.set_attribute("aria-label", "選択したテキストの操作");
    let _ = menu.set_attribute("hidden", "");
    let mut buttons = vec![
        ("explain", "解説する"),
        ("bookmark", "📌 付箋"),
        ("copy", "コピー"),
    ];
    if dom::can_speak() {
        buttons.push(("speak", "🔊 発音"));
    }
    for (action, label) in buttons {
        let button = doc.create_element("button").ok()?;
        let _ = button.set_attribute("type", "button");
        let _ = button.set_attribute(ACTION_ATTR, action);
        button.set_text_content(Some(label));
        menu.append_child(&button).ok()?;
    }
    doc.body()?.append_child(&menu).ok()?;
    Some(menu)
}

/// メニューを作り、閉じる・追いかける操作を監視する。
/// リスナーはページの寿命と同じなので、クロージャは意図的に解放しない。
pub fn init(on_action: impl Fn(MenuAction) + 'static) -> Result<(), String> {
    let doc = document().ok_or("document が取得できません")?;
    let paper = doc
        .get_element_by_id("paper")
        .ok_or("#paper が見つかりません")?;
    let menu = build(&doc).ok_or("選択のメニューを作れません")?;

    // ボタンを押しても本文の選択が消えないようにする
    let on_pointerdown = Closure::<dyn FnMut(PointerEvent)>::new(|ev: PointerEvent| {
        ev.prevent_default();
        cancel_hide();
    });
    menu.add_event_listener_with_callback("pointerdown", on_pointerdown.as_ref().unchecked_ref())
        .map_err(|_| "pointerdown を購読できません")?;
    on_pointerdown.forget();

    let on_click = Closure::<dyn FnMut(Event)>::new(move |ev: Event| {
        let Some(action) = ev
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|el| el.closest(&format!("[{ACTION_ATTR}]")).ok().flatten())
            .and_then(|el| el.get_attribute(ACTION_ATTR))
        else {
            return;
        };
        cancel_hide();
        let text = TEXT.with(|t| t.borrow().clone());
        // コピーと発音は、ボタンを押した処理の中でないとブラウザが許さない
        match action.as_str() {
            "copy" => copy(&text),
            "speak" => {
                dom::speak(&paper_reader_shared::speakable(&text));
            }
            "explain" => {
                hide();
                on_action(MenuAction::Explain);
            }
            "bookmark" => {
                hide();
                on_action(MenuAction::Bookmark);
            }
            _ => {}
        }
    });
    menu.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())
        .map_err(|_| "click を購読できません")?;
    on_click.forget();

    let on_selectionchange = Closure::<dyn FnMut()>::new(|| {
        if dom::has_paper_selection() {
            // 範囲を調整している間は追いかける
            cancel_hide();
            if dom::is_mobile() {
                settle();
            }
            place();
        } else {
            hide_after(HIDE_DELAY_MS);
        }
    });
    doc.add_event_listener_with_callback(
        "selectionchange",
        on_selectionchange.as_ref().unchecked_ref(),
    )
    .map_err(|_| "selectionchange を購読できません")?;
    on_selectionchange.forget();

    let on_scroll = Closure::<dyn FnMut()>::new(place);
    paper
        .add_event_listener_with_callback("scroll", on_scroll.as_ref().unchecked_ref())
        .map_err(|_| "scroll を購読できません")?;
    // 本文の大きさが変わると（直近の単語の行が現れる、解説シートが動くなど）選択の位置もずれる
    let observer = ResizeObserver::new(on_scroll.as_ref().unchecked_ref())
        .map_err(|_| "本文の大きさを監視できません")?;
    observer.observe(&paper);
    on_scroll.forget();
    Ok(())
}
