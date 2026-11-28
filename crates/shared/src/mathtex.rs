//! 解説に出る LaTeX の数式を、組版しやすい木にする（描くのは web の `math_view.rs`）。
//!
//! 論文の本文と LLM の解説によく出る範囲だけを扱う。知らない命令は名前を立体で出し、
//! 壊れた入力でも落ちずに、読める形にする。

use crate::formula::symbol;

/// 1 つの文字のかたまりの種類（前後の空きと、字体を決める）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomClass {
    /// 変数（斜体）
    Italic,
    /// 数字・記号・本文の語（立体）
    Upright,
    /// 2 項演算子（+ − × など）
    Binary,
    /// 関係子（= ≤ → など）
    Relation,
    /// 区切りの点（, ;）
    Punct,
    /// 開きかっこ
    Open,
    /// 閉じかっこ
    Close,
    /// 関数名（log、min など）。`limits` なら、別の行の数式では添字を真下・真上に置く
    Function { limits: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccentKind {
    Hat,
    Bar,
    Tilde,
    Vec,
    Dot,
    DoubleDot,
}

/// 表（行列・場合分けなど）の列の寄せ方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAlign {
    Center,
    /// すべて左（cases）
    Left,
    /// 右・左を交互に（aligned）
    Alternate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MathNode {
    Atom(String, AtomClass),
    Row(Vec<MathNode>),
    /// 空き（1/18 em 単位）
    Space(i8),
    /// ∑ や ∫ などの大きい演算子
    BigOp {
        symbol: &'static str,
        /// 別の行の数式では、添字を真下・真上に置く
        limits: bool,
    },
    Scripts {
        base: Box<MathNode>,
        sub: Option<Box<MathNode>>,
        sup: Option<Box<MathNode>>,
    },
    Frac(Box<MathNode>, Box<MathNode>),
    Sqrt {
        index: Option<Box<MathNode>>,
        body: Box<MathNode>,
    },
    Accent(AccentKind, Box<MathNode>),
    /// 中身の高さに合わせて伸びるかっこ（無い側は空の文字列）
    Delimited {
        open: String,
        body: Box<MathNode>,
        close: String,
    },
    Table {
        rows: Vec<Vec<MathNode>>,
        align: TableAlign,
    },
}

/// 入れ子の深さの上限
const MAX_DEPTH: u8 = 24;

/// 解析を止める目印。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    End,
    /// `}`
    Brace,
    /// `]`（\sqrt の指数）
    Bracket,
    /// `\right`
    Right,
    /// `&`
    Cell,
    /// `\\`
    NewRow,
    /// `\end{…}`
    EndEnv,
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

/// LaTeX の数式を解析する。
pub fn parse_math(tex: &str) -> MathNode {
    let mut p = Parser {
        chars: tex.chars().collect(),
        pos: 0,
    };
    // いちばん外でも「\\」があれば行を分ける
    let mut rows = vec![Vec::new()];
    let mut cells = Vec::new();
    loop {
        let (node, stop) = p.row(0, false);
        cells.push(node);
        match stop {
            Stop::End => break,
            Stop::Cell => {}
            Stop::NewRow => {
                if let Some(row) = rows.last_mut() {
                    row.append(&mut cells);
                }
                rows.push(Vec::new());
            }
            // 対応の無い閉じは飛ばす
            _ => {}
        }
    }
    if let Some(row) = rows.last_mut() {
        row.append(&mut cells);
    }
    rows.retain(|r| r.iter().any(|c| !is_empty(c)));
    if rows.len() > 1 {
        MathNode::Table {
            rows,
            align: TableAlign::Alternate,
        }
    } else {
        collapse(rows.pop().unwrap_or_default())
    }
}

fn is_empty(node: &MathNode) -> bool {
    matches!(node, MathNode::Row(items) if items.is_empty())
}

fn collapse(mut items: Vec<MathNode>) -> MathNode {
    if items.len() == 1 {
        items.remove(0)
    } else {
        MathNode::Row(items)
    }
}

fn atom(text: impl Into<String>, class: AtomClass) -> MathNode {
    MathNode::Atom(text.into(), class)
}

/// 1 文字の記号の種類。
fn class_of(c: char) -> AtomClass {
    match c {
        '+' | '−' | '±' | '×' | '·' | '∘' | '⊕' | '⊗' | '⊙' | '∪' | '∩' | '*' => {
            AtomClass::Binary
        }
        '=' | '<' | '>' | '≤' | '≥' | '≠' | '≈' | '∼' | '≡' | '∝' | '→' | '←' | '⇒' | '↦' | '∈'
        | '∉' | '⊂' | '⊆' | ':' => AtomClass::Relation,
        ',' | ';' => AtomClass::Punct,
        '(' | '[' | '⟨' => AtomClass::Open,
        ')' | ']' | '⟩' => AtomClass::Close,
        // ギリシャ文字は、斜体にすると小さい字で π と п のように紛れるので立体で出す
        c if c.is_ascii_alphabetic() => AtomClass::Italic,
        _ => AtomClass::Upright,
    }
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn skip_spaces(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    /// 英字の命令名（`\` の後ろ）。記号の命令なら 1 文字。
    fn command_name(&mut self) -> String {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        if self.pos == start {
            return self.bump().map(String::from).unwrap_or_default();
        }
        self.chars[start..self.pos].iter().collect()
    }

    /// `{…}` の中身をそのままの文字列で読む（本文の語や環境名）。
    fn raw_group(&mut self) -> String {
        self.skip_spaces();
        if self.peek() != Some('{') {
            return self.bump().map(String::from).unwrap_or_default();
        }
        self.pos += 1;
        let mut depth = 0;
        let mut out = String::new();
        while let Some(c) = self.bump() {
            match c {
                '{' => depth += 1,
                '}' if depth == 0 => break,
                '}' => depth -= 1,
                '\\' => {
                    let name = self.command_name();
                    match symbol(&name) {
                        Some(s) => out.push_str(s),
                        None => out.push_str(&name),
                    }
                }
                c => out.push(c),
            }
        }
        out
    }

    /// 目印まで読んで、並びにする。
    fn row(&mut self, depth: u8, in_bracket: bool) -> (MathNode, Stop) {
        let mut items: Vec<MathNode> = Vec::new();
        let stop = loop {
            let Some(c) = self.peek() else {
                break Stop::End;
            };
            match c {
                '}' => {
                    self.pos += 1;
                    break Stop::Brace;
                }
                ']' if in_bracket => {
                    self.pos += 1;
                    break Stop::Bracket;
                }
                '&' => {
                    self.pos += 1;
                    break Stop::Cell;
                }
                '^' | '_' => {
                    self.pos += 1;
                    let script = self.argument(depth);
                    attach(&mut items, script, c == '^');
                }
                '\'' => {
                    self.pos += 1;
                    attach(&mut items, atom("′", AtomClass::Upright), true);
                }
                '\\' => {
                    let at = self.pos;
                    self.pos += 1;
                    let name = self.command_name();
                    match name.as_str() {
                        "\\" => break Stop::NewRow,
                        "right" => break Stop::Right,
                        "end" => {
                            self.raw_group();
                            break Stop::EndEnv;
                        }
                        _ => {
                            self.pos = at + 1;
                            if let Some(node) = self.command(depth) {
                                items.push(node);
                            }
                        }
                    }
                }
                _ => {
                    if let Some(node) = self.single(depth) {
                        push_merged(&mut items, node);
                    }
                }
            }
        };
        (collapse(items), stop)
    }

    /// 命令でない 1 つ分（`{…}` か 1 文字）。
    fn single(&mut self, depth: u8) -> Option<MathNode> {
        let c = self.bump()?;
        match c {
            '{' if depth < MAX_DEPTH => Some(self.group(depth + 1)),
            '{' => None,
            '~' => Some(MathNode::Space(6)),
            '-' => Some(atom("−", AtomClass::Binary)),
            c if c.is_whitespace() => None,
            c => Some(atom(c, class_of(c))),
        }
    }

    /// `{` の後ろから、対応する `}` まで。
    fn group(&mut self, depth: u8) -> MathNode {
        let mut cells = Vec::new();
        loop {
            let (node, stop) = self.row(depth, false);
            cells.push(node);
            if matches!(stop, Stop::Brace | Stop::End) {
                break;
            }
        }
        match collapse(cells) {
            // かっこの中の演算子は、外との間を空けない
            MathNode::Atom(t, AtomClass::Binary | AtomClass::Relation) => {
                atom(t, AtomClass::Upright)
            }
            node => node,
        }
    }

    /// 命令や添字の引数を 1 つ。数字は 1 桁だけ読む（TeX と同じ）。
    fn argument(&mut self, depth: u8) -> MathNode {
        self.skip_spaces();
        let node = match self.peek() {
            Some('\\') => {
                self.pos += 1;
                if depth < MAX_DEPTH {
                    self.command(depth + 1)
                } else {
                    None
                }
            }
            Some('}') | None => None,
            _ => self.single(depth),
        };
        node.unwrap_or_else(|| MathNode::Row(Vec::new()))
    }

    /// `\left` や `\right` の後ろのかっこ。`.` は何も出さない。
    fn delimiter(&mut self) -> String {
        self.skip_spaces();
        match self.bump() {
            Some('.') | None => String::new(),
            Some('\\') => {
                let name = self.command_name();
                match name.as_str() {
                    "{" => "{".into(),
                    "}" => "}".into(),
                    "|" => "‖".into(),
                    _ => symbol(&name).unwrap_or_default().into(),
                }
            }
            Some(c) => c.into(),
        }
    }

    fn environment(&mut self, depth: u8) -> MathNode {
        let name = self.raw_group();
        let name = name.trim_end_matches('*');
        // array は列の指定を読み飛ばす
        if name == "array" {
            self.raw_group();
        }
        let mut rows = vec![Vec::new()];
        loop {
            let (cell, stop) = self.row(depth, false);
            if let Some(row) = rows.last_mut() {
                row.push(cell);
            }
            match stop {
                Stop::Cell | Stop::Brace | Stop::Right | Stop::Bracket => {}
                Stop::NewRow => rows.push(Vec::new()),
                Stop::EndEnv | Stop::End => break,
            }
        }
        rows.retain(|r| r.iter().any(|c| !is_empty(c)));
        let (open, close, align) = match name {
            "pmatrix" => ("(", ")", TableAlign::Center),
            "bmatrix" => ("[", "]", TableAlign::Center),
            "Bmatrix" => ("{", "}", TableAlign::Center),
            "vmatrix" => ("|", "|", TableAlign::Center),
            "Vmatrix" => ("‖", "‖", TableAlign::Center),
            "cases" => ("{", "", TableAlign::Left),
            "aligned" | "align" | "split" => ("", "", TableAlign::Alternate),
            _ => ("", "", TableAlign::Center),
        };
        let table = MathNode::Table { rows, align };
        if open.is_empty() && close.is_empty() {
            table
        } else {
            MathNode::Delimited {
                open: open.into(),
                body: Box::new(table),
                close: close.into(),
            }
        }
    }

    /// `\` の後ろを読む。何も出さない命令は None。
    fn command(&mut self, depth: u8) -> Option<MathNode> {
        let name = self.command_name();
        if depth >= MAX_DEPTH {
            return Some(atom(name, AtomClass::Upright));
        }
        let deeper = depth + 1;
        let accent =
            |p: &mut Self, kind| Some(MathNode::Accent(kind, Box::new(p.argument(deeper))));
        let big = |symbol, limits| Some(MathNode::BigOp { symbol, limits });
        let function = |name: &str, limits| Some(atom(name, AtomClass::Function { limits }));
        match name.as_str() {
            "frac" | "tfrac" | "dfrac" | "cfrac" => {
                let num = self.argument(deeper);
                let den = self.argument(deeper);
                Some(MathNode::Frac(Box::new(num), Box::new(den)))
            }
            "binom" => {
                let top = self.argument(deeper);
                let bottom = self.argument(deeper);
                Some(MathNode::Delimited {
                    open: "(".into(),
                    body: Box::new(MathNode::Table {
                        rows: vec![vec![top], vec![bottom]],
                        align: TableAlign::Center,
                    }),
                    close: ")".into(),
                })
            }
            "sqrt" => {
                self.skip_spaces();
                let index = (self.peek() == Some('[')).then(|| {
                    self.pos += 1;
                    Box::new(self.row(deeper, true).0)
                });
                Some(MathNode::Sqrt {
                    index,
                    body: Box::new(self.argument(deeper)),
                })
            }
            "hat" | "widehat" => accent(self, AccentKind::Hat),
            "bar" | "overline" => accent(self, AccentKind::Bar),
            "tilde" | "widetilde" => accent(self, AccentKind::Tilde),
            "vec" | "overrightarrow" => accent(self, AccentKind::Vec),
            "dot" => accent(self, AccentKind::Dot),
            "ddot" => accent(self, AccentKind::DoubleDot),
            "sum" => big("∑", true),
            "prod" => big("∏", true),
            "int" => big("∫", false),
            "min" | "max" | "argmin" | "argmax" | "lim" | "sup" | "inf" => function(&name, true),
            "log" | "ln" | "exp" | "sin" | "cos" | "tan" | "tanh" | "det" | "dim" | "arg"
            | "deg" | "ker" | "Pr" => function(&name, false),
            "operatorname" => {
                if self.peek() == Some('*') {
                    self.pos += 1;
                    let text = self.raw_group();
                    function(&text, true)
                } else {
                    let text = self.raw_group();
                    function(&text, false)
                }
            }
            "text" | "textrm" | "textbf" | "textit" | "texttt" | "mbox" | "mathrm" | "mathbf"
            | "mathsf" | "mathtt" => Some(atom(self.raw_group(), AtomClass::Upright)),
            // 書体の区別は付けられないので、中身をそのまま組む
            "mathit" | "mathcal" | "mathscr" | "mathfrak" | "mathbb" | "boldsymbol" | "bm"
            | "pmb" => Some(self.argument(deeper)),
            "left" => {
                let open = self.delimiter();
                let mut parts = Vec::new();
                let close = loop {
                    let (node, stop) = self.row(deeper, false);
                    parts.push(node);
                    match stop {
                        Stop::Right => break self.delimiter(),
                        Stop::End | Stop::EndEnv => break String::new(),
                        _ => {}
                    }
                };
                Some(MathNode::Delimited {
                    open,
                    body: Box::new(collapse(parts)),
                    close,
                })
            }
            "begin" => Some(self.environment(deeper)),
            "label" | "tag" | "vspace" | "hspace" => {
                self.raw_group();
                None
            }
            "displaystyle" | "textstyle" | "scriptstyle" | "limits" | "nolimits" | "nonumber"
            | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl" | "Bigr" | "biggl"
            | "biggr" | "middle" | "!" => None,
            "," => Some(MathNode::Space(3)),
            ":" | ">" => Some(MathNode::Space(4)),
            ";" => Some(MathNode::Space(5)),
            " " => Some(MathNode::Space(6)),
            "quad" => Some(MathNode::Space(18)),
            "qquad" => Some(MathNode::Space(36)),
            "{" => Some(atom("{", AtomClass::Open)),
            "}" => Some(atom("}", AtomClass::Close)),
            "|" => Some(atom("‖", AtomClass::Upright)),
            "%" | "$" | "#" | "&" | "_" => Some(atom(name, AtomClass::Upright)),
            _ => match symbol(&name) {
                Some(s) => {
                    let class = s.chars().next().map_or(AtomClass::Upright, class_of);
                    Some(atom(s, class))
                }
                None => Some(atom(name, AtomClass::Upright)),
            },
        }
    }
}

/// 続く英字は 1 つの斜体の語に、続く数字は 1 つの数にまとめる。
fn push_merged(items: &mut Vec<MathNode>, node: MathNode) {
    if let (Some(MathNode::Atom(last, lc)), MathNode::Atom(text, class)) = (items.last_mut(), &node)
    {
        let letters = *lc == AtomClass::Italic
            && *class == AtomClass::Italic
            && last.chars().all(|c| c.is_ascii_alphabetic())
            && text.chars().all(|c| c.is_ascii_alphabetic());
        let number = *lc == AtomClass::Upright
            && *class == AtomClass::Upright
            && last.chars().all(|c| c.is_ascii_digit() || c == '.')
            && text.chars().all(|c| c.is_ascii_digit() || c == '.');
        if letters || number {
            last.push_str(text);
            return;
        }
    }
    items.push(node);
}

/// 添字を直前のものに付ける。
fn attach(items: &mut Vec<MathNode>, script: MathNode, is_sup: bool) {
    let script = Some(Box::new(script));
    let base = match items.pop() {
        Some(MathNode::Scripts {
            base,
            mut sub,
            mut sup,
        }) if (is_sup && sup.is_none()) || (!is_sup && sub.is_none()) => {
            if is_sup {
                sup = script;
            } else {
                sub = script;
            }
            items.push(MathNode::Scripts { base, sub, sup });
            return;
        }
        Some(base) => base,
        None => MathNode::Row(Vec::new()),
    };
    let (sub, sup) = if is_sup {
        (None, script)
    } else {
        (script, None)
    };
    items.push(MathNode::Scripts {
        base: Box::new(base),
        sub,
        sup,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 木を短い文字列にする。
    fn show(node: &MathNode) -> String {
        let list = |v: &[MathNode]| v.iter().map(show).collect::<Vec<_>>().join(" ");
        match node {
            MathNode::Atom(t, _) => t.clone(),
            MathNode::Row(v) => format!("[{}]", list(v)),
            MathNode::Space(_) => "␣".into(),
            MathNode::BigOp { symbol, .. } => symbol.to_string(),
            MathNode::Scripts { base, sub, sup } => {
                let mut s = show(base);
                if let Some(sub) = sub {
                    s.push_str(&format!("_{{{}}}", show(sub)));
                }
                if let Some(sup) = sup {
                    s.push_str(&format!("^{{{}}}", show(sup)));
                }
                s
            }
            MathNode::Frac(a, b) => format!("frac({}, {})", show(a), show(b)),
            MathNode::Sqrt { index, body } => format!(
                "sqrt({}{})",
                index
                    .as_ref()
                    .map(|i| format!("{}; ", show(i)))
                    .unwrap_or_default(),
                show(body)
            ),
            MathNode::Accent(k, b) => format!("{k:?}({})", show(b)),
            MathNode::Delimited { open, body, close } => {
                format!("{open}<{}>{close}", show(body))
            }
            MathNode::Table { rows, .. } => format!(
                "table({})",
                rows.iter()
                    .map(|r| r.iter().map(show).collect::<Vec<_>>().join(" & "))
                    .collect::<Vec<_>>()
                    .join(" // ")
            ),
        }
    }

    fn p(tex: &str) -> String {
        show(&parse_math(tex))
    }

    #[test]
    fn parses_the_reported_objective() {
        assert_eq!(
            p(r"J_{il}=\frac{1}{N}\sum_{i=1}^{N}(a^{ref}_{i}-\hat{a}_{i})^{2}"),
            "[J_{il} = frac(1, N) ∑_{[i = 1]}^{N} ( a_{i}^{ref} − Hat(a)_{i} )^{2}]"
        );
    }

    #[test]
    fn scripts_take_one_token_without_braces() {
        assert_eq!(p("x_12"), "[x_{1} 2]");
        assert_eq!(p(r"\pi_\theta"), "π_{θ}");
        assert_eq!(p("x^2'"), "x^{2}^{′}");
        assert_eq!(p("_a"), "[]_{a}");
    }

    #[test]
    fn delimiters_roots_and_functions() {
        assert_eq!(p(r"\left( x \right)^2"), "(<x>)^{2}");
        assert_eq!(p(r"\left. x \right|"), "<x>|");
        assert_eq!(p(r"\sqrt[3]{x+1}"), "sqrt(3; [x + 1])");
        assert_eq!(p(r"\min_{\theta}\,J"), "[min_{θ} ␣ J]");
        assert!(matches!(
            parse_math(r"\argmin"),
            MathNode::Atom(_, AtomClass::Function { limits: true })
        ));
        assert_eq!(p(r"\text{if } x"), "[if  x]");
    }

    #[test]
    fn environments_and_lines_become_tables() {
        assert_eq!(
            p(r"f(x)=\begin{cases}1 & x>0 \\ 0 & \text{otherwise}\end{cases}"),
            "[f ( x ) = {<table(1 & [x > 0] // 0 & otherwise)>]"
        );
        assert_eq!(p(r"a &= b \\ c &= d"), "table(a & [= b] // c & [= d])");
        assert_eq!(p(r"\begin{pmatrix}1\\2\end{pmatrix}"), "(<table(1 // 2)>)");
    }

    #[test]
    fn broken_input_does_not_panic() {
        for tex in [
            "",
            "{",
            "}",
            "}}a{{",
            r"\",
            r"\frac",
            r"\frac{1}",
            r"\left(",
            r"\right)",
            r"\begin{cases}",
            r"\end{cases}",
            r"\sqrt[",
            "a^",
            "&&",
            r"\\",
        ] {
            parse_math(tex);
        }
        parse_math(&"{".repeat(5_000));
        parse_math(&r"\frac".repeat(2_000));
        parse_math(&r"\left(".repeat(2_000));
        parse_math(&r"\begin{matrix}".repeat(2_000));
        parse_math(&"x^".repeat(2_000));
    }
}
