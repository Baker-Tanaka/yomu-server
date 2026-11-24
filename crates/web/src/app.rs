//! egui の解説 UI。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use paper_reader_shared::{DictEntry, Mode};

use crate::dom;
use crate::state::{self, DictStatus, ExplainPhase, HealthStatus, PaperStatus, Shared, SharedRef};

pub struct PaperReaderApp {
    shared: SharedRef,
    url_input: String,
    markdown_cache: CommonMarkCache,
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
        state::refresh_health(&shared, &cc.egui_ctx);

        Self {
            shared,
            url_input: String::new(),
            markdown_cache: CommonMarkCache::default(),
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
            ui.weak("左の本文で単語をダブルクリックするか、英文をドラッグして選択してください。");
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
        let (phase, running) = {
            let s = self.shared.borrow();
            (s.explain.phase.clone(), s.explain.phase.is_running())
        };

        ui.horizontal_wrapped(|ui| {
            ui.heading("解説");
            if ui
                .add_enabled(!running, egui::Button::new("解説する"))
                .on_hover_text("選択中のテキストを LLM で解説します")
                .clicked()
            {
                state::start_explain(&self.shared, &ctx);
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
            ExplainPhase::Streaming | ExplainPhase::Done => {}
            ExplainPhase::Error(msg) => {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, msg);
                    if ui.button("再試行").clicked() {
                        state::start_explain(&self.shared, &ctx);
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

impl eframe::App for PaperReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            self.url_bar(ui);
            self.status_line(ui);
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| self.selection_view(ui));
        });
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
