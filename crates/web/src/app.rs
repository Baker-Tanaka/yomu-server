//! egui の解説 UI。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use eframe::egui::{self, Margin, RichText};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use paper_reader_shared::{
    DictEntry, DictResponse, ExplainTag, FeedbackItem, FeedbackLabel, FeedbackListResponse,
    GrammarMapResponse, GrammarTagInfo, HistoryEntry, MAX_FEEDBACK_BODY_CHARS,
    MAX_FEEDBACK_TITLE_CHARS, Mode, SearchItem, SearchSort, WordSource, paper_url_from_input,
    pos_groups, pos_summary,
};

use crate::dom;
use crate::recent::RecentWord;
use crate::state::{
    self, DictAiPhase, DictStatus, ExplainPhase, FeedbackStage, GrammarOrder, GrammarStatus,
    HealthStatus, HistoryScope, HistoryStatus, PaperStatus, PapersStatus, Shared, SharedRef, Tab,
};
use crate::theme::{self, Palette};

pub struct PaperReaderApp {
    shared: SharedRef,
    input: String,
    markdown_cache: CommonMarkCache,
    theme: dom::ThemeChoice,
    /// フィードバックの画面の中身が変わったか調べるための値（変わったら大きさを測り直す）
    feedback_layout: u64,
    /// 大きさを測り直す残りの描画回数
    feedback_resize_frames: u8,
    /// 前の描画で測ったフィードバックの画面の中身の高さ
    feedback_content_h: f32,
    /// スマホ用レイアウトか（画面幅で決まる。変わったら文字の大きさなどを切り替える）
    mobile: Option<bool>,
}

impl PaperReaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        theme::install(&cc.egui_ctx);

        let shared: SharedRef = Rc::new(RefCell::new(Shared::new()));

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
        // 講座の名前やカテゴリ（チップの表示に使う）
        state::refresh_grammar(&shared, &cc.egui_ctx);

        let theme = dom::load_theme();
        set_theme(&cc.egui_ctx, theme);

        Self {
            shared,
            input: String::new(),
            markdown_cache: CommonMarkCache::default(),
            theme,
            feedback_layout: 0,
            feedback_resize_frames: 0,
            feedback_content_h: 300.0,
            mobile: None,
        }
    }

    // ───────────── ヘッダー ─────────────

    fn is_mobile(&self) -> bool {
        self.mobile.unwrap_or(false)
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let mobile = self.is_mobile();
        ui.horizontal(|ui| {
            logo(ui, &p);
            ui.label(
                RichText::new("論文リーダー")
                    .size(if mobile { 16.0 } else { 17.0 })
                    .strong()
                    .color(p.strong),
            );
            if !mobile {
                ui.label(RichText::new("for arXiv").size(11.5).color(p.weak));
            }

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
                // スマホでは記号だけにして幅を節約する
                let label = if mobile {
                    label.split(' ').next().unwrap_or(label)
                } else {
                    label
                };
                if theme::subtle_button(ui, label).on_hover_text(tip).clicked() {
                    self.theme = next;
                    dom::apply_theme(next);
                    set_theme(ui.ctx(), next);
                }
                self.settings_menu(ui);
                self.bookmark_menu(ui, &p);
                if ui
                    .add(
                        egui::Button::new(RichText::new("💬").size(if mobile {
                            17.0
                        } else {
                            14.0
                        }))
                        .frame(false),
                    )
                    .on_hover_text("不具合・要望を送る（GitHub の Issue になります）")
                    .clicked()
                {
                    state::open_feedback(&self.shared, ui.ctx());
                }
                self.llm_status(ui, &p);
            });
        });
    }

    /// 設定（⚙）。
    fn settings_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button(RichText::new("⚙").size(15.0), |ui| {
            ui.set_min_width(240.0);
            ui.label(RichText::new("設定").strong());
            ui.separator();
            let mut s = self.shared.borrow_mut();
            let mut show = s.show_course;
            if ui
                .checkbox(&mut show, "講座の案内を表示する")
                .on_hover_text(format!(
                    "文法項目のチップに、対応する「{}」のカテゴリ・レッスンを表示します",
                    s.course_short()
                ))
                .changed()
            {
                s.set_show_course(show);
            }
            let mut auto = s.auto_explain;
            if ui.checkbox(&mut auto, "選択したら自動で解説").changed() {
                s.auto_explain = auto;
            }
            let mut kana = s.show_kana;
            if ui
                .checkbox(&mut kana, "単語の読みをカタカナで表示する")
                .on_hover_text(
                    "ローカル LLM が発音をカタカナで推定します（一度推定した語は単語帳に保存され、次からはすぐ出ます）",
                )
                .changed()
            {
                s.set_show_kana(kana);
            }
            let mut recent = s.show_recent;
            if ui
                .checkbox(&mut recent, "直近の単語のメモを表示する")
                .on_hover_text("上の 1 行に、最近引いた 10 語と訳を出します（このブラウザにだけ保存）")
                .changed()
            {
                s.set_show_recent(recent);
            }
            if !s.recent.is_empty() && ui.button("直近の単語のメモを消す").clicked() {
                s.recent.clear();
            }
            drop(s);
            ui.separator();
            if ui.button("💬 不具合・要望を送る…").clicked() {
                state::open_feedback(&self.shared, ui.ctx());
                ui.close();
            }
        })
        .response
        .on_hover_text("設定");
    }

    /// 付箋（📌）。論文を開いているときだけ出す。
    fn bookmark_menu(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let (has_paper, bookmark, error) = {
            let s = self.shared.borrow();
            (
                s.paper_ref().is_some(),
                s.bookmark.clone(),
                s.bookmark_error.clone(),
            )
        };
        if !has_paper {
            return;
        }
        let icon = RichText::new("📌").size(15.0);
        let icon = if bookmark.is_some() {
            icon.color(theme::BOOKMARK)
        } else {
            icon.color(p.weak)
        };
        ui.menu_button(icon, |ui| {
            ui.set_min_width(250.0);
            ui.label(RichText::new("付箋（どこまで読んだか）").strong());
            ui.separator();
            if let Some(b) = &bookmark {
                ui.label(
                    RichText::new(format!("{}  {}…", format_time(b.updated_at), b.snippet))
                        .size(12.5)
                        .color(p.weak),
                );
                if ui.button("↪ 付箋へ移動").clicked() {
                    state::jump_to_bookmark(&self.shared);
                    ui.close();
                }
            }
            let label = if bookmark.is_some() {
                "📌 ここに貼り直す"
            } else {
                "📌 ここまで読んだ（付箋を貼る）"
            };
            if ui
                .button(label)
                .on_hover_text(
                    "本文で選択している段落、無ければ画面のいちばん上の段落に貼ります。次に開くとそこから表示します",
                )
                .clicked()
            {
                state::place_bookmark(&self.shared, ui.ctx());
                ui.close();
            }
            if bookmark.is_some() && ui.button("付箋をはがす").clicked() {
                state::remove_bookmark(&self.shared, ui.ctx());
                ui.close();
            }
            if let Some(e) = &error {
                ui.add_space(4.0);
                ui.label(RichText::new(e).size(12.5).color(p.error));
            }
        })
        .response
        .on_hover_text(if bookmark.is_some() {
            "付箋（貼ってあります）"
        } else {
            "付箋を貼る"
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
        // スマホでは短く「LLM」だけ（詳しくは押すと再確認、色で状態が分かる）
        let text = if self.is_mobile() {
            "LLM".to_string()
        } else {
            text
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

    /// スマホでシートを下げているときの、選んだ単語の小さな和訳（タップで詳しく）。
    fn word_strip(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let (word, dict, ai, show_kana) = {
            let s = self.shared.borrow();
            let Some(sel) = &s.selection else {
                return;
            };
            (
                sel.text.clone(),
                s.dict.clone(),
                s.dict_ai.clone(),
                s.show_kana,
            )
        };
        let mut expand = false;
        ui.horizontal(|ui| {
            let r = match &dict {
                DictStatus::Loaded(r) => Some(r),
                _ => None,
            };
            let head = r
                .map(|r| r.headword.as_deref().unwrap_or(&r.query))
                .unwrap_or(&word);
            ui.label(RichText::new(head).size(18.0).strong().color(p.strong));
            speak_button(ui, p, head);
            if let Some(kana) = r
                .filter(|_| show_kana)
                .and_then(|r| r.note.as_ref())
                .and_then(|n| n.kana.as_deref())
            {
                ui.label(RichText::new(kana).size(13.5).color(p.weak));
            }
            if let Some((head, meanings)) = r.map(entry_meanings) {
                let summary = pos_summary(head, meanings);
                if !summary.is_empty() {
                    let text = summary
                        .iter()
                        .map(|pos| pos.short())
                        .collect::<Vec<_>>()
                        .join("・");
                    ui.label(RichText::new(text).size(13.0).color(p.word))
                        .on_hover_text(POS_HINT);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                expand = theme::subtle_button(ui, "詳しく").clicked();
                if r.is_some_and(|r| {
                    r.headword.is_none()
                        && r.note.as_ref().is_some_and(|n| {
                            !n.meanings.is_empty() && n.meanings_source == Some(WordSource::Llm)
                        })
                }) {
                    theme::chip(ui, "AI 訳", p.sentence, p.sentence_soft);
                }
            });
        });
        let (text, weak) = match (&dict, &ai) {
            (DictStatus::Loading, _) => ("辞書を検索中…".to_string(), true),
            (DictStatus::Error(e), _) => (format!("辞書: {e}"), true),
            (DictStatus::Loaded(r), ai) => match compact_meanings(r) {
                Some(m) => (m, false),
                None if *ai == DictAiPhase::Loading => ("AI で訳しています…".into(), true),
                None => ("辞書に見つかりませんでした".into(), true),
            },
            (DictStatus::Idle, _) => (String::new(), true),
        };
        let resp = ui.add(
            egui::Label::new(RichText::new(text).size(14.5).color(if weak {
                p.weak
            } else {
                p.text
            }))
            .truncate()
            .sense(egui::Sense::click()),
        );
        if expand || resp.clicked() {
            self.shared.borrow_mut().tab = Tab::Explain;
            dom::set_sheet(dom::Sheet::Half);
        }
    }

    /// 直近に引いた単語のメモ。新しい順に横へ並べ、スワイプ（横スクロール）で前の語を見る。
    /// タップでその語をもう一度引く。
    fn recent_strip(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let items: Vec<RecentWord> = {
            let s = self.shared.borrow();
            if !s.show_recent || s.recent.is_empty() {
                return;
            }
            s.recent.items().cloned().collect()
        };
        let current = {
            let s = self.shared.borrow();
            s.selection
                .as_ref()
                .filter(|sel| sel.mode == Mode::Word)
                .map(|sel| sel.text.to_lowercase())
        };
        let mut picked = None;
        egui::ScrollArea::horizontal()
            .id_salt("recent-words")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(RichText::new("最近").size(11.5).color(p.weak))
                        .on_hover_text(
                            "最近引いた単語（このブラウザにだけ保存。⚙ で非表示・消去）",
                        );
                    for item in &items {
                        let active = current.as_deref() == Some(item.word.to_lowercase().as_str());
                        let mut job = egui::text::LayoutJob::default();
                        job.append(
                            &item.word,
                            0.0,
                            egui::TextFormat::simple(
                                theme::body_font(13.0),
                                if active { p.accent } else { p.strong },
                            ),
                        );
                        job.append(
                            &clip_chars(&item.meaning, 14),
                            6.0,
                            egui::TextFormat::simple(theme::body_font(12.0), p.weak),
                        );
                        let resp = egui::Frame::new()
                            .fill(if active { p.accent_soft } else { p.card_stroke })
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(job)
                                        .wrap_mode(egui::TextWrapMode::Extend)
                                        .selectable(false),
                                );
                            })
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text(format!("{}  {}", item.word, item.meaning));
                        if resp.clicked() {
                            picked = Some(item.word.clone());
                        }
                    }
                });
            });
        ui.add_space(6.0);
        if let Some(word) = picked {
            state::look_up_word(&self.shared, ui.ctx(), &word);
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
                // スマホでは出さない（題名は本文の先頭にあり、シートの高さを節約する）
                if self.is_mobile() {
                    let _ = (&id, &title, &source_url);
                    return;
                }
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
                (Tab::Grammar, "文法項目"),
                (Tab::Words, "単語帳"),
            ],
        );
        if changed {
            let mut s = self.shared.borrow_mut();
            s.tab = tab;
            if tab == Tab::Words {
                s.words.stale = true;
            }
            if tab == Tab::History {
                s.history_stale = true;
            }
            if tab == Tab::Grammar {
                s.grammar_stale = true;
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
            empty_explain(ui, &p, has_paper, self.is_mobile());
            if !has_paper {
                // 論文を開く前は、最近見た論文からすぐ開けるようにする
                ui.add_space(10.0);
                self.recent_papers(ui, &p, Some(5));
            }
            return;
        };

        // 選択中のテキスト（スマホでは、ここにも「解説する」を出してすぐ押せるようにする）
        let mobile = self.is_mobile();
        let can_explain = mobile && {
            let s = self.shared.borrow();
            s.explain.phase == ExplainPhase::Idle
        };
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                match sel.mode {
                    Mode::Word => theme::chip(ui, "単語", p.word, p.word_soft),
                    Mode::Sentence => theme::chip(ui, "英文", p.sentence, p.sentence_soft),
                };
                ui.label(RichText::new("選択中").size(12.0).color(p.weak));
                if can_explain {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::primary_button(ui, "解説する", true).clicked() {
                            state::start_explain(&self.shared, ui.ctx(), false);
                        }
                    });
                }
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
                let retry = {
                    let s = self.shared.borrow();
                    dict_view(ui, &p, &s.dict, &s.dict_ai, s.show_kana)
                };
                if retry {
                    state::fetch_dict_ai(&self.shared, ui.ctx());
                }
            });
        }

        ui.add_space(10.0);
        self.explain_card(ui, &p);
    }

    fn explain_card(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        let (phase, running, from_cache, markdown, is_sentence) = {
            let s = self.shared.borrow();
            (
                s.explain.phase.clone(),
                s.explain.phase.is_running(),
                s.explain.from_cache,
                s.explain.markdown.clone(),
                s.explain
                    .target
                    .as_ref()
                    .is_some_and(|(m, _)| *m == Mode::Sentence),
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
                    } else if theme::primary_button(
                        ui,
                        if self.mobile == Some(true) {
                            "解説する"
                        } else {
                            "解説する  E"
                        },
                        true,
                    )
                    .on_hover_text("和訳・文構造・文法を解説します（E キー）")
                    .clicked()
                    {
                        state::start_explain(&self.shared, &ctx, false);
                    }
                });
            });

            if is_sentence && phase != ExplainPhase::Idle {
                self.grammar_chips(ui, p);
            }

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
                            RichText::new("順番待ち中…（LLM がほかの処理をしています）")
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

    /// 文法項目のチップと、選んだ項目の詳細。
    fn grammar_chips(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let (tags, received, selected, found, show_course, course_short, running) = {
            let s = self.shared.borrow();
            (
                s.explain.tags.clone(),
                s.explain.tags_received,
                s.explain.selected_tag,
                s.explain.highlight_found,
                s.show_course,
                s.course_short(),
                s.explain.phase.is_running(),
            )
        };
        if !received {
            if running {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("文法項目を判定中…").size(13.0).color(p.weak));
                });
            }
            return;
        }
        if tags.is_empty() {
            return;
        }
        ui.add_space(2.0);
        let mut clicked = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for (i, tag) in tags.iter().enumerate() {
                let label = chip_label(tag, show_course, &course_short);
                let (fg, bg) = if tag.in_scope || !show_course {
                    (p.accent, p.accent_soft)
                } else {
                    (p.weak, p.card_stroke)
                };
                let is_selected = selected == Some(i);
                let button = egui::Button::new(RichText::new(label).size(12.5).color(fg))
                    .fill(bg)
                    .stroke(if is_selected {
                        egui::Stroke::new(1.5, fg)
                    } else {
                        egui::Stroke::NONE
                    })
                    .corner_radius(egui::CornerRadius::same(13));
                if ui
                    .add(button)
                    .on_hover_text(format!(
                        "{}（クリックで本文の該当箇所と説明を表示）",
                        tag.name
                    ))
                    .clicked()
                {
                    clicked = Some(i);
                }
            }
        });
        if let Some(i) = clicked {
            state::select_tag(&self.shared, i);
        }
        if let Some(tag) = selected.and_then(|i| tags.get(i)) {
            ui.add_space(4.0);
            tag_detail(ui, p, tag, found, show_course, &course_short);
        }
        ui.add_space(4.0);
    }

    // ───────────── 文法項目タブ ─────────────

    fn grammar_tab(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let ctx = ui.ctx().clone();
        let (stale, order, show_course) = {
            let s = self.shared.borrow();
            (s.grammar_stale, s.grammar_order, s.show_course)
        };
        if stale {
            state::refresh_grammar(&self.shared, &ctx);
        }
        let status = self.shared.borrow().grammar.clone();
        let g = match status {
            GrammarStatus::NotLoaded | GrammarStatus::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("文法項目を読み込み中…").color(p.weak));
                });
                return;
            }
            GrammarStatus::Error(e) => {
                notice(ui, &p, &format!("文法項目: {e}"));
                return;
            }
            GrammarStatus::Loaded(g) => g,
        };

        if show_course {
            theme::card(ui, |ui| {
                ui.label(RichText::new(&g.course).size(13.0).strong().color(p.strong));
                let lessons: usize = g.categories.iter().map(|c| c.lesson_count).sum();
                if g.source == "template" || lessons == 0 {
                    ui.label(
                        RichText::new(
                            "レッスン名はまだ記入されていません。data/grammar_map.local.yaml に書き写すと、\
                             ここと解説のチップにレッスン名が出ます（今はカテゴリ単位の案内だけ）。",
                        )
                        .size(12.5)
                        .color(p.weak),
                    );
                } else {
                    ui.label(
                        RichText::new(format!("記入済みのレッスン: {lessons} 件"))
                            .size(12.5)
                            .color(p.weak),
                    );
                }
            });
            ui.add_space(8.0);
        }

        let mut new_order = order;
        ui.horizontal(|ui| {
            theme::segmented(
                ui,
                &mut new_order,
                &[
                    (GrammarOrder::Category, "カテゴリ順"),
                    (GrammarOrder::Frequency, "よく出る順"),
                ],
            );
            let total: u64 = g.tags.iter().map(|t| t.count).sum();
            ui.label(
                RichText::new(format!("解説した英文での出現 計 {total} 回"))
                    .size(12.0)
                    .color(p.weak),
            );
        });
        if new_order != order {
            self.shared.borrow_mut().grammar_order = new_order;
        }
        ui.add_space(8.0);

        match new_order {
            GrammarOrder::Category => grammar_by_category(ui, &p, &g, show_course),
            GrammarOrder::Frequency => grammar_by_frequency(ui, &p, &g, show_course),
        }
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

        let count = if search.loading && search.items.is_empty() {
            "検索中…".to_string()
        } else {
            format!("約 {} 件", group_digits(search.total))
        };
        let mut sort = search.sort;
        let sort_options = [
            (SearchSort::Newest, "新着"),
            (SearchSort::Relevance, "関連度"),
        ];
        let mut sort_changed = false;
        if self.is_mobile() {
            // スマホでは検索語と件数を 1 行目、並び順を 2 行目に分ける（横に並べると重なる）
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("「{}」", search.query))
                        .strong()
                        .color(p.strong),
                );
                ui.label(RichText::new(&count).size(12.5).color(p.weak));
            });
            sort_changed = theme::segmented(ui, &mut sort, &sort_options);
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("「{}」", search.query))
                        .strong()
                        .color(p.strong),
                );
                ui.label(RichText::new(&count).size(12.5).color(p.weak));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    sort_changed = theme::segmented(ui, &mut sort, &sort_options);
                });
            });
        }
        if sort_changed {
            state::search(&self.shared, &ctx, search.query.clone(), sort);
        }
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

    /// 見た論文の一覧（最後に開いた順）。`limit` があればその件数だけ出す（使用量や管理の操作は出さない）。
    fn recent_papers(&mut self, ui: &mut egui::Ui, p: &Palette, limit: Option<usize>) {
        let ctx = ui.ctx().clone();
        if self.shared.borrow().papers_stale {
            state::refresh_papers(&self.shared, &ctx);
        }
        let status = self.shared.borrow().papers.clone();
        let resp = match status {
            PapersStatus::NotLoaded | PapersStatus::Loading => {
                if limit.is_none() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("見た論文を読み込み中…").color(p.weak));
                    });
                }
                return;
            }
            PapersStatus::Error(e) => {
                if limit.is_none() {
                    notice(ui, p, &format!("見た論文: {e}"));
                }
                return;
            }
            PapersStatus::Loaded(r) => r,
        };
        if resp.papers.is_empty() {
            if limit.is_none() {
                ui.label(
                    RichText::new(
                        "まだ論文を開いていません。開いた論文はここに残り、次からはここから開けます。",
                    )
                    .color(p.weak),
                );
            }
            return;
        }

        if limit.is_some() {
            theme::section_title(ui, "最近見た論文");
        } else {
            let c = &resp.cache;
            ui.label(
                RichText::new(format!(
                    "{} 本 · Mac に保存: 本文 {} 本（{}）・図 {} 枚（{}）",
                    resp.papers.len(),
                    c.papers,
                    format_bytes(c.html_bytes),
                    c.assets,
                    format_bytes(c.asset_bytes)
                ))
                .size(12.0)
                .color(p.weak),
            );
        }
        ui.add_space(4.0);

        let current = self.shared.borrow().paper_ref().map(|(id, _)| id);
        let mut action = None;
        for paper in resp.papers.iter().take(limit.unwrap_or(usize::MAX)) {
            let is_open = current.as_deref() == Some(paper.id.as_str());
            if let Some(a) = paper_row(ui, p, paper, is_open, limit.is_none()) {
                action = Some(a);
            }
            ui.add_space(6.0);
        }
        match action {
            Some(PaperAction::Open(id)) => {
                state::open_paper(&self.shared, &ctx, format!("https://arxiv.org/abs/{id}"));
            }
            Some(PaperAction::Remove { id, purge }) => {
                state::delete_recent_paper(&self.shared, &ctx, id, purge);
            }
            None => {}
        }
    }

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
                    (HistoryScope::Papers, "見た論文"),
                    (HistoryScope::ThisPaper, "この論文"),
                    (HistoryScope::AllPapers, "すべて"),
                ],
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::subtle_button(ui, "↻ 更新").clicked() {
                    let mut s = self.shared.borrow_mut();
                    s.history_stale = true;
                    s.papers_stale = true;
                }
            });
        });
        if new_scope != scope {
            let mut s = self.shared.borrow_mut();
            s.history_scope = new_scope;
            s.history_stale = true;
        }
        ui.add_space(8.0);
        if new_scope == HistoryScope::Papers {
            self.recent_papers(ui, &p, None);
            return;
        }

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
        // PC 用 ⇄ スマホ用（画面幅で決まる）が変わったら、文字の大きさや押せる範囲を切り替える
        let mobile = dom::is_mobile();
        if self.mobile != Some(mobile) {
            self.mobile = Some(mobile);
            theme::set_mobile(&ctx, mobile);
        }
        // スマホで文字を入力するときは、キーボードに隠れないようシートを全体まで上げる
        if mobile && ctx.egui_wants_keyboard_input() {
            dom::raise_sheet(dom::Sheet::Full);
        }
        let feedback_open = self.shared.borrow().feedback.open;
        if !feedback_open && !ctx.egui_wants_keyboard_input() {
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
            .frame(egui::Frame::new().fill(p.bg).inner_margin(if mobile {
                // つまみの下から始まるので上は詰める
                Margin {
                    left: 14,
                    right: 14,
                    top: 2,
                    bottom: 8,
                }
            } else {
                Margin::symmetric(16, 14)
            }))
            .show(ui, |ui| {
                let (tab, has_paper, word_selected) = {
                    let s = self.shared.borrow();
                    (
                        s.tab,
                        s.paper_ref().is_some(),
                        s.selection
                            .as_ref()
                            .is_some_and(|sel| sel.mode == Mode::Word),
                    )
                };
                // 直近に引いた単語のメモ（1 行。横にスワイプで前の語）
                self.recent_strip(ui, &p);
                // スマホでシートを下げているときに単語を選んだら、見出しの代わりに小さく和訳を出す
                if mobile && word_selected && dom::sheet() == dom::Sheet::Peek {
                    self.word_strip(ui, &p);
                    ui.add_space(8.0);
                }
                self.header(ui);
                ui.add_space(if mobile { 8.0 } else { 12.0 });
                // スマホでは論文を読んでいる間は入力欄を隠す（「検索」タブで出す）
                if !mobile || !has_paper || tab == Tab::Search {
                    self.input_bar(ui);
                }
                self.paper_card(ui);
                ui.add_space(if mobile { 10.0 } else { 12.0 });
                self.tabs(ui);
                ui.add_space(if mobile { 8.0 } else { 10.0 });
                let tab = self.shared.borrow().tab;
                egui::ScrollArea::vertical()
                    .id_salt(tab_salt(tab))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        match tab {
                            Tab::Explain => self.explain_tab(ui),
                            Tab::Search => self.search_tab(ui),
                            Tab::History => self.history_tab(ui),
                            Tab::Grammar => self.grammar_tab(ui),
                            Tab::Words => crate::words_view::words_tab(&self.shared, ui),
                        }
                        ui.add_space(16.0);
                    });
            });

        if feedback_open {
            self.feedback_modal(&ctx);
        } else {
            crate::words_view::store_warning_modal(&self.shared, &ctx);
        }
    }
}

impl PaperReaderApp {
    /// 不具合・要望を送る画面（モーダル）。
    fn feedback_modal(&mut self, ctx: &egui::Context) {
        let p = theme::palette(ctx);
        // 枠の内側の幅（左右の余白 18 + 画面端からの 16 を引く）
        let mobile = self.is_mobile();
        let edge = if mobile { 8.0 } else { 16.0 };
        let width = (ctx.content_rect().width() - edge * 2.0 - 36.0).clamp(200.0, 520.0);
        let id = egui::Id::unique("feedback_modal");
        // 中央に置くと下半分しか使えず切れるので、上寄せにする
        let top = if mobile { 8.0 } else { 24.0 };
        let max_h = (ctx.content_rect().height() - top * 2.0 - 36.0).max(160.0);
        // Area は前の描画の大きさを中身の上限にするので、段階などが変わったら測り直す
        let layout = {
            use std::hash::{Hash, Hasher};
            let s = self.shared.borrow();
            let f = &s.feedback;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::mem::discriminant(&f.stage).hash(&mut h);
            f.error.is_some().hash(&mut h);
            f.list
                .as_ref()
                .map(|l| (l.items.len(), l.configured))
                .hash(&mut h);
            (width as u32, max_h as u32).hash(&mut h);
            h.finish()
        };
        if layout != self.feedback_layout {
            self.feedback_layout = layout;
            self.feedback_resize_frames = 3;
        }
        let resize = self.feedback_resize_frames > 0;
        if resize {
            self.feedback_resize_frames -= 1;
            ctx.request_repaint();
        }
        let area = egui::Modal::default_area(id)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
            .default_size(egui::vec2(width + 36.0, max_h + 36.0))
            .sizing_pass(resize);
        // Modal の背景の暗幕は Area の範囲でしか塗られないので、画面全体を下の層で暗くする
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::unique("feedback_backdrop"),
        ))
        .rect_filled(
            ctx.content_rect(),
            0.0,
            egui::Color32::from_black_alpha(110),
        );
        let resp = egui::Modal::new(id)
            .area(area)
            .backdrop_color(egui::Color32::TRANSPARENT)
            .frame(
                egui::Frame::new()
                    .fill(p.card)
                    .stroke(egui::Stroke::new(1.0, p.card_stroke))
                    .corner_radius(egui::CornerRadius::same(14))
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| {
                ui.set_width(width);
                // 中身の高さ（前の描画で測ったもの）に合わせ、画面に収まらなければスクロールする
                let h = self.feedback_content_h.clamp(120.0, max_h);
                egui::ScrollArea::vertical()
                    .min_scrolled_height(h)
                    .max_height(h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.feedback_body(ui, &p);
                        ui.min_rect().height()
                    })
                    .inner
            });
        // 中身の高さは表示した描画でだけ測る（測り直しの描画では値がずれる）。変わったら測り直す
        let measured = resp.inner.ceil();
        if !resize && (measured - self.feedback_content_h).abs() > 1.0 {
            self.feedback_content_h = measured;
            self.feedback_resize_frames = 1;
            ctx.request_repaint();
        }
        let busy = matches!(
            self.shared.borrow().feedback.stage,
            FeedbackStage::Labeling | FeedbackStage::Sending
        );
        // 背景のクリックでは閉じない（書きかけを失わないように）。Esc・✕・「閉じる」で閉じる
        let esc = resp.is_top_modal && ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if esc && !busy {
            self.shared.borrow_mut().feedback.open = false;
        }
    }

    fn feedback_body(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("不具合・要望を送る")
                    .size(18.0)
                    .strong()
                    .color(p.strong),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = matches!(
                    self.shared.borrow().feedback.stage,
                    FeedbackStage::Labeling | FeedbackStage::Sending
                );
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(RichText::new("✕").color(p.weak)).frame(false),
                    )
                    .on_hover_text("閉じる（書いた内容は残ります）")
                    .clicked()
                {
                    self.shared.borrow_mut().feedback.open = false;
                }
            });
        });
        let list = self.shared.borrow().feedback.list.clone();
        destination_note(ui, p, list.as_ref());
        ui.add_space(6.0);

        let stage = self.shared.borrow().feedback.stage.clone();
        match stage {
            FeedbackStage::Edit => self.feedback_edit(ui, p, &ctx),
            FeedbackStage::Labeling => {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("AI がラベル（種類・機能の場所）とタイトルを付けています…")
                            .color(p.weak),
                    );
                });
                ui.add_space(12.0);
            }
            FeedbackStage::Review(preview) => {
                self.feedback_review(ui, p, &ctx, &preview, list.as_ref())
            }
            FeedbackStage::Sending => {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("送信しています…").color(p.weak));
                });
                ui.add_space(12.0);
            }
            FeedbackStage::Done(item) => self.feedback_done(ui, p, &ctx, &item, list.as_ref()),
        }

        if let Some(e) = self.shared.borrow().feedback.error.clone() {
            ui.add_space(6.0);
            notice(ui, p, &e);
        }

        if let Some(list) = &list
            && !list.items.is_empty()
        {
            ui.add_space(10.0);
            ui.separator();
            egui::CollapsingHeader::new(
                RichText::new(format!("送ったフィードバック（{} 件）", list.items.len()))
                    .size(13.0)
                    .color(p.weak),
            )
            .id_salt("feedback_list")
            .default_open(false)
            .show(ui, |ui| {
                for item in &list.items {
                    if feedback_row(ui, p, item, list.configured) {
                        state::retry_feedback(&self.shared, &ctx, item.id);
                    }
                }
            });
        }
    }

    fn feedback_edit(&mut self, ui: &mut egui::Ui, p: &Palette, ctx: &egui::Context) {
        let mut s = self.shared.borrow_mut();
        let f = &mut s.feedback;
        ui.label(
            RichText::new("タイトル（空欄なら AI が付けます）")
                .size(12.5)
                .color(p.weak),
        );
        ui.add(
            egui::TextEdit::singleline(&mut f.title)
                .hint_text("例: iPhone で英文を選択できない")
                .char_limit(MAX_FEEDBACK_TITLE_CHARS)
                .desired_width(f32::INFINITY)
                .margin(Margin::symmetric(8, 6)),
        );
        ui.add_space(4.0);
        ui.label(RichText::new("内容").size(12.5).color(p.weak));
        ui.add(
            egui::TextEdit::multiline(&mut f.body)
                .hint_text(
                    "何が起きたか、どうしてほしいかを書いてください。\n\
                     不具合なら「操作の手順」「期待した動き」「実際の動き」があると助かります。",
                )
                .char_limit(MAX_FEEDBACK_BODY_CHARS)
                .desired_rows(7)
                .desired_width(f32::INFINITY)
                .margin(Margin::symmetric(8, 6)),
        );
        ui.label(
            RichText::new(format!(
                "{} / {MAX_FEEDBACK_BODY_CHARS} 文字",
                f.body.chars().count()
            ))
            .size(11.5)
            .color(p.weak),
        );
        ui.checkbox(
            &mut f.include_context,
            "状況を添付する（開いている論文・選択中のテキスト・ブラウザの種類）",
        );
        ui.add_space(8.0);
        let can_go = !f.body.trim().is_empty();
        drop(s);
        ui.horizontal(|ui| {
            if theme::primary_button(ui, "AI でラベルを付けて確認 →", can_go).clicked()
            {
                state::preview_feedback(&self.shared, ctx);
            }
            if theme::subtle_button(ui, "閉じる").clicked() {
                self.shared.borrow_mut().feedback.open = false;
            }
        });
    }

    fn feedback_review(
        &mut self,
        ui: &mut egui::Ui,
        p: &Palette,
        ctx: &egui::Context,
        preview: &paper_reader_shared::FeedbackPreview,
        list: Option<&FeedbackListResponse>,
    ) {
        let configured = list.is_some_and(|l| l.configured);
        let all_labels: Vec<FeedbackLabel> = list.map(|l| l.labels.clone()).unwrap_or_default();
        {
            let mut s = self.shared.borrow_mut();
            let f = &mut s.feedback;
            ui.label(RichText::new("タイトル").size(12.5).color(p.weak));
            ui.add(
                egui::TextEdit::singleline(&mut f.title)
                    .char_limit(MAX_FEEDBACK_TITLE_CHARS)
                    .desired_width(f32::INFINITY)
                    .margin(Margin::symmetric(8, 6)),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("ラベル").size(12.5).color(p.weak));
                if preview.llm_labeled {
                    ui.label(
                        RichText::new("AI が選びました。押すと付け外しできます")
                            .size(11.5)
                            .color(p.weak),
                    );
                } else {
                    ui.label(
                        RichText::new("AI がラベルを付けられませんでした（手で選べます）")
                            .size(11.5)
                            .color(p.error),
                    );
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                // 種類・場所のラベルと、いま付いている自動ラベル（from-app など）
                let shown: Vec<&FeedbackLabel> = all_labels
                    .iter()
                    .filter(|l| l.kind != "meta" || f.labels.contains(&l.name))
                    .collect();
                for l in shown {
                    let on = f.labels.contains(&l.name);
                    let fixed = l.name == "from-app";
                    let (fg, bg) = if on {
                        (p.on_accent, p.accent)
                    } else {
                        (p.weak, p.card_stroke)
                    };
                    let resp = ui
                        .add_enabled(
                            !fixed,
                            egui::Button::new(RichText::new(&l.name).size(12.0).color(fg))
                                .fill(bg)
                                .corner_radius(egui::CornerRadius::same(11)),
                        )
                        .on_hover_text(&l.description)
                        .on_disabled_hover_text("アプリから送った印として必ず付きます");
                    if resp.clicked() {
                        if on {
                            f.labels.retain(|x| x != &l.name);
                        } else {
                            f.labels.push(l.name.clone());
                        }
                    }
                }
            });
        }
        ui.add_space(8.0);
        ui.label(RichText::new("送られる本文").size(12.5).color(p.weak));
        egui::Frame::new()
            .fill(p.bg)
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .id_salt("feedback_preview_body")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(&preview.issue_body)
                                .monospace()
                                .size(12.0)
                                .color(p.text),
                        );
                    });
            });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let label = if configured {
                "GitHub に Issue を作成"
            } else {
                "保存する（未送信）"
            };
            let title_ok = !self.shared.borrow().feedback.title.trim().is_empty();
            if theme::primary_button(ui, label, title_ok).clicked() {
                state::submit_feedback(&self.shared, ctx);
            }
            if theme::subtle_button(ui, "← 戻って直す").clicked() {
                self.shared.borrow_mut().feedback.stage = FeedbackStage::Edit;
            }
        });
    }

    fn feedback_done(
        &mut self,
        ui: &mut egui::Ui,
        p: &Palette,
        ctx: &egui::Context,
        item: &FeedbackItem,
        list: Option<&FeedbackListResponse>,
    ) {
        let configured = list.is_some_and(|l| l.configured);
        ui.add_space(6.0);
        theme::card(ui, |ui| {
            match (&item.issue_url, item.issue_number) {
                (Some(url), Some(n)) if item.status == "created" => {
                    ui.label(
                        RichText::new(format!("✓ Issue #{n} を作成しました"))
                            .size(16.0)
                            .strong()
                            .color(p.ok),
                    );
                    ui.hyperlink_to(RichText::new("GitHub で開く ↗").size(13.0), url);
                }
                _ => {
                    ui.label(
                        RichText::new("この Mac に保存しました（未送信）")
                            .size(16.0)
                            .strong()
                            .color(p.strong),
                    );
                    let note = if configured {
                        "GitHub に送れませんでした。下の理由を確認して「再送」を押してください（次にサーバーを起動したときにも自動で再送します）。"
                    } else {
                        "GitHub への送信が設定されていません。.env に GITHUB_REPO と GITHUB_TOKEN を設定すると、次の起動時に送信されます。"
                    };
                    ui.label(RichText::new(note).size(12.5).color(p.weak));
                    if let Some(e) = &item.error {
                        notice(ui, p, e);
                    }
                    if configured && theme::subtle_button(ui, "↻ 再送").clicked() {
                        state::retry_feedback(&self.shared, ctx, item.id);
                    }
                }
            }
            ui.horizontal_wrapped(|ui| {
                for l in &item.labels {
                    theme::chip(ui, l, p.accent, p.accent_soft);
                }
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if theme::primary_button(ui, "もう 1 件書く", true).clicked() {
                self.shared.borrow_mut().feedback.stage = FeedbackStage::Edit;
            }
            if theme::subtle_button(ui, "閉じる").clicked() {
                let mut s = self.shared.borrow_mut();
                s.feedback.open = false;
                s.feedback.stage = FeedbackStage::Edit;
            }
        });
    }
}

/// 送信先の説明。
fn destination_note(ui: &mut egui::Ui, p: &Palette, list: Option<&FeedbackListResponse>) {
    let text = match list {
        None => "送信先を確認しています…".to_string(),
        Some(l) => match (&l.repo, l.configured) {
            (Some(repo), true) => format!(
                "GitHub の {repo} に Issue として送ります。公開リポジトリなら誰でも読めるので、個人的な情報は書かないでください。"
            ),
            _ => "GitHub への送信が未設定のため、この Mac に保存します（設定すると次の起動時に送られます）。".into(),
        },
    };
    ui.label(RichText::new(text).size(12.0).color(p.weak));
}

/// 送ったフィードバックの 1 行。「再送」が押されたら true。
fn feedback_row(ui: &mut egui::Ui, p: &Palette, item: &FeedbackItem, configured: bool) -> bool {
    let mut retry = false;
    ui.horizontal_wrapped(|ui| {
        match (&item.issue_url, item.issue_number) {
            (Some(url), Some(n)) if item.status == "created" => {
                ui.hyperlink_to(RichText::new(format!("#{n}")).size(12.5), url);
            }
            _ => {
                theme::chip(ui, "未送信", p.weak, p.card_stroke)
                    .on_hover_text(item.error.as_deref().unwrap_or("GitHub が未設定です"));
            }
        }
        ui.label(RichText::new(&item.title).size(13.0).color(p.text));
        ui.label(
            RichText::new(format_time(item.created_at))
                .size(11.5)
                .color(p.weak),
        );
        if item.status != "created" && configured && ui.small_button("再送").clicked() {
            retry = true;
        }
    });
    retry
}

fn tab_salt(tab: Tab) -> &'static str {
    match tab {
        Tab::Explain => "explain_scroll",
        Tab::Search => "search_scroll",
        Tab::History => "history_scroll",
        Tab::Grammar => "grammar_scroll",
        Tab::Words => "words_scroll",
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
pub(crate) fn notice(ui: &mut egui::Ui, p: &Palette, text: &str) {
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
fn empty_explain(ui: &mut egui::Ui, p: &Palette, has_paper: bool, mobile: bool) {
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
        let steps: &[(&str, &str)] = if mobile {
            &[
                ("長押し", "単語 → 辞書の訳と AI の解説"),
                ("つまみで範囲を広げる", "英文 → 和訳・文構造・文法"),
                ("解説する", "このパネルのボタンで解説"),
                ("つまみ", "上下に動かすかタップで、パネルの大きさを変える"),
            ]
        } else {
            &[
                ("ダブルクリック", "単語 → 辞書の訳と AI の解説"),
                ("ドラッグ", "英文 → 和訳・文構造・文法"),
                ("E", "選択中のテキストを解説"),
                ("Esc", "選択を解除"),
            ]
        };
        for (key, text) in steps {
            ui.horizontal_wrapped(|ui| {
                theme::chip(ui, key, p.accent, p.accent_soft);
                ui.label(RichText::new(*text).size(13.5).color(p.text));
            });
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(if mobile {
                "パネルは下の 3 段階（少し・半分・全体）で止まります。"
            } else {
                "iPhone などでは長押しで選択できます。"
            })
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
pub(crate) fn group_digits(n: u64) -> String {
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

/// 辞書の結果（見出し語と訳のリスト）。辞書に無い語は AI 訳を出す。
/// AI の「再試行」が押されたら true。
fn dict_view(
    ui: &mut egui::Ui,
    p: &Palette,
    status: &DictStatus,
    ai: &DictAiPhase,
    show_kana: bool,
) -> bool {
    let mut retry = false;
    match status {
        DictStatus::Idle => {}
        DictStatus::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("辞書を検索中…").color(p.weak));
            });
        }
        DictStatus::Error(e) => notice(ui, p, &format!("辞書: {e}")),
        DictStatus::Loaded(r) => {
            let note = r.note.as_ref();
            let kana = note
                .filter(|_| show_kana)
                .and_then(|n| n.kana.as_deref().map(|k| (k, n.kana_source)));
            let headword = r.headword.as_deref().unwrap_or(&r.query);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(headword).size(21.0).strong().color(p.strong));
                speak_button(ui, p, headword);
                if let Some((kana, source)) = kana {
                    let resp = ui.label(RichText::new(kana).size(15.0).color(p.text));
                    if source == Some(WordSource::Llm) {
                        resp.on_hover_text(
                            "読みはローカル LLM の推定です（正確な発音とは異なることがあります）",
                        );
                        ui.label(RichText::new("読みは AI 推定").size(11.5).color(p.weak));
                    }
                }
                if r.headword.as_deref().is_some_and(|h| h != r.query) {
                    ui.label(
                        RichText::new(format!("{} の原形", r.query))
                            .size(12.5)
                            .color(p.weak),
                    );
                }
            });
            let note_meanings = note.map(|n| n.meanings.as_slice()).unwrap_or_default();
            let user_meanings = note.is_some_and(|n| n.meanings_source == Some(WordSource::User));
            if r.headword.is_some() {
                meanings_list(ui, p, headword, &r.meanings);
                if user_meanings && !note_meanings.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("自分で登録した訳").size(12.5).color(p.weak));
                    meanings_list(ui, p, headword, note_meanings);
                }
                for entry in &r.related {
                    related_view(ui, p, entry);
                }
            } else if !note_meanings.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    if user_meanings {
                        theme::chip(ui, "自分で登録した訳", p.accent, p.accent_soft);
                    } else {
                        theme::chip(ui, "AI 訳", p.sentence, p.sentence_soft);
                        ui.label(
                            RichText::new("辞書に無い語のため、ローカル LLM が訳しました（誤りを含むことがあります）")
                                .size(12.0)
                                .color(p.weak),
                        );
                    }
                });
                meanings_list(ui, p, headword, note_meanings);
            } else if *ai != DictAiPhase::Loading {
                let checked = note.is_some_and(|n| n.ai_checked);
                ui.label(
                    RichText::new(if checked {
                        format!(
                            "「{}」は辞書に見つからず、AI でも訳せませんでした。",
                            r.query
                        )
                    } else {
                        format!("「{}」は辞書に見つかりませんでした。", r.query)
                    })
                    .color(p.weak),
                );
            }
            match ai {
                DictAiPhase::Idle => {}
                DictAiPhase::Loading => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(if r.needs_ai_meanings() {
                                "辞書に無いため、AI で訳しています…"
                            } else {
                                "読みを推定中…"
                            })
                            .size(13.0)
                            .color(p.weak),
                        );
                    });
                }
                DictAiPhase::Error(e) => {
                    ui.add_space(4.0);
                    notice(ui, p, &format!("AI: {e}"));
                    retry = theme::subtle_button(ui, "再試行").clicked();
                }
            }
        }
    }
    retry
}

/// 発音を再生するボタン（🔊）。ブラウザの音声合成を使うので、対応していなければ出さない。
fn speak_button(ui: &mut egui::Ui, p: &Palette, word: &str) {
    if !dom::can_speak() {
        return;
    }
    // スマホでは指で押しやすいよう大きめに
    let size = if dom::is_mobile() { 19.0 } else { 16.0 };
    if ui
        .add(egui::Button::new(RichText::new("🔊").size(size).color(p.accent)).frame(false))
        .on_hover_text("発音を聞く（端末の音声合成）")
        .clicked()
    {
        dom::speak(word);
    }
}

/// 表示に使う見出しと訳（辞書にあれば辞書の訳、無ければ単語帳の訳）。
fn entry_meanings(r: &DictResponse) -> (&str, &[String]) {
    match &r.headword {
        Some(head) => (head, &r.meanings),
        None => (
            &r.query,
            r.note
                .as_ref()
                .map(|n| n.meanings.as_slice())
                .unwrap_or_default(),
        ),
    }
}

/// 品詞の表示に添える説明。
const POS_HINT: &str = "品詞は訳語の形と英語の語尾から推定しています（誤ることがあります）";

/// 小さな表示用の訳（最初の 3 つを「；」でつなぐ。『』や《》の注記は外す）。
fn compact_meanings(r: &DictResponse) -> Option<String> {
    let (_, meanings) = entry_meanings(r);
    let parts: Vec<String> = meanings
        .iter()
        .take(3)
        .map(|m| strip_dict_notes(m))
        .filter(|m| !m.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("；"))
}

/// EJDict の注記（《...》と『』）を外す。
pub(crate) fn strip_dict_notes(m: &str) -> String {
    let mut out = String::new();
    let mut depth: u32 = 0;
    for c in m.chars() {
        match c {
            '《' => depth += 1,
            '》' => depth = depth.saturating_sub(1),
            '『' | '』' => {}
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
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
    meanings_list(ui, p, &entry.headword, &entry.meanings);
}

/// 訳を品詞ごとにまとめ、番号付きで並べる。元データの『』で囲まれた重要な訳は強調する。
fn meanings_list(ui: &mut egui::Ui, p: &Palette, headword: &str, meanings: &[String]) {
    let font = theme::body_font(14.5);
    for (pos, range) in pos_groups(headword, meanings) {
        if let Some(pos) = pos {
            ui.add_space(3.0);
            theme::chip(ui, pos.label(), p.word, p.word_soft).on_hover_text(POS_HINT);
        }
        for i in range {
            meaning_row(ui, p, &font, i, &meanings[i]);
        }
    }
}

fn meaning_row(ui: &mut egui::Ui, p: &Palette, font: &egui::FontId, i: usize, meaning: &str) {
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

/// 文字数で切り詰める（はみ出したら「…」）。
fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
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

/// チップの文言。
/// - 範囲内でレッスンあり: 「📺 関係詞 › 第3講 …」
/// - 範囲内でレッスン未記入: 「📺 関係詞（パーフェクト英文法）」
/// - 範囲外: 「論文構文：同格の that（講座範囲外）」
/// - 講座の案内がオフ: 文法項目の名前だけ
fn chip_label(tag: &ExplainTag, show_course: bool, course_short: &str) -> String {
    if !show_course {
        return tag.name.clone();
    }
    match (tag.in_scope, tag.lessons.as_slice()) {
        (true, [first, rest @ ..]) => {
            let more = if rest.is_empty() {
                String::new()
            } else {
                format!(" ほか {} 件", rest.len())
            };
            format!("📺 {} › {}{more}", tag.category, first.title)
        }
        (true, []) => format!("📺 {}（{course_short}）", tag.category),
        (false, _) => format!("{}：{}（講座範囲外）", tag.category, tag.name),
    }
}

/// 選んだ文法項目の説明と、対応するレッスン。
fn tag_detail(
    ui: &mut egui::Ui,
    p: &Palette,
    tag: &ExplainTag,
    found: bool,
    show_course: bool,
    course_short: &str,
) {
    egui::Frame::new()
        .fill(p.accent_soft.gamma_multiply(0.6))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&tag.name).size(15.0).strong().color(p.strong));
                theme::chip(ui, &tag.category, p.accent, p.card);
            });
            if !tag.span.is_empty() {
                ui.label(
                    RichText::new(format!("該当箇所: “{}”", tag.span))
                        .size(13.0)
                        .italics()
                        .color(p.text),
                );
                if !found {
                    ui.label(
                        RichText::new("（本文の中では見つかりませんでした）")
                            .size(12.0)
                            .color(p.weak),
                    );
                }
            }
            ui.add_space(2.0);
            ui.label(RichText::new(&tag.hint).size(13.5).color(p.text));
            if show_course {
                ui.add_space(4.0);
                lessons_block(
                    ui,
                    p,
                    tag.in_scope,
                    &tag.lessons,
                    &tag.category,
                    course_short,
                );
            }
        });
}

/// 対応するレッスン（無ければカテゴリ単位の案内か、範囲外の旨）。
fn lessons_block(
    ui: &mut egui::Ui,
    p: &Palette,
    in_scope: bool,
    lessons: &[paper_reader_shared::LessonRef],
    category: &str,
    course_short: &str,
) {
    if !in_scope {
        let text = if category == paper_reader_shared::OUT_OF_COURSE_CATEGORY {
            format!("{course_short}の範囲外の項目です（論文でよく出る構文）。")
        } else {
            format!("記入済みの「{category}」のレッスンには含まれていません（講座範囲外）。")
        };
        ui.label(RichText::new(text).size(12.5).color(p.weak));
    } else if lessons.is_empty() {
        ui.label(
            RichText::new(format!("📺 {course_short}「{category}」で復習できます"))
                .size(12.5)
                .color(p.accent),
        );
    } else {
        for l in lessons {
            ui.label(
                RichText::new(format!("📺 {} › {}", l.category, l.title))
                    .size(12.5)
                    .color(p.accent),
            );
        }
    }
}

/// カテゴリ → タグ → 対応レッスン。
fn grammar_by_category(ui: &mut egui::Ui, p: &Palette, g: &GrammarMapResponse, show_course: bool) {
    for cat in &g.categories {
        let tags: Vec<&GrammarTagInfo> = g.tags.iter().filter(|t| t.category == cat.name).collect();
        if tags.is_empty() {
            continue;
        }
        let total: u64 = tags.iter().map(|t| t.count).sum();
        let mut title = cat.name.clone();
        if !cat.in_course {
            title.push_str("（講座範囲外）");
        }
        let mut meta = format!("出現 {total}");
        if show_course && cat.in_course {
            meta.push_str(&format!(" · レッスン {}", cat.lesson_count));
        }
        theme::card(ui, |ui| {
            egui::CollapsingHeader::new(
                RichText::new(format!("{title}   {meta}"))
                    .strong()
                    .color(if cat.in_course { p.strong } else { p.weak }),
            )
            // 出現回数が 0 から増えたら開き直すよう、有無を ID に含める
            .id_salt(("grammar-cat", &cat.name, total > 0))
            .default_open(total > 0)
            .show(ui, |ui| {
                for tag in tags {
                    grammar_tag_row(ui, p, tag, show_course, &g.course_short, false);
                }
            });
        });
        ui.add_space(6.0);
    }
}

/// 出現回数の多い順（出てきたものだけ）。
fn grammar_by_frequency(ui: &mut egui::Ui, p: &Palette, g: &GrammarMapResponse, show_course: bool) {
    let mut tags: Vec<&GrammarTagInfo> = g.tags.iter().filter(|t| t.count > 0).collect();
    if tags.is_empty() {
        ui.label(
            RichText::new("まだ英文を解説していません。英文を解説すると、出てきた文法項目がここに数えられます。")
                .color(p.weak),
        );
        return;
    }
    tags.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    theme::card(ui, |ui| {
        ui.label(
            RichText::new("よく出る項目ほど、復習の優先度が高い項目です。")
                .size(12.5)
                .color(p.weak),
        );
        for tag in tags {
            ui.separator();
            grammar_tag_row(ui, p, tag, show_course, &g.course_short, true);
        }
    });
}

fn grammar_tag_row(
    ui: &mut egui::Ui,
    p: &Palette,
    tag: &GrammarTagInfo,
    show_course: bool,
    course_short: &str,
    with_category: bool,
) {
    ui.horizontal_wrapped(|ui| {
        let (fg, bg) = if tag.count > 0 {
            (p.on_accent, p.accent)
        } else {
            (p.weak, p.card_stroke)
        };
        theme::chip(ui, &format!("×{}", tag.count), fg, bg);
        ui.label(RichText::new(&tag.name).size(14.5).color(p.strong))
            .on_hover_text(&tag.hint);
        if with_category {
            theme::chip(ui, &tag.category, p.accent, p.accent_soft);
        }
    });
    if show_course {
        ui.indent(("lessons", &tag.id), |ui| {
            lessons_block(
                ui,
                p,
                tag.in_scope,
                &tag.lessons,
                &tag.category,
                course_short,
            );
        });
    }
}

enum PaperAction {
    Open(String),
    Remove { id: String, purge: bool },
}

/// 見た論文 1 件。タイトルか「開く」で開く。`manage` なら一覧から外す操作も出す。
fn paper_row(
    ui: &mut egui::Ui,
    p: &Palette,
    paper: &paper_reader_shared::RecentPaper,
    is_open: bool,
    manage: bool,
) -> Option<PaperAction> {
    let mut action = None;
    theme::card(ui, |ui| {
        let title = ui
            .add(
                egui::Label::new(
                    RichText::new(&paper.title)
                        .size(15.0)
                        .strong()
                        .color(p.strong),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if title.clicked() && !is_open {
            action = Some(PaperAction::Open(paper.id.clone()));
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("arXiv:{}", paper.id))
                    .size(12.0)
                    .color(p.weak),
            );
            ui.label(
                RichText::new(format!(
                    "{} · {} 回",
                    format_time(paper.last_opened_at),
                    paper.open_count
                ))
                .size(12.0)
                .color(p.weak),
            );
            if paper.word_count > 0 {
                theme::chip(
                    ui,
                    &format!("単語 {}", paper.word_count),
                    p.word,
                    p.word_soft,
                );
            }
            if paper.sentence_count > 0 {
                theme::chip(
                    ui,
                    &format!("英文 {}", paper.sentence_count),
                    p.sentence,
                    p.sentence_soft,
                );
            }
        });
        ui.horizontal(|ui| {
            if is_open {
                theme::chip(ui, "表示中", p.ok, p.ok.gamma_multiply(0.15));
            } else if theme::primary_button(ui, "開く", true).clicked() {
                action = Some(PaperAction::Open(paper.id.clone()));
            }
            if paper.bookmarked {
                ui.label(
                    RichText::new("📌 付箋あり")
                        .size(12.0)
                        .color(theme::BOOKMARK),
                )
                .on_hover_text("開くと付箋の位置から表示します");
            }
            if paper.cached {
                ui.label(RichText::new("✓ Mac に保存済み").size(12.0).color(p.ok))
                    .on_hover_text("arXiv にアクセスせずに開けます（図は一度表示したものを保存）");
            }
            if manage {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button(RichText::new("…").color(p.weak), |ui| {
                        if ui.button("一覧から外す").clicked() {
                            action = Some(PaperAction::Remove {
                                id: paper.id.clone(),
                                purge: false,
                            });
                            ui.close();
                        }
                        if ui
                            .button("一覧から外して、保存した本文と図も削除")
                            .clicked()
                        {
                            action = Some(PaperAction::Remove {
                                id: paper.id.clone(),
                                purge: true,
                            });
                            ui.close();
                        }
                        ui.label(
                            RichText::new("調べた単語・英文の履歴は残ります")
                                .size(11.5)
                                .color(p.weak),
                        );
                    });
                });
            }
        });
    });
    action
}

/// 1234567 → "1.2 MB"
fn format_bytes(n: i64) -> String {
    let n = n.max(0) as f64;
    if n >= 1024.0 * 1024.0 {
        format!("{:.1} MB", n / 1024.0 / 1024.0)
    } else if n >= 1024.0 {
        format!("{:.0} KB", n / 1024.0)
    } else {
        format!("{n:.0} B")
    }
}
