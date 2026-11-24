//! egui の解説 UI。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use paper_reader_shared::{DictEntry, HistoryEntry, Mode};

use crate::dom;
use crate::state::{
    self, DictStatus, ExplainPhase, HealthStatus, HistoryScope, HistoryStatus, PaperStatus, Shared,
    SharedRef, Tab,
};

pub struct PaperReaderApp {
    shared: SharedRef,
    url_input: String,
    markdown_cache: CommonMarkCache,
    theme: dom::ThemeChoice,
}

impl PaperReaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        cc.egui_ctx.all_styles_mut(|style| {
            for (text_style, font) in style.text_styles.iter_mut() {
                font.size = match text_style {
                    egui::TextStyle::Heading => 20.0,
                    egui::TextStyle::Small => 11.0,
                    _ => 15.0,
                };
            }
        });

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
            url_input: String::new(),
            markdown_cache: CommonMarkCache::default(),
            theme,
        }
    }

    fn url_bar(&mut self, ui: &mut egui::Ui) {
        let loading = matches!(self.shared.borrow().paper, PaperStatus::Loading);
        let mut submit = false;
        ui.horizontal(|ui| {
            let button_width = 56.0;
            let edit = ui.add_sized(
                [ui.available_width() - button_width - 8.0, 28.0],
                egui::TextEdit::singleline(&mut self.url_input)
                    .hint_text("arXiv の URL（例: https://arxiv.org/abs/1706.03762）"),
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            if ui
                .add_enabled(
                    !loading,
                    egui::Button::new("開く").min_size([button_width, 28.0].into()),
                )
                .clicked()
            {
                submit = true;
            }
        });
        let url = self.url_input.trim().to_string();
        if submit && !loading && !url.is_empty() {
            state::open_paper(&self.shared, ui.ctx(), url);
        }

        match &self.shared.borrow().paper {
            PaperStatus::Empty => {
                ui.weak("論文の URL を入力して「開く」を押してください。");
            }
            PaperStatus::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("論文を読み込み中…");
                });
            }
            PaperStatus::Loaded {
                id,
                title,
                source_url,
            } => {
                ui.label(egui::RichText::new(title).strong());
                ui.horizontal_wrapped(|ui| {
                    ui.weak(format!("arXiv:{id}"));
                    ui.hyperlink_to("原文", source_url);
                });
            }
            PaperStatus::Error(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
        }
    }

    /// LLM の疎通状態（1 行）。
    fn status_line(&self, ui: &mut egui::Ui) {
        let mut recheck = false;
        ui.horizontal_wrapped(|ui| {
            let ok = egui::Color32::from_rgb(0x2e, 0x9d, 0x4a);
            let bad = ui.visuals().error_fg_color;
            match &self.shared.borrow().health {
                HealthStatus::Checking => {
                    ui.spinner();
                    ui.weak("LLM の状態を確認中…");
                }
                HealthStatus::Error(e) => {
                    ui.colored_label(bad, "●");
                    ui.weak(format!("サーバーに接続できません: {e}"));
                }
                HealthStatus::Ok(h) => match (h.llm, h.model_available) {
                    (Some(true), Some(true)) => {
                        ui.colored_label(ok, "●");
                        ui.weak(format!("LLM: {}", h.model));
                    }
                    (Some(true), _) => {
                        ui.colored_label(bad, "●");
                        ui.weak(format!(
                            "LLM は起動していますが、モデル「{}」がありません",
                            h.model
                        ));
                    }
                    _ => {
                        ui.colored_label(bad, "●");
                        ui.weak("LLM ランタイムに接続できません（Ollama が起動しているか確認してください）");
                    }
                },
            }
            if ui.small_button("再確認").clicked() {
                recheck = true;
            }
        });
        if recheck {
            state::refresh_health(&self.shared, ui.ctx());
        }
    }

    fn selection_view(&mut self, ui: &mut egui::Ui) {
        let Some(sel) = self.shared.borrow().selection.clone() else {
            ui.weak("本文の単語をダブルクリックするか、英文をドラッグして選択してください（iPhone などでは長押しで選択）。");
            ui.add_space(6.0);
            ui.weak("ショートカット: E で解説、Esc で選択解除");
            return;
        };

        ui.horizontal(|ui| {
            ui.heading("選択中のテキスト");
            let (label, color) = match sel.mode {
                Mode::Word => ("単語（word）", egui::Color32::from_rgb(0x2e, 0x7d, 0x32)),
                Mode::Sentence => (
                    "英文（sentence）",
                    egui::Color32::from_rgb(0x15, 0x65, 0xc0),
                ),
            };
            ui.label(
                egui::RichText::new(label)
                    .small()
                    .color(egui::Color32::WHITE)
                    .background_color(color),
            );
        });
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(&sel.text).size(16.0));
        });

        if sel.mode == Mode::Word {
            ui.add_space(8.0);
            dict_view(ui, &self.shared.borrow().dict);
        }

        ui.add_space(8.0);
        ui.separator();
        self.explain_view(ui);

        ui.add_space(12.0);
        egui::CollapsingHeader::new("段落（文脈）")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(&sel.context);
            });
    }

    fn explain_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let (phase, running, from_cache) = {
            let s = self.shared.borrow();
            (
                s.explain.phase.clone(),
                s.explain.phase.is_running(),
                s.explain.from_cache,
            )
        };

        ui.horizontal_wrapped(|ui| {
            ui.heading("解説");
            if ui
                .add_enabled(!running, egui::Button::new("解説する"))
                .on_hover_text("選択中のテキストを LLM で解説します（E キー）")
                .clicked()
            {
                state::start_explain(&self.shared, &ctx, false);
            }
            if phase == ExplainPhase::Done
                && ui
                    .button("再生成")
                    .on_hover_text("キャッシュを使わずに解説を作り直します")
                    .clicked()
            {
                state::start_explain(&self.shared, &ctx, true);
            }
            if running && ui.button("停止").clicked() {
                state::stop_explain(&self.shared);
            }
            let mut auto = self.shared.borrow().auto_explain;
            if ui.checkbox(&mut auto, "選択したら自動で解説").changed() {
                self.shared.borrow_mut().auto_explain = auto;
            }
        });

        match &phase {
            ExplainPhase::Idle => {
                ui.weak("「解説する」を押すと、和訳や文法の解説を表示します。");
            }
            ExplainPhase::Connecting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("解説を準備中…");
                });
            }
            ExplainPhase::Queued => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("順番待ち中…（ほかの解説を生成しています）");
                });
            }
            ExplainPhase::Done if from_cache => {
                ui.label(
                    egui::RichText::new("キャッシュから表示")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }
            ExplainPhase::Streaming | ExplainPhase::Done => {}
            ExplainPhase::Error(msg) => {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, msg);
                    if ui.button("再試行").clicked() {
                        state::start_explain(&self.shared, &ctx, false);
                    }
                });
            }
        }

        let markdown = self.shared.borrow().explain.markdown.clone();
        if !markdown.is_empty() {
            ui.add_space(4.0);
            CommonMarkViewer::new().show(ui, &mut self.markdown_cache, &markdown);
            if phase == ExplainPhase::Streaming {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("生成中…");
                });
            }
        }
    }
}

impl PaperReaderApp {
    fn tabs(&mut self, ui: &mut egui::Ui) {
        let mut tab = self.shared.borrow().tab;
        let mut theme = self.theme;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut tab, Tab::Explain, "解説");
            ui.selectable_value(&mut tab, Tab::History, "履歴");
            // 配色（論文ペインと解説ペインの両方）
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.selectable_value(&mut theme, dom::ThemeChoice::Dark, "ダーク");
                ui.selectable_value(&mut theme, dom::ThemeChoice::Light, "ライト");
                ui.selectable_value(&mut theme, dom::ThemeChoice::System, "自動")
                    .on_hover_text("OS の設定（ライト / ダーク）に合わせます");
                ui.weak("表示:");
            });
        });
        if theme != self.theme {
            self.theme = theme;
            dom::apply_theme(theme);
            set_theme(ui.ctx(), theme);
        }
        let mut s = self.shared.borrow_mut();
        if s.tab != tab {
            s.tab = tab;
            if tab == Tab::History {
                s.history_stale = true;
            }
        }
    }

    fn history_view(&mut self, ui: &mut egui::Ui) {
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
            ui.selectable_value(&mut new_scope, HistoryScope::ThisPaper, "この論文");
            ui.selectable_value(&mut new_scope, HistoryScope::AllPapers, "すべての論文");
            if ui.small_button("更新").clicked() {
                self.shared.borrow_mut().history_stale = true;
            }
        });
        if new_scope != scope {
            let mut s = self.shared.borrow_mut();
            s.history_scope = new_scope;
            s.history_stale = true;
        }
        ui.add_space(4.0);

        let status = self.shared.borrow().history.clone();
        let entries = match status {
            HistoryStatus::NotLoaded | HistoryStatus::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("履歴を読み込み中…");
                });
                return;
            }
            HistoryStatus::Error(e) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("履歴: {e}"));
                return;
            }
            HistoryStatus::Loaded(entries) => entries,
        };
        if entries.is_empty() {
            ui.weak(match (scope, has_paper) {
                (HistoryScope::ThisPaper, false) => {
                    "論文を開くと、その論文で調べた単語と解説した文がここに表示されます。"
                }
                _ => "まだ履歴はありません。単語を選択するか、英文を解説するとここに残ります。",
            });
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
                    ui.label(egui::RichText::new(&entry.paper_title).strong());
                    ui.weak(format!("arXiv:{}", entry.paper_id));
                    if current.as_deref() != Some(entry.paper_id.as_str())
                        && ui.small_button("論文を開く").clicked()
                    {
                        action = Some(HistoryAction::OpenPaper(entry.paper_id.clone()));
                    }
                });
            }
            if let Some(a) = history_row(ui, entry) {
                action = Some(a);
            }
        }

        match action {
            Some(HistoryAction::Show(entry)) => {
                state::show_history_entry(&self.shared, &ctx, &entry)
            }
            Some(HistoryAction::Delete(id)) => state::delete_history_entry(&self.shared, &ctx, id),
            Some(HistoryAction::OpenPaper(id)) => {
                let url = format!("https://arxiv.org/abs/{id}");
                self.url_input = url.clone();
                state::open_paper(&self.shared, &ctx, url);
            }
            None => {}
        }
    }
}

enum HistoryAction {
    Show(HistoryEntry),
    Delete(i64),
    OpenPaper(String),
}

/// 履歴の 1 行。クリックで再表示、× で削除。
fn history_row(ui: &mut egui::Ui, entry: &HistoryEntry) -> Option<HistoryAction> {
    let mut action = None;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (label, color) = match entry.mode {
                Mode::Word => ("単語", egui::Color32::from_rgb(0x2e, 0x7d, 0x32)),
                Mode::Sentence => ("英文", egui::Color32::from_rgb(0x15, 0x65, 0xc0)),
            };
            ui.label(
                egui::RichText::new(label)
                    .small()
                    .color(egui::Color32::WHITE)
                    .background_color(color),
            );
            ui.weak(format_time(entry.updated_at));
            if entry.has_explanation {
                ui.weak("・解説あり");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text("履歴から削除").clicked() {
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
        let text =
            egui::RichText::new(preview).size(if entry.mode == Mode::Word { 17.0 } else { 14.0 });
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

        egui::CentralPanel::default_margins().show(ui, |ui| {
            self.url_bar(ui);
            self.status_line(ui);
            ui.separator();
            self.tabs(ui);
            ui.separator();
            let tab = self.shared.borrow().tab;
            egui::ScrollArea::vertical()
                .id_salt(tab_salt(tab))
                .auto_shrink([false, false])
                .show(ui, |ui| match tab {
                    Tab::Explain => self.selection_view(ui),
                    Tab::History => self.history_view(ui),
                });
        });
    }
}

fn tab_salt(tab: Tab) -> &'static str {
    match tab {
        Tab::Explain => "explain_scroll",
        Tab::History => "history_scroll",
    }
}

/// 辞書の結果（見出し語と訳のリスト）。
fn dict_view(ui: &mut egui::Ui, status: &DictStatus) {
    match status {
        DictStatus::Idle => {}
        DictStatus::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("辞書を検索中…");
            });
        }
        DictStatus::Error(e) => {
            ui.colored_label(ui.visuals().error_fg_color, format!("辞書: {e}"));
        }
        DictStatus::Loaded(r) => match &r.headword {
            None => {
                ui.weak(format!("「{}」は辞書に見つかりませんでした。", r.query));
            }
            Some(headword) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(headword)
                            .size(24.0)
                            .strong()
                            .color(ui.visuals().strong_text_color()),
                    );
                    if *headword != r.query {
                        ui.weak(format!("（{} の原形）", r.query));
                    }
                });
                ui.add_space(2.0);
                meanings_list(ui, &r.meanings);
                for entry in &r.related {
                    related_view(ui, entry);
                }
            }
        },
    }
}

fn related_view(ui: &mut egui::Ui, entry: &DictEntry) {
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        ui.weak("原形の候補:");
        ui.label(
            egui::RichText::new(&entry.headword)
                .size(18.0)
                .strong()
                .color(ui.visuals().strong_text_color()),
        );
    });
    meanings_list(ui, &entry.meanings);
}

/// 訳を番号付きで並べる。元データの『』で囲まれた重要な訳は強調色で表示する。
fn meanings_list(ui: &mut egui::Ui, meanings: &[String]) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let normal = ui.visuals().text_color();
    let strong = ui.visuals().strong_text_color();
    let accent = ui.visuals().hyperlink_color;
    for (i, meaning) in meanings.iter().enumerate() {
        let mut job = egui::text::LayoutJob::default();
        job.append(
            &format!("{}. ", i + 1),
            0.0,
            egui::TextFormat::simple(font.clone(), accent),
        );
        for (text, emphasized) in split_emphasis(meaning) {
            let color = if emphasized { strong } else { normal };
            let mut format = egui::TextFormat::simple(font.clone(), color);
            if emphasized {
                format.underline = egui::Stroke::new(1.0, accent);
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
