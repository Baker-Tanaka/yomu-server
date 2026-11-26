//! コマンドパレット（⌘K / Ctrl+K）。打った文字で操作や論文を絞り込み、キーだけで実行する。
//!
//! 出す項目はアプリが渡す。このモジュールは入力欄と一覧の表示、キーとクリックの扱いだけを受け持つ。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, Event, HtmlElement, HtmlInputElement, KeyboardEvent};

const ID: &str = "command-palette";
const INPUT_ID: &str = "command-palette-input";
const LIST_ID: &str = "command-palette-list";
const INDEX_ATTR: &str = "data-index";
/// 日本語入力の変換中に押したキーの keyCode
const KEYCODE_COMPOSING: u32 = 229;

/// 一覧の 1 項目。`T` はアプリが決める操作の型。
#[derive(Debug, Clone)]
pub struct Item<T> {
    /// 右に小さく出す分類（「移動」「節」「論文」など）
    pub group: &'static str,
    pub label: String,
    pub action: T,
}

fn document() -> Option<Document> {
    web_sys::window()?.document()
}

fn by_id<T: wasm_bindgen::JsCast>(id: &str) -> Option<T> {
    document()?.get_element_by_id(id)?.dyn_into().ok()
}

/// 入力中の文字に合う項目を返す関数。
type ItemSource<T> = Box<dyn Fn(&str) -> Vec<Item<T>>>;

struct Palette<T> {
    items: ItemSource<T>,
    run: Box<dyn Fn(T)>,
    shown: RefCell<Vec<Item<T>>>,
    selected: Cell<usize>,
}

impl<T: Clone + 'static> Palette<T> {
    fn is_open() -> bool {
        by_id::<HtmlElement>(ID).is_some_and(|el| !el.hidden())
    }

    fn open(&self) {
        let (Some(root), Some(input)) = (
            by_id::<HtmlElement>(ID),
            by_id::<HtmlInputElement>(INPUT_ID),
        ) else {
            return;
        };
        input.set_value("");
        root.set_hidden(false);
        self.refresh();
        let _ = input.focus();
    }

    fn close() {
        if let Some(root) = by_id::<HtmlElement>(ID) {
            root.set_hidden(true);
        }
        if let Some(input) = by_id::<HtmlElement>(INPUT_ID) {
            let _ = input.blur();
        }
    }

    /// 入力に合わせて一覧を作り直し、先頭を選ぶ。
    fn refresh(&self) {
        let query = by_id::<HtmlInputElement>(INPUT_ID)
            .map(|i| i.value())
            .unwrap_or_default();
        let items = (self.items)(query.trim());
        self.selected.set(0);
        render(&items);
        *self.shown.borrow_mut() = items;
        self.mark();
    }

    /// 選んでいる項目に印を付け、見える位置にする。
    fn mark(&self) {
        let Some(list) = by_id::<Element>(LIST_ID) else {
            return;
        };
        let Ok(rows) = list.query_selector_all(&format!("[{INDEX_ATTR}]")) else {
            return;
        };
        for i in 0..rows.length() {
            let Some(row) = rows.item(i).and_then(|n| n.dyn_into::<HtmlElement>().ok()) else {
                continue;
            };
            let on = i as usize == self.selected.get();
            let _ = row.set_attribute("aria-selected", if on { "true" } else { "false" });
            if on {
                let (top, bottom) = (row.offset_top(), row.offset_top() + row.offset_height());
                if top < list.scroll_top() {
                    list.set_scroll_top(top);
                } else if bottom > list.scroll_top() + list.client_height() {
                    list.set_scroll_top(bottom - list.client_height());
                }
            }
        }
    }

    fn step(&self, delta: isize) {
        let len = self.shown.borrow().len();
        if len == 0 {
            return;
        }
        let next = (self.selected.get() as isize + delta).rem_euclid(len as isize);
        self.selected.set(next as usize);
        self.mark();
    }

    fn run_at(&self, index: usize) {
        let action = self.shown.borrow().get(index).map(|i| i.action.clone());
        if let Some(action) = action {
            Self::close();
            (self.run)(action);
        }
    }
}

fn render<T>(items: &[Item<T>]) -> Option<()> {
    let doc = document()?;
    let list = by_id::<Element>(LIST_ID)?;
    list.set_text_content(None);
    if items.is_empty() {
        let empty = doc.create_element("div").ok()?;
        empty.set_class_name("palette-empty");
        empty.set_text_content(Some("当てはまる項目はありません"));
        list.append_child(&empty).ok()?;
        return Some(());
    }
    for (i, item) in items.iter().enumerate() {
        let row = doc.create_element("button").ok()?;
        let _ = row.set_attribute("type", "button");
        let _ = row.set_attribute("role", "option");
        let _ = row.set_attribute(INDEX_ATTR, &i.to_string());
        let label = doc.create_element("span").ok()?;
        label.set_class_name("palette-label");
        label.set_text_content(Some(&item.label));
        let group = doc.create_element("span").ok()?;
        group.set_class_name("palette-group");
        group.set_text_content(Some(item.group));
        row.append_child(&label).ok()?;
        row.append_child(&group).ok()?;
        list.append_child(&row).ok()?;
    }
    Some(())
}

fn build(doc: &Document) -> Option<(HtmlElement, HtmlInputElement, Element)> {
    let root: HtmlElement = doc.create_element("div").ok()?.dyn_into().ok()?;
    root.set_id(ID);
    root.set_hidden(true);
    let _ = root.set_attribute("lang", "ja");
    let dialog = doc.create_element("div").ok()?;
    dialog.set_class_name("palette-box");
    let _ = dialog.set_attribute("role", "dialog");
    let _ = dialog.set_attribute("aria-label", "コマンドパレット");
    let input: HtmlInputElement = doc.create_element("input").ok()?.dyn_into().ok()?;
    input.set_id(INPUT_ID);
    input.set_type("text");
    for (name, value) in [
        (
            "placeholder",
            "操作・節・論文を絞り込む、または arXiv を検索",
        ),
        ("aria-label", "コマンドパレット"),
        ("autocapitalize", "off"),
        ("autocomplete", "off"),
        ("spellcheck", "false"),
    ] {
        let _ = input.set_attribute(name, value);
    }
    let list = doc.create_element("div").ok()?;
    list.set_id(LIST_ID);
    let _ = list.set_attribute("role", "listbox");
    dialog.append_child(&input).ok()?;
    dialog.append_child(&list).ok()?;
    root.append_child(&dialog).ok()?;
    doc.body()?.append_child(&root).ok()?;
    Some((root, input, list))
}

/// アプリから開くための関数。
pub type Opener = Box<dyn Fn()>;

/// パレットを作り、⌘K / Ctrl+K を監視する。`can_open` が false のあいだは開かない。
/// リスナーはページの寿命と同じなので、クロージャは意図的に解放しない。
pub fn init<T: Clone + 'static>(
    can_open: impl Fn() -> bool + 'static,
    items: impl Fn(&str) -> Vec<Item<T>> + 'static,
    run: impl Fn(T) + 'static,
) -> Result<Opener, String> {
    let doc = document().ok_or("document が取得できません")?;
    let (root, input, list) = build(&doc).ok_or("コマンドパレットを作れません")?;
    let palette = Rc::new(Palette {
        items: Box::new(items),
        run: Box::new(run),
        shown: RefCell::new(Vec::new()),
        selected: Cell::new(0),
    });
    let can_open = Rc::new(can_open);

    // egui や論文側のショートカットより先に受け取る（キャプチャ段階）
    let on_keydown = {
        let palette = palette.clone();
        let can_open = can_open.clone();
        Closure::<dyn FnMut(KeyboardEvent)>::new(move |ev: KeyboardEvent| {
            let key = ev.key();
            let toggle = (ev.meta_key() || ev.ctrl_key())
                && !ev.alt_key()
                && !ev.shift_key()
                && key.eq_ignore_ascii_case("k");
            if toggle {
                ev.prevent_default();
                ev.stop_propagation();
                if Palette::<T>::is_open() {
                    Palette::<T>::close();
                } else if can_open() {
                    palette.open();
                }
                return;
            }
            if !Palette::<T>::is_open() {
                return;
            }
            // 開いている間のキーは、ほかのショートカットに渡さない
            ev.stop_propagation();
            if ev.is_composing() || ev.key_code() == KEYCODE_COMPOSING {
                return;
            }
            match key.as_str() {
                "ArrowDown" => palette.step(1),
                "ArrowUp" => palette.step(-1),
                "Enter" => palette.run_at(palette.selected.get()),
                "Escape" => Palette::<T>::close(),
                // フォーカスを入力欄の外に出さない
                "Tab" => {}
                _ => return,
            }
            ev.prevent_default();
        })
    };
    web_sys::window()
        .ok_or("window が取得できません")?
        .add_event_listener_with_callback_and_bool(
            "keydown",
            on_keydown.as_ref().unchecked_ref(),
            true,
        )
        .map_err(|_| "keydown を購読できません")?;
    on_keydown.forget();

    let on_input = {
        let palette = palette.clone();
        Closure::<dyn FnMut()>::new(move || palette.refresh())
    };
    input
        .add_event_listener_with_callback("input", on_input.as_ref().unchecked_ref())
        .map_err(|_| "input を購読できません")?;
    on_input.forget();

    let on_list = {
        let palette = palette.clone();
        Closure::<dyn FnMut(Event)>::new(move |ev: Event| {
            let index = ev
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|el| el.closest(&format!("[{INDEX_ATTR}]")).ok().flatten())
                .and_then(|el| el.get_attribute(INDEX_ATTR))
                .and_then(|v| v.parse().ok());
            if let Some(index) = index {
                palette.run_at(index);
            }
        })
    };
    list.add_event_listener_with_callback("click", on_list.as_ref().unchecked_ref())
        .map_err(|_| "click を購読できません")?;
    on_list.forget();

    // 背景を押したら閉じる
    let on_backdrop = {
        let root = root.clone();
        Closure::<dyn FnMut(Event)>::new(move |ev: Event| {
            let on_background = ev
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .is_some_and(|el| el.id() == root.id());
            if on_background {
                Palette::<T>::close();
            }
        })
    };
    root.add_event_listener_with_callback("click", on_backdrop.as_ref().unchecked_ref())
        .map_err(|_| "click を購読できません")?;
    on_backdrop.forget();

    Ok(Box::new(move || {
        if can_open() {
            palette.open();
        }
    }))
}
