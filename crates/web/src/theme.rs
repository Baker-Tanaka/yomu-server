//! 解説ペイン（egui）の配色と、カード・チップ・ボタンなどの共通部品。
//!
//! 色は論文ペインの paper.css（--paper-bg など）となじむように合わせている。

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, RichText, Stroke, TextStyle, Theme, Vec2,
};

/// 配色。
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// ペインの背景
    pub bg: Color32,
    /// カードの背景
    pub card: Color32,
    pub card_stroke: Color32,
    pub text: Color32,
    pub strong: Color32,
    pub weak: Color32,
    pub accent: Color32,
    /// アクセントの薄い背景（選択中のタブ、チップなど）
    pub accent_soft: Color32,
    /// アクセント色の上に置く文字
    pub on_accent: Color32,
    pub ok: Color32,
    pub error: Color32,
    pub error_soft: Color32,
    /// 単語・英文の色
    pub word: Color32,
    pub word_soft: Color32,
    pub sentence: Color32,
    pub sentence_soft: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const LIGHT: Palette = Palette {
    bg: hex(0xf4f2ee),
    card: hex(0xffffff),
    card_stroke: hex(0xe6e2da),
    text: hex(0x2b2f36),
    strong: hex(0x14171c),
    weak: hex(0x7a7f88),
    accent: hex(0x4f5bd5),
    accent_soft: hex(0xe7e9fb),
    on_accent: hex(0xffffff),
    ok: hex(0x2f9e5b),
    error: hex(0xc4372f),
    error_soft: hex(0xfcebea),
    word: hex(0x1e7a52),
    word_soft: hex(0xdff3e8),
    sentence: hex(0x3d5bd1),
    sentence_soft: hex(0xe3e8fb),
};

pub const DARK: Palette = Palette {
    bg: hex(0x1a1b1f),
    card: hex(0x24262c),
    card_stroke: hex(0x32353d),
    text: hex(0xd9dbe0),
    strong: hex(0xf3f4f6),
    weak: hex(0x8d929c),
    accent: hex(0x8c96ff),
    accent_soft: hex(0x2e3156),
    on_accent: hex(0x15162a),
    ok: hex(0x4cc282),
    error: hex(0xff7b72),
    error_soft: hex(0x3a2224),
    word: hex(0x5fd39a),
    word_soft: hex(0x1d3a2c),
    sentence: hex(0x8fa5ff),
    sentence_soft: hex(0x262d52),
};

pub fn palette(ctx: &egui::Context) -> Palette {
    match ctx.theme() {
        Theme::Dark => DARK,
        Theme::Light => LIGHT,
    }
}

/// ライト・ダークの両方の見た目を設定する（どちらを使うかは ThemePreference で決まる）。
pub fn install(ctx: &egui::Context) {
    for (theme, p) in [(Theme::Light, LIGHT), (Theme::Dark, DARK)] {
        ctx.set_visuals_of(theme, visuals(theme, &p));
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = Vec2::new(8.0, 7.0);
            style.spacing.button_padding = Vec2::new(10.0, 4.0);
            style.spacing.interact_size.y = 26.0;
            style.spacing.indent = 16.0;
            for (text_style, font) in style.text_styles.iter_mut() {
                font.size = match text_style {
                    TextStyle::Heading => 19.0,
                    TextStyle::Small => 11.5,
                    TextStyle::Button => 14.0,
                    TextStyle::Monospace => 13.5,
                    _ => 15.0,
                };
            }
        });
    }
}

/// PC 用とスマホ用で、文字の大きさと押せる範囲を切り替える。
/// スマホでは指で押しやすいよう、ボタンを高く・間隔を広く・文字を少し大きくする。
pub fn set_mobile(ctx: &egui::Context, mobile: bool) {
    ctx.all_styles_mut(|style| {
        let sp = &mut style.spacing;
        if mobile {
            sp.item_spacing = Vec2::new(8.0, 9.0);
            sp.button_padding = Vec2::new(12.0, 7.0);
            sp.interact_size.y = 36.0;
            sp.icon_width = 20.0;
            sp.icon_width_inner = 12.0;
        } else {
            sp.item_spacing = Vec2::new(8.0, 7.0);
            sp.button_padding = Vec2::new(10.0, 4.0);
            sp.interact_size.y = 26.0;
            sp.icon_width = 14.0;
            sp.icon_width_inner = 8.0;
        }
        for (text_style, font) in style.text_styles.iter_mut() {
            font.size = match (text_style, mobile) {
                (TextStyle::Heading, false) => 19.0,
                (TextStyle::Heading, true) => 20.0,
                (TextStyle::Small, false) => 11.5,
                (TextStyle::Small, true) => 12.5,
                (TextStyle::Button, false) => 14.0,
                (TextStyle::Button, true) => 15.5,
                (TextStyle::Monospace, false) => 13.5,
                (TextStyle::Monospace, true) => 14.0,
                (_, false) => 15.0,
                (_, true) => 16.0,
            };
        }
    });
}

fn visuals(theme: Theme, p: &Palette) -> egui::Visuals {
    let mut v = match theme {
        Theme::Dark => egui::Visuals::dark(),
        Theme::Light => egui::Visuals::light(),
    };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0, p.card_stroke);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = p.card;
    v.faint_bg_color = p.card;
    v.code_bg_color = p.accent_soft;
    v.hyperlink_color = p.accent;
    v.error_fg_color = p.error;
    v.override_text_color = None;
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.5, p.accent);

    let radius = CornerRadius::same(8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.card_stroke);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;

    let (inactive, hovered, active) = match theme {
        Theme::Dark => (hex(0x2c2f36), hex(0x353943), hex(0x40444f)),
        Theme::Light => (hex(0xebe8e2), hex(0xe2ded6), hex(0xd6d1c7)),
    };
    for (wv, fill) in [
        (&mut w.inactive, inactive),
        (&mut w.hovered, hovered),
        (&mut w.active, active),
        (&mut w.open, hovered),
    ] {
        wv.bg_fill = fill;
        wv.weak_bg_fill = fill;
        wv.corner_radius = radius;
        wv.expansion = 0.0;
    }
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.6));
    w.hovered.fg_stroke = Stroke::new(1.5, p.strong);
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.5, p.strong);
    v
}

/// 角丸のカード。
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = palette(ui.ctx());
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.card_stroke))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// 小さな角丸のラベル（「単語」「cs.CL」など）。
pub fn chip(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.5).color(fg));
        })
        .response
}

/// セクションの見出し（小さめ・太字）。
pub fn section_title(ui: &mut egui::Ui, text: &str) {
    let p = palette(ui.ctx());
    ui.label(RichText::new(text).size(13.0).strong().color(p.weak));
}

/// 主要な操作のボタン（アクセント色）。
pub fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let p = palette(ui.ctx());
    let button = egui::Button::new(RichText::new(text).color(p.on_accent).strong())
        .fill(p.accent)
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(0.0, 28.0));
    ui.add_enabled(enabled, button)
}

/// 控えめなボタン（枠だけ）。
pub fn subtle_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = palette(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(text).size(13.0).color(p.text))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, p.card_stroke))
            .corner_radius(CornerRadius::same(8)),
    )
}

/// 横並びの切り替え（タブなど）。選ばれたものはアクセント色。
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let p = palette(ui.ctx());
    let mut changed = false;
    egui::Frame::new()
        .fill(match ui.ctx().theme() {
            Theme::Dark => hex(0x232529),
            Theme::Light => hex(0xe9e6df),
        })
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for (opt, label) in options {
                    let selected = *value == *opt;
                    let text = if selected {
                        RichText::new(*label).strong().color(p.strong)
                    } else {
                        RichText::new(*label).color(p.weak)
                    };
                    let button = egui::Button::new(text)
                        .fill(if selected {
                            p.card
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(if selected {
                            Stroke::new(1.0, p.card_stroke)
                        } else {
                            Stroke::NONE
                        })
                        .corner_radius(CornerRadius::same(7))
                        .min_size(Vec2::new(0.0, 24.0));
                    if ui.add(button).clicked() && !selected {
                        *value = *opt;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// 本文用のフォント。
pub fn body_font(size: f32) -> FontId {
    FontId::proportional(size)
}
