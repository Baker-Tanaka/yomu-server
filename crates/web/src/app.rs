//! egui の解説 UI。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use eframe::egui::{self, Margin, RichText};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use paper_reader_shared::{
    APP_VERSION, DictEntry, DictResponse, ExplainTag, FeedbackItem, FeedbackLabel,
    FeedbackListResponse, GIT_HASH, GrammarTagInfo, GrammarTagsResponse, HistoryEntry,
    MAX_FEEDBACK_BODY_CHARS, MAX_FEEDBACK_TITLE_CHARS, Mode, ReadLaterItem, ReadLaterPut,
    SLASH_READING_HEADING, SearchItem, SearchSort, TagGuide, WordSource, build_version_label,
    paper_url_from_input, plain_arrows, pos_groups, pos_summary, version_label, without_section,
};

use crate::dom;
use crate::state::{
    self, DictAiPhase, DictStatus, ExplainPhase, FeedbackStage, GrammarOrder, GrammarStatus,
    HealthStatus, HistoryScope, HistoryStatus, PaperStatus, PapersStatus, SearchView, Shared,
    SharedRef, Tab,
};
use crate::theme::{self, Palette};

/// タブの余白を詰める幅（これより狭いと、いつもの余白では 5 つのタブが入らない）
const NARROW_TABS_WIDTH: f32 = 340.0;
/// ヘッダーの副題（for arXiv）を出すのに要る、題名の右の残り幅
const SUBTITLE_MIN_WIDTH: f32 = 56.0;
/// スマホ用の下端のタブの高さと、文字を小さくするタブの幅
const BOTTOM_TAB_HEIGHT: f32 = 38.0;
const NARROW_BOTTOM_TAB_WIDTH: f32 = 66.0;
/// スマホで選択中の英文を出す行数
const SELECTION_PREVIEW_ROWS: usize = 3;
/// 検索欄の案内文
const SEARCH_HINT: &str = "arXiv の URL・ID、またはキーワードで検索";
/// 論文を開く前の画面に出す「後で読む」の件数
const READ_LATER_PREVIEW: usize = 3;

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
    /// 次の描画で入力欄にフォーカスを移す（「別の論文を開く」を押したとき）
    focus_input: bool,
    /// この描画で、検索欄（HTML の入力欄）を重ねたか。重ねなければ隠す
    search_input_placed: bool,
    /// コマンドパレットを開く（⚙ のメニューから）
    open_palette: Option<crate::command_palette::Opener>,
}

impl PaperReaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        theme::install(&cc.egui_ctx);

        let shared: SharedRef = Rc::new(RefCell::new(Shared::new()));

        let ctx = cc.egui_ctx.clone();
        let shared_for_dom = shared.clone();
        if let Err(e) = dom::watch_selection(move |info| {
            let text = info.text.clone();
            state::on_select(&shared_for_dom, &ctx, info);
            // シートの高さが変わったあとの本文の範囲に合わせて出す
            crate::selection_menu::show(&text);
        }) {
            log::error!("選択の監視を開始できません: {e}");
        }
        let ctx = cc.egui_ctx.clone();
        let shared_for_menu = shared.clone();
        if let Err(e) = crate::selection_menu::init(move |action| {
            use crate::selection_menu::MenuAction;
            match action {
                MenuAction::Explain => {
                    state::on_shortcut(&shared_for_menu, &ctx, dom::Shortcut::Explain);
                }
                MenuAction::Bookmark => state::place_bookmark(&shared_for_menu, &ctx),
            }
            ctx.request_repaint();
        }) {
            log::error!("選択のメニューを使えません: {e}");
        }
        let ctx = cc.egui_ctx.clone();
        let shared_for_keys = shared.clone();
        if let Err(e) = dom::watch_shortcuts(move |key| {
            state::on_shortcut(&shared_for_keys, &ctx, key);
        }) {
            log::error!("ショートカットを有効にできません: {e}");
        }
        let ctx = cc.egui_ctx.clone();
        let shared_for_cites = shared.clone();
        if let Err(e) = crate::cite_peek::watch(move |action| {
            state::on_cite_action(&shared_for_cites, &ctx, action);
        }) {
            log::error!("引用の覗き見を有効にできません: {e}");
        }
        let ctx = cc.egui_ctx.clone();
        let shared_for_recent = shared.clone();
        if let Err(e) = crate::recent_bar::watch(move |word| {
            state::look_up_word(&shared_for_recent, &ctx, &word);
            ctx.request_repaint();
        }) {
            log::error!("直近の単語のメモを使えません: {e}");
        }
        if let Err(e) = crate::toc::init() {
            log::error!("目次を使えません: {e}");
        }
        if let Err(e) = dom::init_splitter() {
            log::error!("ペイン境界のドラッグを有効にできません: {e}");
        }
        state::refresh_health(&shared, &cc.egui_ctx);
        // 実験モード（端末の LLM）を前回オンにしていれば読み込む（既定はオフで、何も読まない）
        crate::local_llm::resume(&shared, &cc.egui_ctx);

        if let Err(e) = crate::search_input::init(&cc.egui_ctx, SEARCH_HINT) {
            log::error!("検索欄を作れません: {e}");
        }

        let ctx = cc.egui_ctx.clone();
        let shared_for_palette = shared.clone();
        let shared_for_items = shared.clone();
        let shared_for_guard = shared.clone();
        let palette_shared = shared.clone();
        let palette_ctx = cc.egui_ctx.clone();
        let open_palette = match crate::command_palette::init(
            // 不具合・要望の画面の上には出さない
            move || !shared_for_guard.borrow().feedback.open,
            move |query| {
                // 開いたときに、まだ読んでいない一覧があれば読み込む（次に打った文字から反映する）
                if palette_shared.borrow().papers_stale {
                    state::refresh_papers(&palette_shared, &palette_ctx);
                }
                crate::commands::items(&shared_for_items, query)
            },
            move |command| crate::commands::run(&shared_for_palette, &ctx, command),
        ) {
            Ok(open) => Some(open),
            Err(e) => {
                log::error!("コマンドパレットを使えません: {e}");
                None
            }
        };

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
            focus_input: false,
            search_input_placed: false,
            open_palette,
        }
    }

    // ───────────── ヘッダー ─────────────

    fn is_mobile(&self) -> bool {
        self.mobile.unwrap_or(false)
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let mobile = self.is_mobile();
        // 右の操作を先に置き、題名は残りの幅に収める（狭い画面で重ならないように）
        let title = |ui: &mut egui::Ui| {
            logo(ui, &p);
            ui.label(
                RichText::new("論文リーダー")
                    .size(if mobile { 16.0 } else { 17.0 })
                    .strong()
                    .color(p.strong),
            );
            // 中途半端に省略するくらいなら出さない
            if !mobile && ui.available_width() >= SUBTITLE_MIN_WIDTH {
                ui.label(RichText::new("for arXiv").size(11.5).color(p.weak));
            }
        };
        egui::Sides::new()
            .shrink_left()
            .truncate()
            .spacing(4.0)
            .show(ui, title, |ui| {
                if mobile {
                    ui.spacing_mut().item_spacing.x = 4.0;
                }
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
    }

    /// 設定（⚙）。
    fn settings_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button(RichText::new("⚙").size(15.0), |ui| {
            ui.set_min_width(240.0);
            ui.label(RichText::new("設定").strong());
            ui.separator();
            let mut s = self.shared.borrow_mut();
            let mut auto = s.auto_explain;
            if ui.checkbox(&mut auto, "選択したら自動で解説").changed() {
                s.auto_explain = auto;
            }
            let mut slash = s.show_slash;
            if ui
                .checkbox(&mut slash, "スラッシュリーディングを表示する")
                .on_hover_text("英文の解説に、意味のまとまりごとの区切り（/）と語順どおりの直訳を付けます")
                .changed()
            {
                s.set_show_slash(slash);
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
                .on_hover_text("本文の上の 1 行に、最近引いた 10 語と訳を出します（このブラウザにだけ保存）")
                .changed()
            {
                s.set_show_recent(recent);
            }
            if !s.recent.is_empty() && ui.button("直近の単語のメモを消す").clicked() {
                s.recent.clear();
            }
            let local_on = s.local.enabled;
            drop(s);
            ui.separator();
            let mut on = local_on;
            if ui
                .checkbox(&mut on, "🧪 実験: 端末で LLM を動かす")
                .on_hover_text(
                    "この端末の GPU（WebGPU）で小さな LLM を動かし、Mac の LLM と比べます。オンにするとモデル（数百 MB〜1.4 GB）を Mac からダウンロードします",
                )
                .changed()
            {
                crate::local_llm::set_enabled(&self.shared, ui.ctx(), on);
            }
            if on {
                use crate::local_llm::LocalPhase;
                let status = match &self.shared.borrow().local.phase {
                    LocalPhase::Off => String::new(),
                    LocalPhase::Unsupported => "この端末は WebGPU に未対応です".into(),
                    LocalPhase::Preparing => "準備しています…".into(),
                    LocalPhase::Loading { progress, .. } => {
                        format!("モデルを読み込み中… {:.0}%", progress * 100.0)
                    }
                    LocalPhase::Ready { .. } => {
                        "準備完了（解説タブで単語・英文を選んで「端末で訳す」）".into()
                    }
                    LocalPhase::Error(e) => format!("エラー: {e}"),
                };
                if !status.is_empty() {
                    ui.label(RichText::new(status).small().weak());
                }
            }
            ui.separator();
            if !self.is_mobile()
                && let Some(open) = &self.open_palette
                && ui
                    .button("⌨ コマンドパレット（⌘K / Ctrl+K）")
                    .on_hover_text("操作・節・論文を文字で絞り込んで実行します")
                    .clicked()
            {
                ui.close();
                open();
            }
            if ui.button("💬 不具合・要望を送る…").clicked() {
                state::open_feedback(&self.shared, ui.ctx());
                ui.close();
            }
            ui.separator();
            self.version_info(ui);
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

    /// 画面（wasm）とサーバーのバージョン・Git ハッシュ。食い違っていたら再読み込みを促す。
    fn version_info(&self, ui: &mut egui::Ui) {
        let web = build_version_label();
        ui.label(RichText::new(format!("画面: {web}")).small().weak())
            .on_hover_text("この画面（ブラウザで動いている部分）をビルドしたバージョンとコミット");
        let s = self.shared.borrow();
        let HealthStatus::Ok(h) = &s.health else {
            return;
        };
        if h.version.is_empty() {
            return;
        }
        let server = version_label(&h.version, &h.git_hash, &h.git_date);
        ui.label(RichText::new(format!("サーバー: {server}")).small().weak())
            .on_hover_text("サーバーをビルドしたバージョンとコミット");
        let differs = h.version != APP_VERSION
            || (!h.git_hash.is_empty() && !GIT_HASH.is_empty() && h.git_hash != GIT_HASH);
        if differs {
            let warn = ui.visuals().warn_fg_color;
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "画面とサーバーのバージョンが違います。ページを再読み込みしてください",
                    )
                    .small()
                    .color(warn),
                )
                .wrap(),
            );
        }
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
        let r = match &dict {
            DictStatus::Loaded(r) => Some(r),
            _ => None,
        };
        let head = r
            .map(|r| r.headword.as_deref().unwrap_or(&r.query))
            .unwrap_or(&word);
        // 右のボタンを先に置き、見出し・読み・品詞は残りの幅で省略する（重ならないように）
        egui::Sides::new().shrink_left().truncate().show(
            ui,
            |ui| {
                ui.label(RichText::new(head).size(18.0).strong().color(p.strong));
                speak_button(ui, p, head);
                // 品詞は短いので先に出し、長くなる読みを最後にする
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
                if let Some(kana) = r
                    .filter(|_| show_kana)
                    .and_then(|r| r.note.as_ref())
                    .and_then(|n| n.kana.as_deref())
                {
                    ui.label(RichText::new(kana).size(13.5).color(p.weak));
                }
            },
            |ui| {
                expand = theme::subtle_button(ui, "詳しく").clicked();
                if r.is_some_and(|r| {
                    r.headword.is_none()
                        && r.note.as_ref().is_some_and(|n| {
                            !n.meanings.is_empty() && n.meanings_source == Some(WordSource::Llm)
                        })
                }) {
                    theme::chip(ui, "AI 訳", p.sentence, p.sentence_soft);
                }
            },
        );
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

    /// スマホでシートを下げたまま英文を選んだときの小さな表示。
    /// 範囲を調整している本文を隠さず、解説を始める・開くボタンだけを出す。
    fn sentence_strip(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let (text, phase) = {
            let s = self.shared.borrow();
            let Some(sel) = &s.selection else {
                return;
            };
            (sel.text.clone(), s.explain.phase.clone())
        };
        let status = match &phase {
            ExplainPhase::Idle => "英文を選択中",
            ExplainPhase::Done => "解説ができました",
            ExplainPhase::Error(_) => "解説できませんでした",
            _ => "解説を生成中…",
        };
        let (mut explain, mut open) = (false, false);
        egui::Sides::new().shrink_left().truncate().show(
            ui,
            |ui| {
                theme::chip(ui, "英文", p.sentence, p.sentence_soft);
                if phase.is_running() {
                    ui.spinner();
                }
                ui.label(RichText::new(status).size(13.5).color(p.weak));
            },
            |ui| {
                if phase == ExplainPhase::Idle {
                    explain = theme::primary_button(ui, "解説する", true).clicked();
                    open = theme::subtle_button(ui, "開く").clicked();
                } else {
                    open = theme::primary_button(ui, "見る", true).clicked();
                }
            },
        );
        // 選べている範囲が分かるように、終わりの方を出す
        let resp = ui.add(
            egui::Label::new(
                RichText::new(selection_ends(&text))
                    .size(14.5)
                    .color(p.text),
            )
            .truncate()
            .sense(egui::Sense::click()),
        );
        if explain {
            self.shared.borrow_mut().tab = Tab::Explain;
            state::start_explain(&self.shared, ui.ctx(), false);
        } else if open || resp.clicked() {
            self.shared.borrow_mut().tab = Tab::Explain;
            dom::set_sheet(if phase == ExplainPhase::Idle {
                dom::Sheet::Half
            } else {
                dom::Sheet::Full
            });
        }
    }

    /// 直近に引いた単語のメモを、本文の上の 1 行に反映する。
    fn sync_recent_bar(&self) {
        let s = self.shared.borrow();
        let active = s
            .selection
            .as_ref()
            .filter(|sel| sel.mode == Mode::Word)
            .map(|sel| sel.text.as_str());
        let limit = if s.show_recent { usize::MAX } else { 0 };
        crate::recent_bar::render(s.recent.items().take(limit), active);
    }

    // ───────────── 入力欄と論文 ─────────────

    /// 検索欄とボタン。欄は canvas の上に重ねた HTML の入力欄で、egui 側は同じ場所に下地を描く
    /// （メニューやモーダルが重なるあいだは入力欄を隠し、下地に入力中の文字を見せる）。
    fn input_bar(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let loading = matches!(self.shared.borrow().paper, PaperStatus::Loading);
        self.input = crate::search_input::value();
        let is_paper = paper_url_from_input(&self.input).is_some();
        let label = if is_paper { "開く" } else { "検索" };
        let mut submit = crate::search_input::take_submitted() && !self.input.trim().is_empty();
        ui.horizontal(|ui| {
            let button_width = 64.0;
            let size = egui::vec2(ui.available_width() - button_width - 8.0, 34.0);
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            let covered = {
                let s = self.shared.borrow();
                s.feedback.open || s.store_warning.is_some() || egui::Popup::is_any_open(ui.ctx())
            };
            if covered {
                ui.painter().rect_filled(rect, 8.0, p.card);
                let (text, color) = if self.input.is_empty() {
                    (SEARCH_HINT, p.weak)
                } else {
                    (self.input.as_str(), p.text)
                };
                ui.painter().with_clip_rect(rect.shrink(10.0)).text(
                    rect.left_center() + egui::vec2(10.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    text,
                    theme::body_font(14.0),
                    color,
                );
            } else if ui.is_rect_visible(rect) {
                crate::search_input::place(rect, ui.ctx().zoom_factor());
                self.search_input_placed = true;
                if std::mem::take(&mut self.focus_input) {
                    crate::search_input::focus();
                }
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
                            if theme::subtle_button(ui, "別の論文を開く")
                                .on_hover_text(
                                    "「検索」タブに切り替えて、URL・ID・キーワードの欄を出します",
                                )
                                .clicked()
                            {
                                self.shared.borrow_mut().tab = Tab::Search;
                                self.focus_input = true;
                            }
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
        let changed = ui
            .scope(|ui| {
                // 狭い画面（iPhone の拡大表示など）では、タブが右にはみ出さないよう余白を詰める
                if ui.available_width() < NARROW_TABS_WIDTH {
                    ui.spacing_mut().button_padding.x = 4.0;
                }
                theme::segmented(
                    ui,
                    &mut tab,
                    &[
                        (Tab::Explain, "解説"),
                        (Tab::Search, search_label.as_str()),
                        (Tab::History, "履歴"),
                        (Tab::Grammar, "文法項目"),
                        (Tab::Words, "単語帳"),
                    ],
                )
            })
            .inner;
        if changed {
            state::show_tab(&self.shared, tab);
        }
    }

    /// スマホ用のタブ。親指が届くようシートの下端に固定し、幅を 5 等分する。
    fn bottom_tabs(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let current = self.shared.borrow().tab;
        let search_count = self.shared.borrow().search.items.len();
        let search_label = if search_count > 0 {
            format!("検索 {search_count}")
        } else {
            "検索".to_string()
        };
        let tabs = [
            (Tab::Explain, "解説"),
            (Tab::Search, search_label.as_str()),
            (Tab::History, "履歴"),
            (Tab::Grammar, "文法項目"),
            (Tab::Words, "単語帳"),
        ];
        let width = ui.available_width() / tabs.len() as f32;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (tab, label) in tabs {
                let selected = tab == current;
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(width, BOTTOM_TAB_HEIGHT),
                    egui::Sense::click(),
                );
                let painter = ui.painter();
                if selected {
                    painter.rect_filled(rect.shrink2(egui::vec2(3.0, 2.0)), 10.0, p.accent_soft);
                }
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    theme::body_font(if width < NARROW_BOTTOM_TAB_WIDTH {
                        12.0
                    } else {
                        13.5
                    }),
                    if selected { p.accent } else { p.weak },
                );
                if resp.clicked() {
                    state::show_tab(&self.shared, tab);
                    // 小さくしているときにタブを押したら、中身が見える高さにする
                    dom::raise_sheet(dom::Sheet::Half);
                }
            }
        });
    }

    // ───────────── 解説タブ ─────────────

    fn explain_button_at_right(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::primary_button(ui, "解説する", true).clicked() {
                state::start_explain(&self.shared, ui.ctx(), false);
            }
        });
    }

    fn explain_tab(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let (sel, has_paper) = {
            let s = self.shared.borrow();
            (s.selection.clone(), s.paper_ref().is_some())
        };
        let Some(sel) = sel else {
            empty_explain(ui, &p, has_paper, self.is_mobile());
            if !has_paper {
                // 論文を開く前は、後で読む論文と最近見た論文からすぐ開けるようにする
                self.refresh_read_later_if_stale(ui.ctx());
                if !self.shared.borrow().read_later.items.is_empty() {
                    ui.add_space(10.0);
                    self.read_later_list(ui, &p, Some(READ_LATER_PREVIEW));
                }
                ui.add_space(10.0);
                self.recent_papers(ui, &p, Some(5));
            }
            return;
        };

        // スマホではすぐ押せるように、解説する前は上のカードにも「解説する」を出す
        let mobile = self.is_mobile();
        let can_explain = mobile && {
            let s = self.shared.borrow();
            s.explain.phase == ExplainPhase::Idle
        };
        // スマホで単語を選んだときは、見出し語が辞書にあるので「選択中」のカードを省き、訳を先頭に出す
        let dict_first = mobile && sel.mode == Mode::Word;
        if !dict_first {
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    match sel.mode {
                        Mode::Word => theme::chip(ui, "単語", p.word, p.word_soft),
                        Mode::Sentence => theme::chip(ui, "英文", p.sentence, p.sentence_soft),
                    };
                    ui.label(RichText::new("選択中").size(12.0).color(p.weak));
                    if can_explain {
                        self.explain_button_at_right(ui);
                    }
                });
                ui.add_space(2.0);
                match sel.mode {
                    Mode::Word => {
                        ui.label(RichText::new(&sel.text).size(22.0).strong().color(p.strong));
                    }
                    Mode::Sentence => {
                        let text = RichText::new(&sel.text).size(15.5).color(p.strong);
                        if mobile {
                            // 長い英文で解説が画面の外に出ないよう、数行にまとめる
                            let mut job = egui::text::LayoutJob::simple(
                                sel.text.clone(),
                                theme::body_font(15.5),
                                p.strong,
                                ui.available_width(),
                            );
                            job.wrap.max_rows = SELECTION_PREVIEW_ROWS;
                            ui.label(job);
                        } else {
                            ui.label(text);
                        }
                    }
                }
                context_view(ui, &p, &sel);
            });
        }

        // 辞書
        if sel.mode == Mode::Word {
            if !dict_first {
                ui.add_space(10.0);
            }
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::section_title(ui, "辞書");
                    if dict_first && can_explain {
                        self.explain_button_at_right(ui);
                    }
                });
                let retry = {
                    let s = self.shared.borrow();
                    dict_view(ui, &p, &s.dict, &s.dict_ai, s.show_kana)
                };
                if retry {
                    state::fetch_dict_ai(&self.shared, ui.ctx());
                }
                if dict_first {
                    context_view(ui, &p, &sel);
                }
            });
        }

        ui.add_space(10.0);
        self.explain_card(ui, &p);

        if self.shared.borrow().local.enabled {
            ui.add_space(10.0);
            self.local_llm_card(ui, &p, &sel);
        }
    }

    /// 実験モード: 端末の LLM の状態と、選択中のテキストの訳・計測値。
    fn local_llm_card(&mut self, ui: &mut egui::Ui, p: &Palette, sel: &dom::SelectionInfo) {
        use crate::local_llm::{self, LocalPhase};
        let local = self.shared.borrow().local.clone();
        let ctx = ui.ctx().clone();
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section_title(ui, "🧪 端末の LLM（実験）");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::subtle_button(ui, "オフ")
                        .on_hover_text("実験モードをオフにして、モデルをメモリから外します（ダウンロードしたモデルは端末に残ります）")
                        .clicked()
                    {
                        local_llm::set_enabled(&self.shared, &ctx, false);
                    }
                });
            });

            // モデルの選択（サーバーにあるもの）
            if !local.models.is_empty() {
                let selected = local
                    .current_model()
                    .map(|m| m.label.clone())
                    .unwrap_or_default();
                let mut picked = None;
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("local-llm-model")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for m in &local.models {
                                let label = format!(
                                    "{}  {} · GPU {:.1} GB",
                                    m.label,
                                    format_bytes(m.bytes as i64),
                                    f64::from(m.vram_mb) / 1024.0
                                );
                                if ui
                                    .selectable_label(m.id == local.model_id, label)
                                    .on_hover_text(&m.note)
                                    .clicked()
                                {
                                    picked = Some(m.id.clone());
                                }
                            }
                        });
                    let loading = matches!(local.phase, LocalPhase::Loading { .. });
                    if !loading
                        && theme::subtle_button(ui, "削除")
                            .on_hover_text("このモデルを端末から消し、実験モードをオフにします")
                            .clicked()
                    {
                        local_llm::delete_model(&self.shared, &ctx);
                    }
                });
                if let Some(id) = picked {
                    local_llm::select_model(&self.shared, &ctx, &id);
                }
            }

            match &local.phase {
                LocalPhase::Off => {}
                LocalPhase::Unsupported => notice(
                    ui,
                    p,
                    "このブラウザは WebGPU に対応していないため、端末では動かせません",
                ),
                LocalPhase::Preparing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("準備しています…").color(p.weak));
                    });
                }
                LocalPhase::Loading { progress, text } => {
                    ui.add(
                        egui::ProgressBar::new(*progress)
                            .show_percentage()
                            .desired_height(14.0),
                    );
                    ui.label(
                        RichText::new(if text.is_empty() {
                            "モデルをダウンロードしています（初回のみ。次からは端末から読みます）"
                        } else {
                            text.as_str()
                        })
                        .size(12.0)
                        .color(p.weak),
                    );
                }
                LocalPhase::Ready { load_secs } => {
                    ui.label(
                        RichText::new(format!("準備完了（読み込み {load_secs:.1} 秒）"))
                            .size(12.5)
                            .color(p.ok),
                    );
                }
                LocalPhase::Error(e) => notice(ui, p, e),
            }

            if !matches!(local.phase, LocalPhase::Ready { .. }) {
                return;
            }
            let run = &local.run;
            let same_target = run
                .target
                .as_ref()
                .is_some_and(|(m, t)| *m == sel.mode && *t == sel.text);
            ui.horizontal(|ui| {
                if run.running {
                    ui.spinner();
                    if theme::subtle_button(ui, "停止").clicked() {
                        local_llm::interrupt();
                    }
                } else if theme::primary_button(
                    ui,
                    match sel.mode {
                        Mode::Word => "端末で訳す",
                        Mode::Sentence => "端末で和訳",
                    },
                    true,
                )
                .on_hover_text("この端末の LLM で訳し、速さを測ります（Mac の LLM は使いません）")
                .clicked()
                {
                    local_llm::translate(&self.shared, &ctx);
                }
            });
            if !same_target {
                return;
            }
            if !run.text.is_empty() {
                ui.label(RichText::new(run.text.trim()).size(14.5).color(p.text));
            }
            if let Some(e) = &run.error {
                notice(ui, p, e);
            }
            if let Some(st) = &run.stats {
                ui.label(
                    RichText::new(format!(
                        "最初の文字 {:.1} 秒 · 合計 {:.1} 秒 · {} トークン · 生成 {:.1} トークン/秒 · 読み込み {:.0} トークン/秒",
                        st.ttft_ms / 1000.0,
                        st.total_ms / 1000.0,
                        st.tokens,
                        st.decode_tps,
                        st.prefill_tps
                    ))
                    .size(12.0)
                    .color(p.weak),
                );
            }
        });
    }

    fn explain_card(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        let (phase, running, from_cache, markdown, is_sentence, show_slash) = {
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
                s.show_slash,
            )
        };
        // スラッシュリーディングを表示しない設定なら、その節を隠す（生成し直さない）
        let markdown = if show_slash {
            markdown
        } else {
            without_section(&markdown, SLASH_READING_HEADING).into_owned()
        };
        let markdown = plain_arrows(&markdown).into_owned();
        // 表示する設定なのに付いていない解説（設定を変える前の解説）なら、付けて作り直せるようにする
        let missing_slash = show_slash
            && is_sentence
            && phase == ExplainPhase::Done
            && !markdown.contains(SLASH_READING_HEADING);

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

            if missing_slash {
                ui.add_space(4.0);
                if theme::subtle_button(ui, "／ スラッシュリーディングを付けて解説し直す")
                    .on_hover_text("この解説はスラッシュリーディングなしで作られています")
                    .clicked()
                {
                    state::start_explain(&self.shared, &ctx, true);
                }
            }

            ui.add_space(4.0);
            ui.separator();
            let mut slash = show_slash;
            if is_sentence
                && ui
                    .checkbox(
                        &mut slash,
                        RichText::new("スラッシュリーディング").size(13.0),
                    )
                    .on_hover_text(
                        "意味のまとまりごとの区切り（/）と、語順どおりの直訳を表示します",
                    )
                    .changed()
            {
                self.shared.borrow_mut().set_show_slash(slash);
            }
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
        let (tags, received, selected, found, running) = {
            let s = self.shared.borrow();
            (
                s.explain.tags.clone(),
                s.explain.tags_received,
                s.explain.selected_tag,
                s.explain.highlight_found,
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
                let is_selected = selected == Some(i);
                let button = egui::Button::new(RichText::new(&tag.name).size(12.5).color(p.accent))
                    .fill(p.accent_soft)
                    .stroke(if is_selected {
                        egui::Stroke::new(1.5, p.accent)
                    } else {
                        egui::Stroke::NONE
                    })
                    .corner_radius(egui::CornerRadius::same(13));
                if ui
                    .add(button)
                    .on_hover_text("クリックで本文の該当箇所と説明を表示")
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
            tag_detail(ui, p, tag, found);
        }
        ui.add_space(4.0);
    }

    // ───────────── 文法項目タブ ─────────────

    fn grammar_tab(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let ctx = ui.ctx().clone();
        let (stale, order) = {
            let s = self.shared.borrow();
            (s.grammar_stale, s.grammar_order)
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

        let mut new_order = order;
        theme::segmented(
            ui,
            &mut new_order,
            &[
                (GrammarOrder::Category, "カテゴリ順"),
                (GrammarOrder::Frequency, "よく出る順"),
            ],
        );
        // 切り替えの横に並べると、狭い画面で右にはみ出す
        let total: u64 = g.tags.iter().map(|t| t.count).sum();
        ui.label(
            RichText::new(format!("解説した英文での出現 計 {total} 回"))
                .size(12.0)
                .color(p.weak),
        );
        if new_order != order {
            self.shared.borrow_mut().grammar_order = new_order;
        }
        ui.add_space(8.0);

        match new_order {
            GrammarOrder::Category => grammar_by_category(ui, &p, &g),
            GrammarOrder::Frequency => grammar_by_frequency(ui, &p, &g),
        }
    }

    // ───────────── 検索タブ ─────────────

    fn search_tab(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let ctx = ui.ctx().clone();
        self.refresh_read_later_if_stale(&ctx);

        let (view, later_count) = {
            let s = self.shared.borrow();
            (s.search_view, s.read_later.items.len())
        };
        let later_label = if later_count > 0 {
            format!("後で読む {later_count}")
        } else {
            "後で読む".to_string()
        };
        let mut new_view = view;
        if theme::segmented(
            ui,
            &mut new_view,
            &[
                (SearchView::Results, "検索結果"),
                (SearchView::Later, later_label.as_str()),
            ],
        ) {
            let mut s = self.shared.borrow_mut();
            s.search_view = new_view;
            s.read_later.stale = true;
        }
        ui.add_space(8.0);
        if let Some(e) = self.shared.borrow().read_later.error.clone() {
            notice(ui, &p, &e);
            ui.add_space(6.0);
        }
        if new_view == SearchView::Later {
            self.read_later_list(ui, &p, None);
            return;
        }
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
        let mut action = None;
        for item in &search.items {
            let later = self.shared.borrow().read_later.contains(&item.id);
            let is_open = current.as_deref() == Some(&item.id);
            if let Some(a) = search_result(ui, &p, item, is_open, later) {
                action = Some((a, item));
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
        match action {
            Some((SearchAction::Open, item)) => {
                let url = format!("https://arxiv.org/abs/{}", item.id);
                state::open_paper(&self.shared, &ctx, url);
            }
            Some((SearchAction::AddLater, item)) => state::add_read_later(
                &self.shared,
                &ctx,
                ReadLaterPut {
                    paper_id: item.id.clone(),
                    title: item.title.clone(),
                    authors: item.authors.clone(),
                },
            ),
            Some((SearchAction::RemoveLater, item)) => {
                state::remove_read_later(&self.shared, &ctx, item.id.clone());
            }
            None => {}
        }
    }

    fn refresh_read_later_if_stale(&self, ctx: &egui::Context) {
        let stale = self.shared.borrow().read_later.stale;
        if stale {
            state::refresh_read_later(&self.shared, ctx);
        }
    }

    /// 後で読む論文の一覧（後から積んだ順）。`limit` があれば先頭のその件数だけ出す。
    fn read_later_list(&mut self, ui: &mut egui::Ui, p: &Palette, limit: Option<usize>) {
        let ctx = ui.ctx().clone();
        let later = self.shared.borrow().read_later.clone();
        if limit.is_some() {
            theme::section_title(ui, &format!("後で読む（{} 件）", later.items.len()));
            ui.add_space(4.0);
        } else if let Some(e) = &later.load_error {
            notice(ui, p, &format!("後で読む: {e}"));
            if theme::subtle_button(ui, "再試行").clicked() {
                self.shared.borrow_mut().read_later.stale = true;
            }
            ui.add_space(6.0);
        }
        if later.items.is_empty() {
            if later.loaded {
                ui.label(
                    RichText::new(
                        "後で読む論文はありません。検索結果の「＋ 後で読む」で、ここに積めます。",
                    )
                    .color(p.weak),
                );
            } else if later.load_error.is_none() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("読み込み中…").color(p.weak));
                });
            }
            return;
        }

        let current = self.shared.borrow().paper_ref().map(|(id, _)| id);
        let mut action = None;
        for item in later.items.iter().take(limit.unwrap_or(usize::MAX)) {
            // 表示中の論文の ID には版が付くことがある
            let is_open = current
                .as_deref()
                .is_some_and(|c| is_same_paper(c, &item.id));
            if let Some(a) = read_later_row(ui, p, item, is_open) {
                action = Some((a, item.id.clone()));
            }
            ui.add_space(6.0);
        }
        match action {
            Some((ReadLaterAction::Open, id)) => {
                state::open_paper(&self.shared, &ctx, format!("https://arxiv.org/abs/{id}"));
            }
            Some((ReadLaterAction::Remove, id)) => {
                state::remove_read_later(&self.shared, &ctx, id);
            }
            None => {}
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

        self.sync_recent_bar();
        let requested = self.shared.borrow_mut().requested_theme.take();
        if let Some(theme) = requested {
            self.theme = theme;
            dom::apply_theme(theme);
            set_theme(&ctx, theme);
        }
        let p = theme::palette(&ctx);
        self.search_input_placed = false;
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
                let (tab, has_paper, selected) = {
                    let s = self.shared.borrow();
                    (
                        s.tab,
                        s.paper_ref().is_some(),
                        s.selection.as_ref().map(|sel| sel.mode),
                    )
                };
                // スマホでシートを下げているときに選んだら、見出しの代わりに小さな表示を出す
                // （単語は和訳、英文は「解説する」）。
                // ほかは何も出さない（高さが足りず、下の部品が半分だけ見えるため）
                if mobile
                    && dom::sheet() == dom::Sheet::Peek
                    && let Some(mode) = selected
                {
                    match mode {
                        Mode::Word => self.word_strip(ui, &p),
                        Mode::Sentence => self.sentence_strip(ui, &p),
                    }
                    return;
                }
                if mobile {
                    egui::Panel::bottom("bottom-tabs")
                        .show_separator_line(false)
                        .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                            top: 2,
                            ..Margin::ZERO
                        }))
                        .show(ui, |ui| self.bottom_tabs(ui));
                    // シートが「少し」のときは見出しとタブだけにする
                    if dom::sheet() == dom::Sheet::Peek {
                        self.header(ui);
                        return;
                    }
                }
                // 半分の高さで論文を読んでいる間は見出しを畳み、訳や解説を先に見せる
                let compact = mobile && has_paper && dom::sheet() == dom::Sheet::Half;
                if !compact {
                    self.header(ui);
                    ui.add_space(if mobile { 8.0 } else { 12.0 });
                }
                // 論文を読んでいる間は入力欄を隠す（読むモード）。「検索」タブで出す（検索モード）
                if show_input_bar(has_paper, tab) {
                    self.input_bar(ui);
                }
                self.paper_card(ui);
                if !mobile {
                    ui.add_space(12.0);
                    self.tabs(ui);
                    ui.add_space(10.0);
                } else if show_input_bar(has_paper, tab) {
                    ui.add_space(8.0);
                }
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

        if !self.search_input_placed {
            crate::search_input::hide();
        }

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

/// 論文の URL・検索の入力欄を出すか。論文を開く前と「検索」タブでだけ出し、
/// 論文を読んでいる間（ほかのタブ）は隠して本文と解説に場所を使う。
fn show_input_bar(has_paper: bool, tab: Tab) -> bool {
    !has_paper || tab == Tab::Search
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

/// 長い選択は、最初と最後の数語だけにする（どこからどこまで選べたかが分かるように）。
fn selection_ends(text: &str) -> String {
    const HEAD: usize = 2;
    const TAIL: usize = 3;
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= HEAD + TAIL + 1 {
        return text.to_string();
    }
    format!(
        "{} … {}",
        words[..HEAD].join(" "),
        words[words.len() - TAIL..].join(" ")
    )
}

/// 選択を含む段落（開くと読める）。
fn context_view(ui: &mut egui::Ui, p: &Palette, sel: &dom::SelectionInfo) {
    if sel.context == sel.text {
        return;
    }
    ui.add_space(2.0);
    egui::CollapsingHeader::new(RichText::new("段落（文脈）").size(12.5).color(p.weak))
        .id_salt("context")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(RichText::new(&sel.context).size(13.5).color(p.text));
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

enum SearchAction {
    Open,
    AddLater,
    RemoveLater,
}

enum ReadLaterAction {
    Open,
    Remove,
}

/// `current`（版が付くことがある）が、版なしの ID `base` の論文か。
fn is_same_paper(current: &str, base: &str) -> bool {
    current.strip_prefix(base).is_some_and(|rest| {
        rest.is_empty()
            || rest
                .strip_prefix('v')
                .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// 後で読む論文 1 件。タイトルか「開く」で開く。
fn read_later_row(
    ui: &mut egui::Ui,
    p: &Palette,
    item: &ReadLaterItem,
    is_open: bool,
) -> Option<ReadLaterAction> {
    let mut action = None;
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
        if title.clicked() && !is_open {
            action = Some(ReadLaterAction::Open);
        }
        ui.label(
            RichText::new(authors_line(&item.authors))
                .size(12.5)
                .color(p.text),
        );
        ui.label(
            RichText::new(format!(
                "arXiv:{} · {} に追加",
                item.id,
                format_time(item.added_at)
            ))
            .size(12.0)
            .color(p.weak),
        );
        ui.horizontal(|ui| {
            if is_open {
                theme::chip(ui, "表示中", p.ok, p.ok.gamma_multiply(0.15));
            } else if theme::primary_button(ui, "開く", true).clicked() {
                action = Some(ReadLaterAction::Open);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::subtle_button(ui, "外す")
                    .on_hover_text("後で読むから外します")
                    .clicked()
                {
                    action = Some(ReadLaterAction::Remove);
                }
            });
        });
    });
    action
}

/// 検索結果の 1 件。押された操作を返す（`later` は「後で読む」に積んであるか）。
fn search_result(
    ui: &mut egui::Ui,
    p: &Palette,
    item: &SearchItem,
    is_open: bool,
    later: bool,
) -> Option<SearchAction> {
    let mut open = false;
    let mut action = None;
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

        ui.horizontal_wrapped(|ui| {
            if is_open {
                theme::chip(ui, "表示中", p.ok, p.ok.gamma_multiply(0.15));
            } else if theme::primary_button(ui, "読む", true).clicked() {
                open = true;
            }
            if later {
                if theme::subtle_button(ui, "✓ 後で読むに追加済み")
                    .on_hover_text("もう一度押すと外します")
                    .clicked()
                {
                    action = Some(SearchAction::RemoveLater);
                }
            } else if theme::subtle_button(ui, "＋ 後で読む").clicked() {
                action = Some(SearchAction::AddLater);
            }
            ui.hyperlink_to(
                RichText::new("arXiv ↗").size(12.5),
                format!("https://arxiv.org/abs/{}", item.id),
            );
        });
    });
    if open && !is_open {
        action = Some(SearchAction::Open);
    }
    action
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
                    // 語の途中で折り返さず、入らなければまとめて次の行に送る
                    let resp = ui.add(
                        egui::Label::new(RichText::new(kana).size(15.0).color(p.text)).extend(),
                    );
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

/// 選んだ文法項目の説明。
fn tag_detail(ui: &mut egui::Ui, p: &Palette, tag: &ExplainTag, found: bool) {
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
            if tag.guide.summary.is_empty() {
                ui.label(RichText::new(&tag.hint).size(13.5).color(p.text));
            } else {
                ui.label(RichText::new(&tag.guide.summary).size(14.0).color(p.strong));
                guide_body(ui, p, &tag.guide);
            }
        });
}

/// 文法項目の解説のうち、要約より下（形・例文・TOEIC・注意）。空の欄は出さない。
fn guide_body(ui: &mut egui::Ui, p: &Palette, guide: &TagGuide) {
    guide_section(ui, p, "形", &guide.form);
    if !guide.examples.is_empty() {
        guide_heading(ui, p, "例文");
        for example in &guide.examples {
            ui.label(RichText::new(&example.en).size(13.5).color(p.strong));
            ui.label(RichText::new(&example.ja).size(12.5).color(p.weak));
        }
    }
    guide_section(ui, p, "TOEIC では", &guide.toeic);
    guide_section(ui, p, "注意", &guide.caution);
}

fn guide_section(ui: &mut egui::Ui, p: &Palette, title: &str, body: &str) {
    if !body.is_empty() {
        guide_heading(ui, p, title);
        ui.label(RichText::new(body).size(13.5).color(p.text));
    }
}

fn guide_heading(ui: &mut egui::Ui, p: &Palette, title: &str) {
    ui.add_space(5.0);
    ui.label(RichText::new(title).size(12.0).strong().color(p.accent));
}

/// カテゴリ → タグ。
fn grammar_by_category(ui: &mut egui::Ui, p: &Palette, g: &GrammarTagsResponse) {
    for cat in &g.categories {
        let tags: Vec<&GrammarTagInfo> = g.tags.iter().filter(|t| &t.category == cat).collect();
        if tags.is_empty() {
            continue;
        }
        let total: u64 = tags.iter().map(|t| t.count).sum();
        theme::card(ui, |ui| {
            egui::CollapsingHeader::new(
                RichText::new(format!("{cat}   出現 {total}"))
                    .strong()
                    .color(p.strong),
            )
            // 出現回数が 0 から増えたら開き直すよう、有無を ID に含める
            .id_salt(("grammar-cat", cat, total > 0))
            .default_open(total > 0)
            .show(ui, |ui| {
                for tag in tags {
                    grammar_tag_row(ui, p, tag, false);
                }
            });
        });
        ui.add_space(6.0);
    }
}

/// 出現回数の多い順（出てきたものだけ）。
fn grammar_by_frequency(ui: &mut egui::Ui, p: &Palette, g: &GrammarTagsResponse) {
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
            grammar_tag_row(ui, p, tag, true);
        }
    });
}

fn grammar_tag_row(ui: &mut egui::Ui, p: &Palette, tag: &GrammarTagInfo, with_category: bool) {
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
    if tag.guide.summary.is_empty() {
        return;
    }
    ui.indent(("grammar-guide", &tag.id), |ui| {
        ui.label(RichText::new(&tag.guide.summary).size(13.0).color(p.text));
        // 項目が多いので、要約より下は畳んでおく
        egui::CollapsingHeader::new(RichText::new("くわしく").size(12.5).color(p.accent))
            .id_salt(("grammar-more", &tag.id))
            .show(ui, |ui| guide_body(ui, p, &tag.guide));
    });
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
