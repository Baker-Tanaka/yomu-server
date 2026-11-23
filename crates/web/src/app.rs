//! egui の解説 UI。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use eframe::egui;
use paper_reader_shared::Mode;

use crate::dom::{self, SelectionInfo};

/// 論文の読み込み状態。
#[derive(Debug, Clone, Default)]
enum PaperStatus {
    #[default]
    Empty,
    Loading,
    Loaded {
        id: String,
        title: String,
        source_url: String,
    },
    Error(String),
}

/// DOM のイベントや非同期処理と共有する状態。
#[derive(Default)]
struct Shared {
    paper: PaperStatus,
    selection: Option<SelectionInfo>,
}

pub struct PaperReaderApp {
    shared: Rc<RefCell<Shared>>,
    url_input: String,
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

        let shared: Rc<RefCell<Shared>> = Rc::default();

        let ctx = cc.egui_ctx.clone();
        let shared_for_dom = shared.clone();
        if let Err(e) = dom::watch_selection(move |info| {
            shared_for_dom.borrow_mut().selection = Some(info);
            ctx.request_repaint();
        }) {
            log::error!("選択の監視を開始できません: {e}");
        }

        Self {
            shared,
            url_input: String::new(),
        }
    }

    fn open_paper(&mut self, ctx: &egui::Context) {
        let url = self.url_input.trim().to_string();
        if url.is_empty() {
            return;
        }
        self.shared.borrow_mut().paper = PaperStatus::Loading;
        let shared = self.shared.clone();
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = crate::api::fetch_paper(&url).await;
            let mut shared = shared.borrow_mut();
            match result {
                Ok(paper) => {
                    dom::set_paper_html(&paper.html, &paper.title);
                    shared.selection = None;
                    shared.paper = PaperStatus::Loaded {
                        id: paper.id,
                        title: paper.title,
                        source_url: paper.source_url,
                    };
                }
                // 失敗しても表示中の論文と選択はそのまま残す
                Err(e) => shared.paper = PaperStatus::Error(e),
            }
            ctx.request_repaint();
        });
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
        if submit && !loading {
            self.open_paper(ui.ctx());
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

    fn selection_view(&self, ui: &mut egui::Ui) {
        let shared = self.shared.borrow();
        let Some(sel) = &shared.selection else {
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

        ui.add_space(12.0);
        ui.heading("段落（文脈）");
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(&sel.context);
        });
    }
}

impl eframe::App for PaperReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            self.url_bar(ui);
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| self.selection_view(ui));
        });
    }
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
