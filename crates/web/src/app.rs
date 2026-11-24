//! egui の解説 UI。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use eframe::egui::{self, Margin, RichText};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use paper_reader_shared::{
    DictEntry, HistoryEntry, Mode, SearchItem, SearchSort, paper_url_from_input,
};

use crate::dom;
use crate::state::{
    self, DictStatus, ExplainPhase, HealthStatus, HistoryScope, HistoryStatus, PaperStatus, Shared,
    SharedRef, Tab,
};
use crate::theme::{self, Palette};

pub struct PaperReaderApp {
    shared: SharedRef,
    input: String,
    markdown_cache: CommonMarkCache,
    theme: dom::ThemeChoice,
}

impl PaperReaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        theme::install(&cc.egui_ctx);

        let shared: SharedRef = Rc::new(RefCell::new(Shared::default()));

        let ctx = cc.egui_ctx.clone();
        let shared_for_dom = shared.clone();
        if let Err(e) = dom::watch_selection(move |info| {
            state::on_select(&shared_for_dom, &ctx, info);
        }) {
            log::error!("選択の監視を開始できません: {e}");
        }
        let ctx = cc.egui_ctx.clone();
        let shared_for_keys = shared.clone();
        if let Err(e) = dom::watch_shortcuts(move |key| {
            state::on_shortcut(&shared_for_keys, &ctx, key);
        }) {
            log::error!("ショートカットを有効にできません: {e}");
        }
        if let Err(e) = dom::init_splitter() {
            log::error!("ペイン境界のドラッグを有効にできません: {e}");
        }
        state::refresh_health(&shared, &cc.egui_ctx);

        let theme = dom::load_theme();
        set_theme(&cc.egui_ctx, theme);

        Self {
            shared,
            input: String::new(),
            markdown_cache: CommonMarkCache::default(),
            theme,
        }
    }

    // ───────────── ヘッダー ─────────────

    fn header(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            logo(ui, &p);
            ui.label(
                RichText::new("論文リーダー")
                    .size(17.0)
                    .strong()
                    .color(p.strong),
            );
            ui.label(RichText::new("for arXiv").size(11.5).color(p.weak));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (label, next, tip) = match self.theme {
                    dom::ThemeChoice::System => (
                        "◐ 自動",
                        dom::ThemeChoice::Light,
                        "OS の設定に合わせています。クリックでライトに",
                    ),
                    dom::ThemeChoice::Light => {
                        ("☀ ライト", dom::ThemeChoice::Dark, "クリックでダークに")
                    }
                    dom::ThemeChoice::Dark => (
                        "☾ ダーク",
                        dom::ThemeChoice::System,
                        "クリックで自動（OS の設定）に",
                    ),
                };
                if theme::subtle_button(ui, label).on_hover_text(tip).clicked() {
                    self.theme = next;
                    dom::apply_theme(next);
                    set_theme(ui.ctx(), next);
                }
                self.llm_status(ui, &p);
            });
        });
    }

    /// LLM の状態（丸と短い文言。詳しくはホバーで、クリックで再確認）。
    fn llm_status(&self, ui: &mut egui::Ui, p: &Palette) {
        let (color, text, detail) = match &self.shared.borrow().health {
            HealthStatus::Checking => (
                p.weak,
                "確認中".to_string(),
                "LLM の状態を確認しています".to_string(),
            ),
            HealthStatus::Error(e) => (
                p.error,
                "接続エラー".into(),
                format!("サーバーに接続できません: {e}"),
            ),
            HealthStatus::Ok(h) => match (h.llm, h.model_available) {
                (Some(true), Some(true)) => {
                    (p.ok, "LLM 準備完了".into(), format!("モデル: {}", h.model))
                }
                (Some(true), _) => (
                    p.error,
                    "モデルなし".into(),
                    format!(
                        "LLM は起動していますが、モデル「{}」がありません（ollama pull {}）",
                        h.model, h.model
                    ),
                ),
                _ => (
                    p.error,
                    "LLM 停止中".into(),
                    "LLM ランタイムに接続できません（Ollama が起動しているか確認してください）"
                        .into(),
                ),
            },
        };
        // 丸 + 文言の角丸ラベルを、内容の大きさぴったりに描く
        let galley = ui
            .painter()
            .layout_no_wrap(text, theme::body_font(12.0), p.text);
        let size = egui::vec2(9.0 + 8.0 + 5.0 + galley.size().x + 10.0, 24.0);
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, 12.0, color.gamma_multiply(0.14));
        painter.circle_filled(egui::pos2(rect.left() + 13.0, rect.center().y), 4.0, color);
        painter.galley(
            egui::pos2(rect.left() + 22.0, rect.center().y - galley.size().y / 2.0),
            galley,
            p.text,
        );
        let resp = resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!("{detail}\nクリックで再確認"));
        if resp.clicked() {
            state::refresh_health(&self.shared, ui.ctx());
        }
    }

    // ───────────── 入力欄と論文 ─────────────

    fn input_bar(&mut self, ui: &mut egui::Ui) {
        let loading = matches!(self.shared.borrow().paper, PaperStatus::Loading);
        let is_paper = paper_url_from_input(&self.input).is_some();
        let label = if is_paper { "開く" } else { "検索" };
        let mut submit = false;
        ui.horizontal(|ui| {
            let button_width = 64.0;
            let edit = ui.add_sized(
                [ui.available_width() - button_width - 8.0, 34.0],
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text("arXiv の URL・ID、またはキーワードで検索")
                    .margin(Margin::symmetric(10, 8)),
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            ui.scope(|ui| {
                ui.set_min_size(egui::vec2(button_width, 34.0));
                let enabled = !self.input.trim().is_empty() && !(is_paper && loading);
                if theme::primary_button(ui, label, enabled)
                    .on_hover_text(if is_paper {
                        "この論文を開きます"
                    } else {
                        "arXiv で論文を検索します"
                    })
                    .clicked()
                {
                    submit = true;
                }
            });
        });
        if submit && !(is_paper && loading) {
            state::submit_input(&self.shared, ui.ctx(), &self.input);
        }
    }

    fn paper_card(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let paper = self.shared.borrow().paper.clone();
        match paper {
            PaperStatus::Empty => {}
            PaperStatus::Loading => {
                ui.add_space(8.0);
                theme::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("論文を読み込み中…").color(p.weak));
                    });
                });
            }
            PaperStatus::Loaded {
                id,
                title,
                source_url,
            } => {
                ui.add_space(8.0);
                theme::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        theme::chip(ui, &format!("arXiv:{id}"), p.accent, p.accent_soft);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.hyperlink_to(RichText::new("原文 ↗").size(12.5), source_url);
                        });
                    });
                    ui.add_space(2.0);
                    ui.label(RichText::new(title).size(15.5).strong().color(p.strong));
                });
            }
            PaperStatus::Error(e) => {
                ui.add_space(8.0);
                notice(ui, &p, &e);
            }
        }
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        let mut tab = self.shared.borrow().tab;
        let search_label = {
            let s = self.shared.borrow();
            if s.search.items.is_empty() {
                "検索".to_string()
            } else {
                format!("検索 {}", s.search.items.len())
            }
        };
        let changed = theme::segmented(
            ui,
            &mut tab,
            &[
                (Tab::Explain, "解説"),
                (Tab::Search, search_label.as_str()),
                (Tab::History, "履歴"),
            ],
        );
        if changed {
            let mut s = self.shared.borrow_mut();
            s.tab = tab;
            if tab == Tab::History {
                s.history_stale = true;
            }
        }
    }

    // ───────────── 解説タブ ─────────────

    fn explain_tab(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let (sel, has_paper) = {
            let s = self.shared.borrow();
            (s.selection.clone(), s.paper_ref().is_some())
        };
        let Some(sel) = sel else {
            empty_explain(ui, &p, has_paper);
            return;
        };

        // 選択中のテキスト
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                match sel.mode {
                    Mode::Word => theme::chip(ui, "単語", p.word, p.word_soft),
                    Mode::Sentence => theme::chip(ui, "英文", p.sentence, p.sentence_soft),
                };
                ui.label(RichText::new("選択中").size(12.0).color(p.weak));
            });
            ui.add_space(2.0);
            match sel.mode {
                Mode::Word => {
                    ui.label(RichText::new(&sel.text).size(22.0).strong().color(p.strong));
                }
                Mode::Sentence => {
                    ui.label(RichText::new(&sel.text).size(15.5).color(p.strong));
                }
            }
            if sel.context != sel.text {
                ui.add_space(2.0);
                egui::CollapsingHeader::new(RichText::new("段落（文脈）").size(12.5).color(p.weak))
                    .id_salt("context")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.label(RichText::new(&sel.context).size(13.5).color(p.text));
                    });
            }
        });

        // 辞書
        if sel.mode == Mode::Word {
            ui.add_space(10.0);
            theme::card(ui, |ui| {
                theme::section_title(ui, "辞書");
                dict_view(ui, &p, &self.shared.borrow().dict);
            });
        }

        ui.add_space(10.0);
        self.explain_card(ui, &p);
    }

    fn explain_card(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        let (phase, running, from_cache, markdown) = {
            let s = self.shared.borrow();
            (
                s.explain.phase.clone(),
                s.explain.phase.is_running(),
                s.explain.from_cache,
                s.explain.markdown.clone(),
            )
        };

        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section_title(ui, "AI 解説");
                if from_cache && phase == ExplainPhase::Done {
                    theme::chip(ui, "キャッシュ", p.weak, p.card_stroke)
                        .on_hover_text("保存済みの解説を表示しています");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if running {
                        if theme::subtle_button(ui, "停止").clicked() {
                            state::stop_explain(&self.shared);
                        }
                    } else if phase == ExplainPhase::Done {
                        if theme::subtle_button(ui, "↻ 再生成")
                            .on_hover_text("キャッシュを使わずに解説を作り直します")
                            .clicked()
                        {
                            state::start_explain(&self.shared, &ctx, true);
                        }
                    } else if theme::primary_button(ui, "解説する  E", true)
                        .on_hover_text("和訳・文構造・文法を解説します（E キー）")
                        .clicked()
                    {
                        state::start_explain(&self.shared, &ctx, false);
                    }
                });
            });

            match &phase {
                ExplainPhase::Idle => {
                    ui.label(
                        RichText::new("「解説する」を押すと、和訳や文法の解説がここに流れます。")
                            .size(13.5)
                            .color(p.weak),
                    );
                }
                ExplainPhase::Connecting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("解説を準備中…").color(p.weak));
                    });
                }
                ExplainPhase::Queued => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("順番待ち中…（ほかの解説を生成しています）")
                                .color(p.weak),
                        );
                    });
                }
                ExplainPhase::Streaming | ExplainPhase::Done => {}
                ExplainPhase::Error(msg) => {
                    notice(ui, p, msg);
                    if theme::subtle_button(ui, "再試行").clicked() {
                        state::start_explain(&self.shared, &ctx, false);
                    }
                }
            }

            if !markdown.is_empty() {
                ui.add_space(2.0);
                CommonMarkViewer::new().show(ui, &mut self.markdown_cache, &markdown);
                if phase == ExplainPhase::Streaming {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("生成中…").size(12.5).color(p.weak));
                    });
                }
            }

            ui.add_space(4.0);
            ui.separator();
            let mut auto = self.shared.borrow().auto_explain;
            if ui
                .checkbox(&mut auto, RichText::new("選択したら自動で解説").size(13.0))
                .changed()
            {
                self.shared.borrow_mut().auto_explain = auto;
            }
        });
    }

    // ───────────── 検索タブ ─────────────

    fn search_tab(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let ctx = ui.ctx().clone();
        let search = self.shared.borrow().search.clone();

        if search.query.is_empty() {
            empty_search(ui, &p);
            return;
        }

        ui.horizontal(|ui| {
            let count = if search.loading && search.items.is_empty() {
                "検索中…".to_string()
            } else {
                format!("約 {} 件", group_digits(search.total))
            };
            ui.label(
                RichText::new(format!("「{}」", search.query))
                    .strong()
                    .color(p.strong),
            );
            ui.label(RichText::new(count).size(12.5).color(p.weak));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut sort = search.sort;
                if theme::segmented(
                    ui,
                    &mut sort,
                    &[
                        (SearchSort::Newest, "新着"),
                        (SearchSort::Relevance, "関連度"),
                    ],
                ) {
                    state::search(&self.shared, &ctx, search.query.clone(), sort);
                }
            });
        });
        ui.add_space(6.0);

        if let Some(e) = &search.error {
            notice(ui, &p, e);
            if theme::subtle_button(ui, "再試行").clicked() {
                if search.items.is_empty() {
                    state::search(&self.shared, &ctx, search.query.clone(), search.sort);
                } else {
                    state::search_more(&self.shared, &ctx);
                }
            }
            ui.add_space(6.0);
        }

        if search.items.is_empty() {
            if search.loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("arXiv を検索しています…").color(p.weak));
                });
            } else if search.error.is_none() {
                ui.label(
                    RichText::new(
                        "見つかりませんでした。語を減らすか、英語で入力してみてください。",
                    )
                    .color(p.weak),
                );
            }
            return;
        }

        let current = self.shared.borrow().paper_ref().map(|(id, _)| id);
        let mut open = None;
        for item in &search.items {
            if let Some(id) = search_result(ui, &p, item, current.as_deref() == Some(&item.id)) {
                open = Some(id);
            }
            ui.add_space(8.0);
        }
        if search.has_more() {
            ui.vertical_centered(|ui| {
                if search.loading {
                    ui.spinner();
                } else if theme::subtle_button(ui, "さらに表示").clicked() {
                    state::search_more(&self.shared, &ctx);
                }
            });
        }
        if let Some(id) = open {
            state::open_paper(&self.shared, &ctx, format!("https://arxiv.org/abs/{id}"));
        }
    }

    // ───────────── 履歴タブ ─────────────

    fn history_tab(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let ctx = ui.ctx().clone();
        let (scope, has_paper, stale) = {
            let s = self.shared.borrow();
            (s.history_scope, s.paper_ref().is_some(), s.history_stale)
        };
        if stale {
            state::refresh_history(&self.shared, &ctx);
        }

        let mut new_scope = scope;
        ui.horizontal(|ui| {
            theme::segmented(
                ui,
                &mut new_scope,
                &[
                    (HistoryScope::ThisPaper, "この論文"),
                    (HistoryScope::AllPapers, "すべて"),
                ],
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::subtle_button(ui, "↻ 更新").clicked() {
                    self.shared.borrow_mut().history_stale = true;
                }
            });
        });
        if new_scope != scope {
            let mut s = self.shared.borrow_mut();
            s.history_scope = new_scope;
            s.history_stale = true;
        }
        ui.add_space(8.0);

        let status = self.shared.borrow().history.clone();
        let entries = match status {
            HistoryStatus::NotLoaded | HistoryStatus::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("履歴を読み込み中…").color(p.weak));
                });
                return;
            }
            HistoryStatus::Error(e) => {
                notice(ui, &p, &format!("履歴: {e}"));
                return;
            }
            HistoryStatus::Loaded(entries) => entries,
        };
        if entries.is_empty() {
            ui.label(
                RichText::new(match (scope, has_paper) {
                    (HistoryScope::ThisPaper, false) => {
                        "論文を開くと、その論文で調べた単語と解説した文がここに並びます。"
                    }
                    _ => "まだ履歴はありません。単語を選ぶか、英文を解説するとここに残ります。",
                })
                .color(p.weak),
            );
            return;
        }

        let current = self.shared.borrow().paper_ref().map(|(id, _)| id);
        let mut action = None;
        let mut last_paper: Option<&str> = None;
        for entry in &entries {
            if scope == HistoryScope::AllPapers && last_paper != Some(entry.paper_id.as_str()) {
                last_paper = Some(&entry.paper_id);
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&entry.paper_title).strong().color(p.strong));
                    if current.as_deref() != Some(entry.paper_id.as_str())
                        && ui
                            .link(RichText::new("開く →").size(12.5))
                            .on_hover_text(format!("arXiv:{} を開きます", entry.paper_id))
                            .clicked()
                    {
                        action = Some(HistoryAction::OpenPaper(entry.paper_id.clone()));
                    }
                });
                ui.add_space(2.0);
            }
            if let Some(a) = history_row(ui, &p, entry) {
                action = Some(a);
            }
            ui.add_space(6.0);
        }

        match action {
            Some(HistoryAction::Show(entry)) => {
                state::show_history_entry(&self.shared, &ctx, &entry)
            }
            Some(HistoryAction::Delete(id)) => state::delete_history_entry(&self.shared, &ctx, id),
            Some(HistoryAction::OpenPaper(id)) => {
                state::open_paper(&self.shared, &ctx, format!("https://arxiv.org/abs/{id}"));
            }
            None => {}
        }
    }
}

impl eframe::App for PaperReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 解説ペインにフォーカスがあるときのショートカット（論文ペイン側は dom::watch_shortcuts）
        let ctx = ui.ctx().clone();
        if !ctx.egui_wants_keyboard_input() {
            let (explain, clear) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::E) && i.modifiers.is_none(),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if explain {
                state::on_shortcut(&self.shared, &ctx, dom::Shortcut::Explain);
            } else if clear {
                state::on_shortcut(&self.shared, &ctx, dom::Shortcut::ClearSelection);
            }
        }

        let p = theme::palette(&ctx);
        egui::CentralPanel::default_margins()
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(16, 14)),
            )
            .show(ui, |ui| {
                self.header(ui);
                ui.add_space(12.0);
                self.input_bar(ui);
                self.paper_card(ui);
                ui.add_space(12.0);
                self.tabs(ui);
                ui.add_space(10.0);
                let tab = self.shared.borrow().tab;
                egui::ScrollArea::vertical()
                    .id_salt(tab_salt(tab))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        match tab {
                            Tab::Explain => self.explain_tab(ui),
                            Tab::Search => self.search_tab(ui),
                            Tab::History => self.history_tab(ui),
                        }
                        ui.add_space(16.0);
                    });
            });
    }
}

fn tab_salt(tab: Tab) -> &'static str {
    match tab {
        Tab::Explain => "explain_scroll",
        Tab::Search => "search_scroll",
        Tab::History => "history_scroll",
    }
}

/// ロゴ（アクセント色の角丸に「読」）。
fn logo(ui: &mut egui::Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 7.0, p.accent);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "読",
        theme::body_font(15.0),
        p.on_accent,
    );
}

/// エラーなどのお知らせ（赤みのある帯）。
fn notice(ui: &mut egui::Ui, p: &Palette, text: &str) {
    egui::Frame::new()
        .fill(p.error_soft)
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(p.error));
        });
}

/// 選択前の案内。
fn empty_explain(ui: &mut egui::Ui, p: &Palette, has_paper: bool) {
    theme::card(ui, |ui| {
        ui.add_space(4.0);
        if has_paper {
            ui.label(
                RichText::new("テキストを選んで読み解く")
                    .size(17.0)
                    .strong()
                    .color(p.strong),
            );
        } else {
            ui.label(
                RichText::new("まずは論文を開きましょう")
                    .size(17.0)
                    .strong()
                    .color(p.strong),
            );
            ui.label(
                RichText::new(
                    "上の欄に arXiv の URL・ID（例: 1706.03762）か、キーワードを入れてください。",
                )
                .color(p.weak),
            );
        }
        ui.add_space(8.0);
        for (key, text) in [
            ("ダブルクリック", "単語 → 辞書の訳と AI の解説"),
            ("ドラッグ", "英文 → 和訳・文構造・文法"),
            ("E", "選択中のテキストを解説"),
            ("Esc", "選択を解除"),
        ] {
            ui.horizontal(|ui| {
                theme::chip(ui, key, p.accent, p.accent_soft);
                ui.label(RichText::new(text).size(13.5).color(p.text));
            });
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("iPhone などでは長押しで選択できます。")
                .size(12.5)
                .color(p.weak),
        );
    });
}

/// 検索前の案内。
fn empty_search(ui: &mut egui::Ui, p: &Palette) {
    theme::card(ui, |ui| {
        ui.label(
            RichText::new("arXiv の論文を検索")
                .size(17.0)
                .strong()
                .color(p.strong),
        );
        ui.label(
            RichText::new("上の欄にキーワードを入れて「検索」を押してください。").color(p.weak),
        );
        ui.add_space(8.0);
        for (example, text) in [
            ("attention is all you need", "題名で探す"),
            ("\"in-context learning\"", "語句で探す"),
            ("au:Hinton ti:capsule", "著者・題名を指定"),
            ("cat:cs.CL retrieval", "分野を指定"),
        ] {
            ui.horizontal(|ui| {
                theme::chip(ui, example, p.accent, p.accent_soft);
                ui.label(RichText::new(text).size(13.0).color(p.weak));
            });
        }
    });
}

/// 検索結果の 1 件。「読む」かタイトルが押されたら論文 ID を返す。
fn search_result(
    ui: &mut egui::Ui,
    p: &Palette,
    item: &SearchItem,
    is_open: bool,
) -> Option<String> {
    let mut open = false;
    theme::card(ui, |ui| {
        let title = ui
            .add(
                egui::Label::new(
                    RichText::new(&item.title)
                        .size(15.0)
                        .strong()
                        .color(p.strong),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if title.hovered() {
            ui.painter().line_segment(
                [title.rect.left_bottom(), title.rect.right_bottom()],
                egui::Stroke::new(1.0, p.accent),
            );
        }
        open |= title.clicked();

        ui.label(
            RichText::new(authors_line(&item.authors))
                .size(12.5)
                .color(p.text),
        );
        ui.horizontal_wrapped(|ui| {
            if !item.primary_category.is_empty() {
                theme::chip(ui, &item.primary_category, p.accent, p.accent_soft);
            }
            ui.label(RichText::new(&item.published).size(12.0).color(p.weak));
            ui.label(
                RichText::new(format!("arXiv:{}", item.id))
                    .size(12.0)
                    .color(p.weak),
            );
        });

        egui::CollapsingHeader::new(RichText::new("概要").size(12.5).color(p.weak))
            .id_salt(("abstract", &item.id))
            .default_open(false)
            .show(ui, |ui| {
                ui.label(RichText::new(&item.summary).size(13.5).color(p.text));
            });

        ui.horizontal(|ui| {
            if is_open {
                theme::chip(ui, "表示中", p.ok, p.ok.gamma_multiply(0.15));
            } else if theme::primary_button(ui, "読む", true).clicked() {
                open = true;
            }
            ui.hyperlink_to(
                RichText::new("arXiv ↗").size(12.5),
                format!("https://arxiv.org/abs/{}", item.id),
            );
        });
    });
    (open && !is_open).then(|| item.id.clone())
}

/// "A, B, C ほか 5 名"
fn authors_line(authors: &[String]) -> String {
    const SHOW: usize = 3;
    match authors.len() {
        0 => "著者不明".into(),
        n if n <= SHOW => authors.join(", "),
        n => format!("{} ほか {} 名", authors[..SHOW].join(", "), n - SHOW),
    }
}

/// 12345 → "12,345"
fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

enum HistoryAction {
    Show(HistoryEntry),
    Delete(i64),
    OpenPaper(String),
}

/// 履歴の 1 行。クリックで再表示、× で削除。
fn history_row(ui: &mut egui::Ui, p: &Palette, entry: &HistoryEntry) -> Option<HistoryAction> {
    let mut action = None;
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            match entry.mode {
                Mode::Word => theme::chip(ui, "単語", p.word, p.word_soft),
                Mode::Sentence => theme::chip(ui, "英文", p.sentence, p.sentence_soft),
            };
            ui.label(
                RichText::new(format_time(entry.updated_at))
                    .size(12.0)
                    .color(p.weak),
            );
            if entry.has_explanation {
                ui.label(RichText::new("✓ 解説あり").size(12.0).color(p.ok));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(RichText::new("×").color(p.weak)).frame(false))
                    .on_hover_text("履歴から削除")
                    .clicked()
                {
                    action = Some(HistoryAction::Delete(entry.id));
                }
            });
        });
        let preview: String = entry.text.chars().take(160).collect();
        let preview = if preview.len() < entry.text.len() {
            format!("{preview}…")
        } else {
            preview
        };
        let text = match entry.mode {
            Mode::Word => RichText::new(preview).size(17.0).strong().color(p.strong),
            Mode::Sentence => RichText::new(preview).size(14.0).color(p.text),
        };
        let resp = ui
            .add(egui::Label::new(text).sense(egui::Sense::click()))
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("クリックで再表示");
        if resp.clicked() {
            action = Some(HistoryAction::Show(entry.clone()));
        }
    });
    action
}

/// UNIX 秒をローカル時刻の "11/24 09:15" にする。
fn format_time(secs: i64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(secs as f64 * 1000.0));
    format!(
        "{}/{} {:02}:{:02}",
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes()
    )
}

/// 辞書の結果（見出し語と訳のリスト）。
fn dict_view(ui: &mut egui::Ui, p: &Palette, status: &DictStatus) {
    match status {
        DictStatus::Idle => {}
        DictStatus::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("辞書を検索中…").color(p.weak));
            });
        }
        DictStatus::Error(e) => notice(ui, p, &format!("辞書: {e}")),
        DictStatus::Loaded(r) => match &r.headword {
            None => {
                ui.label(
                    RichText::new(format!("「{}」は辞書に見つかりませんでした。", r.query))
                        .color(p.weak),
                );
            }
            Some(headword) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(headword).size(21.0).strong().color(p.strong));
                    if *headword != r.query {
                        ui.label(
                            RichText::new(format!("{} の原形", r.query))
                                .size(12.5)
                                .color(p.weak),
                        );
                    }
                });
                meanings_list(ui, p, &r.meanings);
                for entry in &r.related {
                    related_view(ui, p, entry);
                }
            }
        },
    }
}

fn related_view(ui: &mut egui::Ui, p: &Palette, entry: &DictEntry) {
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("原形の候補").size(12.5).color(p.weak));
        ui.label(
            RichText::new(&entry.headword)
                .size(17.0)
                .strong()
                .color(p.strong),
        );
    });
    meanings_list(ui, p, &entry.meanings);
}

/// 訳を番号付きで並べる。元データの『』で囲まれた重要な訳は強調する。
fn meanings_list(ui: &mut egui::Ui, p: &Palette, meanings: &[String]) {
    let font = theme::body_font(14.5);
    for (i, meaning) in meanings.iter().enumerate() {
        let mut job = egui::text::LayoutJob::default();
        job.append(
            &format!("{}  ", i + 1),
            0.0,
            egui::TextFormat::simple(font.clone(), p.accent),
        );
        for (text, emphasized) in split_emphasis(meaning) {
            let mut format =
                egui::TextFormat::simple(font.clone(), if emphasized { p.strong } else { p.text });
            if emphasized {
                format.background = p.accent_soft;
            }
            job.append(text, 0.0, format);
        }
        ui.label(job);
    }
}

/// "『走る』,駆ける" → [("走る", true), (",駆ける", false)]
fn split_emphasis(s: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find('『') {
        if start > 0 {
            out.push((&rest[..start], false));
        }
        let after = &rest[start + '『'.len_utf8()..];
        match after.find('』') {
            Some(end) => {
                out.push((&after[..end], true));
                rest = &after[end + '』'.len_utf8()..];
            }
            None => {
                rest = after;
                break;
            }
        }
    }
    if !rest.is_empty() {
        out.push((rest, false));
    }
    out
}

/// egui の配色を切り替える（System なら OS の設定に従う）。
fn set_theme(ctx: &egui::Context, theme: dom::ThemeChoice) {
    ctx.set_theme(match theme {
        dom::ThemeChoice::System => egui::ThemePreference::System,
        dom::ThemeChoice::Light => egui::ThemePreference::Light,
        dom::ThemeChoice::Dark => egui::ThemePreference::Dark,
    });
}

/// Noto Sans JP を追加して日本語を表示できるようにする。
fn install_fonts(ctx: &egui::Context) {
    const NAME: &str = "noto_sans_jp";
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        NAME.to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/NotoSansJP-Regular.otf"
        ))),
    );
    // 既定の欧文フォントの次に置き、和文はこちらで表示する
    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        list.insert(1.min(list.len()), NAME.to_owned());
    }
    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        list.push(NAME.to_owned());
    }
    ctx.set_fonts(fonts);
}
