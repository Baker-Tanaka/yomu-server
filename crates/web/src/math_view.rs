//! LaTeX の数式を組んで描く（解析は shared の `mathtex.rs`）。
//!
//! 数式用のフォントは持たず、画面のフォントの文字と線で描く（ハット、根号、伸びるかっこは線）。
//! 解説の Markdown の `$…$` は、egui_commonmark の数式の描画に `show` を渡して組む。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, FontId, Galley, Painter, Pos2, Shape, Stroke, pos2, vec2};
use paper_reader_shared::{AccentKind, AtomClass, MathNode, TableAlign, parse_math};

/// 本文の文字に対する数式の大きさ
const SCALE: f32 = 1.1;
/// 添字の大きさと、その下限
const SCRIPT_SCALE: f32 = 0.72;
const MIN_SIZE: f32 = 8.5;
/// 横幅に収めるために縮める下限。これより小さくせず、横にスクロールする
const MIN_FIT: f32 = 0.75;
/// 文の中の数式が行からこれだけはみ出すのは許し、行の高さを変えない（文字の大きさに対する比）
const LINE_OVERFLOW: f32 = 0.22;
/// 斜体の文字は上が右へはみ出すので、後ろに足す幅
const ITALIC_CORRECTION: f32 = 0.08;
/// 分数の線や = の中心の高さ（文字の大きさに対する比）
const AXIS: f32 = 0.28;
/// これより長い LaTeX は組まずに、そのまま出す
const MAX_TEX_BYTES: usize = 4_000;
/// 解析した結果を覚えておく数
const CACHE_MAX: usize = 400;

thread_local! {
    static CACHE: RefCell<HashMap<String, Rc<MathNode>>> = RefCell::new(HashMap::new());
}

/// 描くもの。座標は、左端のベースラインを原点にする。
enum Op {
    Text(Pos2, Arc<Galley>),
    Line(Vec<Pos2>, f32),
    Dot(Pos2, f32),
}

/// 組んだ結果。`a` はベースラインより上の高さ、`d` は下の深さ。
#[derive(Default)]
struct Laid {
    w: f32,
    a: f32,
    d: f32,
    ops: Vec<Op>,
}

impl Laid {
    /// `child` を (x, ベースラインから y) に置く（y は下が正）。高さと深さを広げる。幅は変えない。
    fn put(&mut self, child: Laid, x: f32, y: f32) {
        let delta = vec2(x, y);
        self.a = self.a.max(child.a - y);
        self.d = self.d.max(child.d + y);
        self.ops.extend(child.ops.into_iter().map(|op| match op {
            Op::Text(p, g) => Op::Text(p + delta, g),
            Op::Line(points, w) => Op::Line(points.into_iter().map(|p| p + delta).collect(), w),
            Op::Dot(p, r) => Op::Dot(p + delta, r),
        }));
    }

    fn append(&mut self, child: Laid, gap: f32) {
        let x = self.w + gap;
        self.w = x + child.w;
        self.put(child, x, 0.0);
    }

    fn line(&mut self, points: Vec<Pos2>, width: f32) {
        self.ops.push(Op::Line(points, width));
    }
}

#[derive(Clone, Copy)]
struct Style {
    size: f32,
    /// 別の行に置く数式（∑ の添字を上下に置き、分数を大きく組む）
    display: bool,
    /// 添字の中（演算子の前後を空けない）
    script: bool,
    /// 文の中の数式（次の行に重ならないように、分数は斜線で組む）
    in_text: bool,
}

impl Style {
    fn script(self) -> Self {
        Self {
            size: (self.size * SCRIPT_SCALE).max(MIN_SIZE),
            display: false,
            script: true,
            in_text: self.in_text,
        }
    }

    fn stroke(self) -> f32 {
        (self.size / 15.0).max(1.0)
    }
}

fn has_descender(c: char) -> bool {
    "gjpqyβγζημξρφχψ,;()[]{}|‖∫".contains(c)
}

fn is_short(c: char) -> bool {
    "acegmnopqrsuvwxyzαγεηικμνοπρστυχω−=+·,.;:…∼≈→←".contains(c)
}

/// 1 行の文字の上端からベースラインまで。
fn baseline_of(galley: &Galley, size: f32) -> f32 {
    galley
        .rows
        .first()
        .and_then(|r| r.row.glyphs.first().map(|g| r.pos.y + g.pos.y))
        .unwrap_or(size * 0.8)
}

fn text(painter: &Painter, s: &str, size: f32, italic: bool) -> Laid {
    let mut job = LayoutJob::default();
    job.append(
        s,
        0.0,
        TextFormat {
            font_id: FontId::proportional(size),
            color: Color32::PLACEHOLDER,
            italics: italic,
            ..Default::default()
        },
    );
    let galley = painter.layout_job(job);
    let baseline = baseline_of(&galley, size);
    let a = if s.chars().all(is_short) { 0.56 } else { 0.76 };
    let d = if s.chars().any(has_descender) {
        0.24
    } else {
        0.04
    };
    let correction = if italic { ITALIC_CORRECTION } else { 0.0 };
    Laid {
        w: galley.size().x + correction * size,
        a: a * size,
        d: d * size,
        ops: vec![Op::Text(pos2(0.0, -baseline), galley)],
    }
}

/// 前後の空きを決めるための種類。分数などは、ふつうの項とみなす。
fn class_of(node: &MathNode) -> AtomClass {
    match node {
        MathNode::Atom(_, class) => *class,
        MathNode::BigOp { .. } => AtomClass::Function { limits: true },
        MathNode::Scripts { base, .. } => class_of(base),
        MathNode::Delimited { .. } => AtomClass::Open,
        _ => AtomClass::Upright,
    }
}

/// 並びの中で、`prev` と `next` の間に空ける幅（em）。`unary` は next の演算子が単項のとき。
fn gap(prev: AtomClass, next: AtomClass, unary: bool, script: bool) -> f32 {
    use AtomClass::{Binary, Close, Function, Italic, Open, Punct, Relation};
    match (prev, next) {
        (Function { .. }, Open) => 0.0,
        (Function { .. }, _) => 0.17,
        (_, Punct | Close) => 0.03,
        (Open, _) => 0.02,
        _ if script => 0.03,
        (Relation, _) | (_, Relation) => 0.3,
        (_, Binary) if unary => 0.03,
        (Binary, _) | (_, Binary) => 0.24,
        (Punct, _) => 0.2,
        (_, Function { .. }) => 0.17,
        (Italic, Open) => 0.07,
        _ => 0.06,
    }
}

fn row(painter: &Painter, items: &[MathNode], style: Style) -> Laid {
    use AtomClass::{Binary, Function, Open, Punct, Relation};
    let mut out = Laid::default();
    let mut prev: Option<AtomClass> = None;
    // いまの演算子が単項（左に項が無い）なら、後ろも空けない
    let mut after_unary = false;
    for (i, node) in items.iter().enumerate() {
        if let MathNode::Space(n) = node {
            out.w += f32::from(*n) / 18.0 * style.size;
            prev = None;
            continue;
        }
        let class = class_of(node);
        let unary = class == Binary
            && matches!(
                prev,
                None | Some(Binary | Relation | Open | Punct | Function { .. })
            );
        let em = match prev {
            _ if i == 0 => 0.0,
            _ if after_unary => 0.03,
            Some(prev) => gap(prev, class, unary, style.script),
            None => 0.04,
        };
        after_unary = unary;
        out.append(layout(painter, node, style), em * style.size);
        prev = Some(class);
    }
    out
}

fn big_op(painter: &Painter, symbol: &str, style: Style) -> Laid {
    let scale = if style.display { 1.4 } else { 1.12 };
    let size = style.size * scale;
    let glyph = text(painter, symbol, size, false);
    // 記号の中心を、分数の線の高さに合わせる
    let (top, bottom) = (0.76 * size, 0.14 * size);
    let shift = (top - bottom) / 2.0 - AXIS * style.size;
    let mut out = Laid {
        w: glyph.w,
        ..Laid::default()
    };
    out.put(glyph, 0.0, shift);
    out.a = top - shift;
    out.d = bottom + shift;
    out
}

fn scripts(
    painter: &Painter,
    base: &MathNode,
    sub: Option<&MathNode>,
    sup: Option<&MathNode>,
    style: Style,
) -> Laid {
    let small = style.script();
    let limits = style.display
        && matches!(
            base,
            MathNode::BigOp { limits: true, .. }
                | MathNode::Atom(_, AtomClass::Function { limits: true })
        );
    let base = layout(painter, base, style);
    let sub = sub.map(|n| layout(painter, n, small));
    let sup = sup.map(|n| layout(painter, n, small));
    let gap = style.size * 0.1;
    let mut out = Laid::default();
    if limits {
        let width = [
            base.w,
            sub.as_ref().map_or(0.0, |l| l.w),
            sup.as_ref().map_or(0.0, |l| l.w),
        ]
        .into_iter()
        .fold(0.0, f32::max);
        let (a, d, w) = (base.a, base.d, base.w);
        out.put(base, (width - w) / 2.0, 0.0);
        if let Some(sup) = sup {
            let (w, y) = (sup.w, -(a + gap + sup.d));
            out.put(sup, (width - w) / 2.0, y);
        }
        if let Some(sub) = sub {
            let (w, y) = (sub.w, d + gap + sub.a);
            out.put(sub, (width - w) / 2.0, y);
        }
        out.w = width;
        return out;
    }
    let (base_w, base_a, base_d) = (base.w, base.a, base.d);
    out.put(base, 0.0, 0.0);
    let x = base_w + style.size * 0.03;
    let sup_y = -(style.size * 0.38).max(base_a - small.size * 0.4);
    let mut sub_y = (style.size * 0.17).max(base_d + small.size * 0.08);
    // 上と下の添字が重ならないようにする
    if let (Some(sub), Some(sup)) = (&sub, &sup) {
        sub_y = sub_y.max(sup_y + sup.d + sub.a + 1.0);
    }
    let mut width: f32 = 0.0;
    if let Some(sup) = sup {
        width = width.max(sup.w);
        out.put(sup, x + style.size * 0.05, sup_y);
    }
    if let Some(sub) = sub {
        width = width.max(sub.w);
        out.put(sub, x, sub_y);
    }
    out.w = x + width + style.size * 0.03;
    out
}

/// 斜線の分数で、かっこが要る（2 つ以上のものが並ぶ）分子・分母か。
fn needs_parens(node: &MathNode) -> bool {
    matches!(node, MathNode::Row(items) if items.len() > 1) || matches!(node, MathNode::Frac(..))
}

/// 文の中の分数（1/N、(a+b)/2）。
fn slashed(painter: &Painter, num: &MathNode, den: &MathNode, style: Style) -> Laid {
    let part = |node: &MathNode| {
        let body = layout(painter, node, style);
        if !needs_parens(node) {
            return body;
        }
        let mut out = text(painter, "(", style.size, false);
        out.append(body, 0.0);
        out.append(text(painter, ")", style.size, false), 0.0);
        out
    };
    let mut out = part(num);
    out.append(text(painter, "/", style.size, false), style.size * 0.02);
    out.append(part(den), style.size * 0.02);
    out
}

fn frac(painter: &Painter, num: &MathNode, den: &MathNode, style: Style) -> Laid {
    if style.in_text {
        return slashed(painter, num, den, style);
    }
    let inner = if style.display {
        Style {
            display: false,
            ..style
        }
    } else {
        Style {
            size: (style.size * 0.78).max(MIN_SIZE),
            display: false,
            ..style
        }
    };
    let num = layout(painter, num, inner);
    let den = layout(painter, den, inner);
    let width = num.w.max(den.w) + style.size * 0.2;
    let axis = style.size * AXIS;
    let space = style.size * 0.12;
    let mut out = Laid {
        w: width + style.size * 0.1,
        ..Laid::default()
    };
    let left = style.size * 0.05;
    out.line(
        vec![pos2(left, -axis), pos2(left + width, -axis)],
        style.stroke(),
    );
    let (w, y) = (num.w, -axis - space - num.d);
    out.put(num, left + (width - w) / 2.0, y);
    let (w, y) = (den.w, -axis + space + den.a);
    out.put(den, left + (width - w) / 2.0, y);
    out.a = out.a.max(axis);
    out
}

fn sqrt(painter: &Painter, index: Option<&MathNode>, body: &MathNode, style: Style) -> Laid {
    let em = style.size;
    let body = layout(painter, body, style);
    let index = index.map(|n| layout(painter, n, style.script().script()));
    let lead = index.as_ref().map_or(0.0, |i| (i.w - em * 0.25).max(0.0));
    let surd = em * 0.6;
    let top = -(body.a + em * 0.14);
    let bottom = body.d.max(em * 0.08);
    let end = lead + surd + body.w + em * 0.1;
    let mut out = Laid {
        w: end,
        a: -top + style.stroke(),
        d: bottom,
        ops: Vec::new(),
    };
    out.line(
        vec![
            pos2(lead + em * 0.02, -em * 0.24),
            pos2(lead + em * 0.16, -em * 0.32),
            pos2(lead + em * 0.32, bottom),
            pos2(lead + surd * 0.92, top),
            pos2(end, top),
        ],
        style.stroke(),
    );
    out.put(body, lead + surd, 0.0);
    if let Some(index) = index {
        out.put(index, 0.0, -em * 0.5);
    }
    out
}

fn accent(painter: &Painter, kind: AccentKind, body: &MathNode, style: Style) -> Laid {
    let em = style.size;
    let italic = matches!(body, MathNode::Atom(_, AtomClass::Italic));
    let body = layout(painter, body, style);
    let (w, a) = (body.w, body.a);
    let mut out = Laid {
        w,
        ..Laid::default()
    };
    out.put(body, 0.0, 0.0);
    let y = -(a + em * 0.07);
    // 斜体は上の方が右へ寄る
    let cx = w / 2.0 + if italic { em * 0.07 } else { 0.0 };
    let half = (w * 0.42).clamp(em * 0.16, em * 0.8);
    let stroke = style.stroke();
    match kind {
        AccentKind::Hat => out.line(
            vec![
                pos2(cx - half.min(em * 0.28), y),
                pos2(cx, y - em * 0.16),
                pos2(cx + half.min(em * 0.28), y),
            ],
            stroke,
        ),
        AccentKind::Bar => out.line(
            vec![
                pos2(cx - half, y - em * 0.03),
                pos2(cx + half, y - em * 0.03),
            ],
            stroke,
        ),
        AccentKind::Tilde => {
            let half = half.min(em * 0.32);
            let points = (0..=8)
                .map(|i| {
                    let t = i as f32 / 8.0;
                    let wave = (t * std::f32::consts::TAU).sin();
                    pos2(cx - half + 2.0 * half * t, y - em * 0.06 - wave * em * 0.06)
                })
                .collect();
            out.line(points, stroke);
        }
        AccentKind::Vec => {
            let (left, right, mid) = (cx - half, cx + half, y - em * 0.07);
            out.line(vec![pos2(left, mid), pos2(right, mid)], stroke);
            out.line(
                vec![
                    pos2(right - em * 0.12, mid - em * 0.08),
                    pos2(right, mid),
                    pos2(right - em * 0.12, mid + em * 0.08),
                ],
                stroke,
            );
        }
        AccentKind::Dot => out.ops.push(Op::Dot(pos2(cx, y - em * 0.06), stroke * 0.9)),
        AccentKind::DoubleDot => {
            for dx in [-em * 0.11, em * 0.11] {
                out.ops
                    .push(Op::Dot(pos2(cx + dx, y - em * 0.06), stroke * 0.9));
            }
        }
    }
    out.a = out.a.max(a + em * 0.28);
    out
}

/// 高さ `top..bottom` に伸ばしたかっこを線で描く。知らない記号なら None。
fn stretched(delim: &str, top: f32, bottom: f32, style: Style) -> Option<Laid> {
    let em = style.size;
    let h = bottom - top;
    let at = |x: f32, t: f32| pos2(x, top + h * t);
    let w = match delim {
        "{" | "}" => em * 0.42,
        "|" => em * 0.26,
        "‖" => em * 0.38,
        _ => em * 0.36,
    };
    let (l, r, m) = (w * 0.2, w * 0.8, w * 0.5);
    let curve = |x_end: f32, x_mid: f32| -> Vec<Pos2> {
        (0..=16)
            .map(|i| {
                let t = i as f32 / 16.0;
                let bend = 4.0 * t * (1.0 - t);
                at(x_end + (x_mid - x_end) * bend, t)
            })
            .collect()
    };
    let brace = |tip: f32, edge: f32| {
        vec![
            at(edge, 0.0),
            at(m, 0.05),
            at(m, 0.43),
            at(tip, 0.5),
            at(m, 0.57),
            at(m, 0.95),
            at(edge, 1.0),
        ]
    };
    let lines = match delim {
        "(" => vec![curve(r, l)],
        ")" => vec![curve(l, r)],
        "[" => vec![vec![at(r, 0.0), at(l, 0.0), at(l, 1.0), at(r, 1.0)]],
        "]" => vec![vec![at(l, 0.0), at(r, 0.0), at(r, 1.0), at(l, 1.0)]],
        "{" => vec![brace(l, r)],
        "}" => vec![brace(r, l)],
        "⟨" => vec![vec![at(r, 0.0), at(l, 0.5), at(r, 1.0)]],
        "⟩" => vec![vec![at(l, 0.0), at(r, 0.5), at(l, 1.0)]],
        "|" => vec![vec![at(m, 0.0), at(m, 1.0)]],
        "‖" => vec![
            vec![at(w * 0.32, 0.0), at(w * 0.32, 1.0)],
            vec![at(w * 0.68, 0.0), at(w * 0.68, 1.0)],
        ],
        _ => return None,
    };
    Some(Laid {
        w,
        a: -top,
        d: bottom,
        ops: lines
            .into_iter()
            .map(|points| Op::Line(points, style.stroke()))
            .collect(),
    })
}

fn delimited(painter: &Painter, open: &str, body: &MathNode, close: &str, style: Style) -> Laid {
    let em = style.size;
    let body = layout(painter, body, style);
    let tall = body.a + body.d > em * 1.15;
    // 分数の線の高さを中心に、上下を同じだけ伸ばす
    let axis = em * AXIS;
    let half = (body.a - axis).max(body.d + axis) + em * 0.06;
    let side = |delim: &str| -> Option<Laid> {
        if delim.is_empty() {
            return None;
        }
        tall.then(|| stretched(delim, -axis - half, -axis + half, style))
            .flatten()
            .or_else(|| Some(text(painter, delim, em, false)))
    };
    let (open, close) = (side(open), side(close));
    let mut out = Laid::default();
    if let Some(open) = open {
        out.append(open, 0.0);
    }
    out.append(body, em * 0.04);
    if let Some(close) = close {
        out.append(close, em * 0.04);
    }
    out
}

fn table(painter: &Painter, rows: &[Vec<MathNode>], align: TableAlign, style: Style) -> Laid {
    let em = style.size;
    let cell_style = Style {
        display: false,
        ..style
    };
    let cells: Vec<Vec<Laid>> = rows
        .iter()
        .map(|r| r.iter().map(|c| layout(painter, c, cell_style)).collect())
        .collect();
    let columns = cells.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<f32> = (0..columns)
        .map(|i| {
            cells
                .iter()
                .filter_map(|r| r.get(i).map(|c| c.w))
                .fold(0.0, f32::max)
        })
        .collect();
    let column_gap = |i: usize| match align {
        // 「x &= y」の組の中は詰め、組の間は空ける
        TableAlign::Alternate if i % 2 == 1 => em * 0.24,
        TableAlign::Alternate => em * 1.2,
        TableAlign::Center | TableAlign::Left => em * 0.9,
    };
    let row_gap = em * 0.34;
    let heights: Vec<(f32, f32)> = cells
        .iter()
        .map(|r| {
            r.iter()
                .fold((em * 0.76, em * 0.2), |(a, d), c| (a.max(c.a), d.max(c.d)))
        })
        .collect();
    let total: f32 = heights.iter().map(|(a, d)| a + d).sum::<f32>()
        + row_gap * heights.len().saturating_sub(1) as f32;
    let mut top = -em * AXIS - total / 2.0;
    let mut out = Laid::default();
    for (cells, (a, d)) in cells.into_iter().zip(heights) {
        let baseline = top + a;
        let mut x = 0.0;
        for (i, cell) in cells.into_iter().enumerate() {
            if i > 0 {
                x += column_gap(i);
            }
            let free = widths[i] - cell.w;
            let offset = match align {
                TableAlign::Left => 0.0,
                TableAlign::Center => free / 2.0,
                TableAlign::Alternate if i % 2 == 0 => free,
                TableAlign::Alternate => 0.0,
            };
            out.put(cell, x + offset, baseline);
            x += widths[i];
        }
        out.w = out.w.max(x);
        top = baseline + d + row_gap;
    }
    out
}

fn layout(painter: &Painter, node: &MathNode, style: Style) -> Laid {
    match node {
        MathNode::Atom(s, class) => text(painter, s, style.size, *class == AtomClass::Italic),
        MathNode::Row(items) => row(painter, items, style),
        MathNode::Space(n) => Laid {
            w: f32::from(*n) / 18.0 * style.size,
            ..Laid::default()
        },
        MathNode::BigOp { symbol, .. } => big_op(painter, symbol, style),
        MathNode::Scripts { base, sub, sup } => {
            scripts(painter, base, sub.as_deref(), sup.as_deref(), style)
        }
        MathNode::Frac(num, den) => frac(painter, num, den, style),
        MathNode::Sqrt { index, body } => sqrt(painter, index.as_deref(), body, style),
        MathNode::Accent(kind, body) => accent(painter, *kind, body, style),
        MathNode::Delimited { open, body, close } => delimited(painter, open, body, close, style),
        MathNode::Table { rows, align } => table(painter, rows, *align, style),
    }
}

fn parsed(tex: &str) -> Rc<MathNode> {
    CACHE.with(|cache| {
        if let Some(node) = cache.borrow().get(tex) {
            return node.clone();
        }
        let node = Rc::new(parse_math(tex));
        let mut cache = cache.borrow_mut();
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
        cache.insert(tex.to_string(), node.clone());
        node
    })
}

fn paint(painter: &Painter, laid: &Laid, origin: Pos2, color: Color32) {
    let delta = origin.to_vec2();
    for op in &laid.ops {
        match op {
            Op::Text(p, galley) => painter.galley(*p + delta, galley.clone(), color),
            Op::Line(points, width) => {
                painter.add(Shape::line(
                    points.iter().map(|p| *p + delta).collect(),
                    Stroke::new(*width, color),
                ));
            }
            Op::Dot(p, r) => {
                painter.circle_filled(*p + delta, *r, color);
            }
        }
    }
}

/// 数式を 1 つ描く。`inline` なら文の中に、そうでなければ 1 行を使って中央に置く。
/// 横に収まらなければ縮め、それでも収まらなければ横にスクロールする。
pub fn math(ui: &mut egui::Ui, tex: &str, inline: bool) -> egui::Response {
    if tex.len() > MAX_TEX_BYTES {
        return ui.label(tex);
    }
    let node = parsed(tex.trim());
    let body = egui::TextStyle::Body.resolve(ui.style()).size;
    let color = ui.visuals().text_color();
    let available = ui.max_rect().width().max(40.0);
    let mut style = Style {
        size: body * SCALE,
        display: !inline,
        script: false,
        in_text: inline,
    };
    let mut laid = layout(ui.painter(), &node, style);
    if laid.w > available {
        style.size *= (available / laid.w).max(MIN_FIT);
        laid = layout(ui.painter(), &node, style);
    }
    // 文の中では、周りの文字とベースラインをそろえる。行に収まる数式は文字と同じ高さの枠にし、
    // 高い数式は上下に同じだけ広げる（折り返しの行では、高い部品ほど下に置かれるため）
    let (line_height, line_baseline) = {
        let probe = ui.painter().layout_no_wrap(
            "x".to_string(),
            FontId::proportional(body),
            Color32::PLACEHOLDER,
        );
        (probe.size().y, baseline_of(&probe, body))
    };
    let slack = style.size * LINE_OVERFLOW;
    let extra = (laid.a - line_baseline - slack)
        .max(laid.d - (line_height - line_baseline) - slack)
        .max(0.0);
    let height = if inline {
        line_height + extra * 2.0
    } else {
        laid.a + laid.d + style.size * 0.5
    };
    let place = |ui: &mut egui::Ui, width: f32| {
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let x = rect.left() + ((width - laid.w) / 2.0).max(0.0);
            let y = if inline {
                rect.center().y - line_height / 2.0 + line_baseline
            } else {
                rect.top() + style.size * 0.25 + laid.a
            };
            paint(ui.painter(), &laid, pos2(x, y), color);
        }
        response
    };
    if laid.w > available {
        egui::ScrollArea::horizontal()
            .id_salt(("math", tex))
            .max_width(available)
            .show(ui, |ui| place(ui, laid.w))
            .inner
    } else if inline {
        place(ui, laid.w)
    } else {
        let width = ui.available_width().max(laid.w);
        place(ui, width)
    }
}

/// egui_commonmark の数式の描画。
pub fn show(ui: &mut egui::Ui, tex: &str, inline: bool) {
    math(ui, tex, inline);
}
