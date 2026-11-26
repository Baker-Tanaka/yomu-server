//! スラッシュリーディングの表示。意味のまとまりごとに英語（上）と直訳（下）を組にして、
//! 英語の語順のまま左から右へ「／」で区切って並べる（入り切らなければ次の行へ折り返す）。

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Galley, Pos2, Rect, Sense, Stroke, Vec2};
use paper_reader_shared::SlashChunk;

use crate::theme::{self, Palette};

const EN_SIZE: f32 = 15.5;
const JA_SIZE: f32 = 12.5;
/// まとまりの内側の余白
const PAD: Vec2 = Vec2::new(8.0, 5.0);
/// 区切りの「／」を置く幅と、行の間隔
const SLASH_W: f32 = 14.0;
const ROW_GAP: f32 = 8.0;
/// 英語と直訳のあいだ（下線を引く）
const LINE_GAP: f32 = 7.0;

struct Block {
    en: Arc<Galley>,
    ja: Option<Arc<Galley>>,
    size: Vec2,
}

pub fn show(ui: &mut egui::Ui, p: &Palette, chunks: &[SlashChunk]) {
    let width = ui.available_width();
    let inner = (width - PAD.x * 2.0 - SLASH_W).max(40.0);
    let layout = |ui: &egui::Ui, text: &str, size: f32, color: Color32, wrap: f32| {
        ui.painter()
            .layout(text.to_owned(), theme::body_font(size), color, wrap)
    };
    let blocks: Vec<Block> = chunks
        .iter()
        .map(|c| {
            let en = layout(ui, &c.en, EN_SIZE, p.strong, inner);
            // 直訳が長いときは、英語より少し広いところで折り返して縦に伸びすぎないようにする
            let ja_wrap = (en.size().x * 1.4).max(150.0).min(inner);
            let ja = (!c.ja.is_empty()).then(|| layout(ui, &c.ja, JA_SIZE, p.weak, ja_wrap));
            let text_w = en.size().x.max(ja.as_ref().map_or(0.0, |g| g.size().x));
            let text_h = en.size().y + ja.as_ref().map_or(0.0, |g| LINE_GAP + g.size().y);
            Block {
                en,
                ja,
                size: Vec2::new(text_w, text_h) + PAD * 2.0,
            }
        })
        .collect();

    // 先に置き場所（左上からの相対位置と、行の高さ）を決める
    let mut rows: Vec<(usize, usize, f32)> = Vec::new();
    let (mut start, mut x, mut h) = (0, 0.0_f32, 0.0_f32);
    for (i, b) in blocks.iter().enumerate() {
        let need = b.size.x + if i + 1 < blocks.len() { SLASH_W } else { 0.0 };
        if i > start && x + need > width {
            rows.push((start, i, h));
            (start, x, h) = (i, 0.0, 0.0);
        }
        x += b.size.x + SLASH_W;
        h = h.max(b.size.y);
    }
    if start < blocks.len() {
        rows.push((start, blocks.len(), h));
    }
    let height =
        rows.iter().map(|r| r.2).sum::<f32>() + ROW_GAP * rows.len().saturating_sub(1) as f32;
    let (area, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    // 読み上げ用には、区切った英文と直訳を文章として渡す
    response.widget_info(|| {
        let text: Vec<String> = chunks
            .iter()
            .map(|c| format!("{} {}", c.en, c.ja))
            .collect();
        egui::WidgetInfo::labeled(egui::Role::Label, true, text.join(" / "))
    });
    if !ui.is_rect_visible(area) {
        return;
    }

    let painter = ui.painter();
    let mut y = area.top();
    for (from, to, row_h) in rows {
        let mut x = area.left();
        for (i, b) in blocks.iter().enumerate().take(to).skip(from) {
            let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(b.size.x, row_h));
            painter.rect_filled(
                rect,
                CornerRadius::same(8),
                p.accent_soft.gamma_multiply(0.55),
            );
            let at = rect.min + PAD;
            painter.galley(at, b.en.clone(), p.strong);
            if let Some(ja) = &b.ja {
                let line_y = at.y + b.en.size().y + LINE_GAP * 0.5;
                painter.hline(
                    at.x..=rect.right() - PAD.x,
                    line_y,
                    Stroke::new(1.0, p.accent.gamma_multiply(0.45)),
                );
                painter.galley(Pos2::new(at.x, line_y + LINE_GAP * 0.5), ja.clone(), p.weak);
            }
            x += b.size.x;
            if i + 1 < blocks.len() {
                let center = Pos2::new(x + SLASH_W * 0.5, y + PAD.y + EN_SIZE * 0.6);
                painter.line_segment(
                    [center + Vec2::new(-3.0, 8.0), center + Vec2::new(3.0, -8.0)],
                    Stroke::new(2.0, p.accent),
                );
            }
            x += SLASH_W;
        }
        y += row_h + ROW_GAP;
    }
}
