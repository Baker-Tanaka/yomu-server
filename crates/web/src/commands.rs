//! コマンドパレットに出す項目と、選ばれた項目の実行。

use eframe::egui;
use paper_reader_shared::{SearchSort, palette_matches, paper_url_from_input};

use crate::command_palette::Item;
use crate::dom::{self, ThemeChoice};
use crate::state::{self, PapersStatus, SearchView, SharedRef, Tab};
use crate::toc;

/// 見た論文・後で読むから出す件数
const MAX_PAPERS: usize = 8;

/// パレットから実行する操作。
#[derive(Debug, Clone)]
pub enum Command {
    ShowTab(Tab),
    ShowReadLater,
    PlaceBookmark,
    JumpBookmark,
    OpenToc,
    Section(usize),
    /// 配色（画面側の状態も変えるので、次の描画でアプリが適用する）
    Theme(ThemeChoice),
    Feedback,
    OpenUrl(String),
    Search(String),
}

fn item(group: &'static str, label: impl Into<String>, action: Command) -> Item<Command> {
    Item {
        group,
        label: label.into(),
        action,
    }
}

/// `query` に合う項目。入力が arXiv の URL・ID なら開く項目を先頭に、検索の項目を最後に出す。
pub fn items(shared: &SharedRef, query: &str) -> Vec<Item<Command>> {
    let s = shared.borrow();
    let mut all = Vec::new();
    if s.paper_ref().is_some() {
        all.push(item("本文", "目次を開く", Command::OpenToc));
        all.push(item(
            "付箋",
            "付箋を貼る（ここまで読んだ）",
            Command::PlaceBookmark,
        ));
        if s.bookmark.is_some() {
            all.push(item("付箋", "付箋へ移動", Command::JumpBookmark));
        }
    }
    for (tab, label) in [
        (Tab::Explain, "解説を表示"),
        (Tab::Search, "検索を表示"),
        (Tab::History, "履歴を表示"),
        (Tab::Grammar, "文法項目を表示"),
        (Tab::Words, "単語帳を表示"),
    ] {
        all.push(item("移動", label, Command::ShowTab(tab)));
    }
    all.push(item("移動", "後で読むを表示", Command::ShowReadLater));
    for (choice, label) in [
        (ThemeChoice::Light, "配色をライトにする"),
        (ThemeChoice::Dark, "配色をダークにする"),
        (ThemeChoice::System, "配色を自動（OS の設定）にする"),
    ] {
        all.push(item("設定", label, Command::Theme(choice)));
    }
    all.push(item("設定", "不具合・要望を送る", Command::Feedback));

    // 節と論文は数が多いので、何も打っていないときは出さない
    if !query.is_empty() {
        for (i, section) in toc::sections().into_iter().enumerate() {
            all.push(item("節へ移る", section.title, Command::Section(i)));
        }
        let open = |id: &str| Command::OpenUrl(format!("https://arxiv.org/abs/{id}"));
        for paper in s.read_later.items.iter().take(MAX_PAPERS) {
            all.push(item("後で読む", paper.title.clone(), open(&paper.id)));
        }
        if let PapersStatus::Loaded(r) = &s.papers {
            for paper in r.papers.iter().take(MAX_PAPERS) {
                all.push(item("見た論文", paper.title.clone(), open(&paper.id)));
            }
        }
    }
    all.retain(|i| palette_matches(query, &format!("{} {}", i.group, i.label)));

    if !query.is_empty() {
        if let Some(url) = paper_url_from_input(query) {
            all.insert(
                0,
                item(
                    "論文",
                    format!("この論文を開く: {query}"),
                    Command::OpenUrl(url),
                ),
            );
        }
        all.push(item(
            "検索",
            format!("「{query}」を arXiv で検索"),
            Command::Search(query.to_string()),
        ));
    }
    all
}

pub fn run(shared: &SharedRef, ctx: &egui::Context, command: Command) {
    // 解説ペインの内容を出す操作は、スマホではシートを上げる
    let reveal = || dom::raise_sheet(dom::Sheet::Half);
    match command {
        Command::ShowTab(tab) => {
            state::show_tab(shared, tab);
            if tab == Tab::Search {
                shared.borrow_mut().search_view = SearchView::Results;
            }
            reveal();
        }
        Command::ShowReadLater => {
            state::show_tab(shared, Tab::Search);
            shared.borrow_mut().search_view = SearchView::Later;
            reveal();
        }
        Command::PlaceBookmark => state::place_bookmark(shared, ctx),
        Command::JumpBookmark => state::jump_to_bookmark(shared),
        Command::OpenToc => toc::open(),
        Command::Section(index) => toc::jump(index),
        Command::Theme(choice) => shared.borrow_mut().requested_theme = Some(choice),
        Command::Feedback => state::open_feedback(shared, ctx),
        Command::OpenUrl(url) => state::open_paper(shared, ctx, url),
        Command::Search(query) => state::search(shared, ctx, query, SearchSort::Relevance),
    }
    ctx.request_repaint();
}
