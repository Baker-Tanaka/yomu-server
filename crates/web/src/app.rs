//! egui の解説 UI。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use eframe::egui;
use paper_reader_shared::{DictEntry, DictResponse, Mode};

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

/// 辞書の検索状態。
#[derive(Debug, Clone, Default)]
enum DictStatus {
    /// 単語が選択されていない
    #[default]
    Idle,
    Loading,
    Loaded(DictResponse),
    Error(String),
}

/// DOM のイベントや非同期処理と共有する状態。
#[derive(Default)]
struct Shared {
    paper: PaperStatus,
    selection: Option<SelectionInfo>,
    dict: DictStatus,
    /// 選択ごとに増える番号。古い選択に対する応答を捨てるために使う
    selection_seq: u64,
}

impl Shared {
    /// 新しい選択を反映し、単語なら辞書を引き始める。
    fn on_select(this: &Rc<RefCell<Self>>, ctx: &egui::Context, info: SelectionInfo) {
        let (seq, word) = {
            let mut s = this.borrow_mut();
            s.selection_seq += 1;
            let word = (info.mode == Mode::Word).then(|| info.text.clone());
            s.dict = if word.is_some() {
                DictStatus::Loading
            } else {
                DictStatus::Idle
            };
            s.selection = Some(info);
            (s.selection_seq, word)
        };
        ctx.request_repaint();

        let Some(word) = word else {
            return;
        };
        let this = this.clone();
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = crate::api::fetch_dict(&word).await;
            let mut s = this.borrow_mut();
            // 応答待ちの間に別の選択が来ていたら、この結果は捨てる
            if s.selection_seq != seq {
                return;
            }
            s.dict = match result {
                Ok(r) => DictStatus::Loaded(r),
                Err(e) => DictStatus::Error(e),
            };
            ctx.request_repaint();
        });
    }
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
            Shared::on_select(&shared_for_dom, &ctx, info);
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
                    shared.dict = DictStatus::Idle;
                    shared.selection_seq += 1;
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

        if sel.mode == Mode::Word {
            dict_view(ui, &shared.dict);
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
        }

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
