//! 本文の数式の LaTeX（MathML の `alttext` など）の扱い。

use std::borrow::Cow;

/// LLM に渡し、画面にも出せるように、LaTeX から表示だけの指定と行の継ぎ目を除く。
pub fn clean_tex(tex: &str) -> String {
    // 行末の「%」は、改行を無かったことにする書き方（LaTeXML の alttext に入る）。「\%」は残す
    let mut joined = String::with_capacity(tex.len());
    let mut chars = tex.chars().peekable();
    let mut prev = ' ';
    while let Some(c) = chars.next() {
        if c == '%' && prev != '\\' && matches!(chars.peek(), Some('\n' | '\r')) {
            while chars.next_if(|c| c.is_whitespace()).is_some() {}
        } else {
            joined.push(c);
        }
        prev = c;
    }
    let joined = joined
        .replace("\\displaystyle", " ")
        .replace("\\textstyle", " ");
    joined.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 英文の中に数式を入れるときの形。
pub fn inline_tex(tex: &str) -> String {
    format!("${tex}$")
}

/// `$…$` の中身が数式らしいか（金額の $ は数式にしない）。
fn math_like(inner: &str) -> bool {
    if inner.starts_with(char::is_whitespace) || inner.ends_with(char::is_whitespace) {
        return false;
    }
    inner.contains(['\\', '^', '_', '{', '='])
        || (inner.chars().count() <= 3
            && !inner.contains(char::is_whitespace)
            && inner.starts_with(char::is_alphabetic))
}

/// `$…$`（1 行の中）と `$$…$$`（複数行でもよい）で書かれた数式を `f` の結果に置き換える。
fn map_math(text: &str, f: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        if let Some(body) = after.strip_prefix("$$")
            && let Some(end) = body.find("$$")
            && math_like(body[..end].trim())
        {
            out.push_str(&f(body[..end].trim()));
            rest = &body[end + 2..];
            continue;
        }
        let body = &after[1..];
        match body.find(['$', '\n']) {
            Some(end) if body[end..].starts_with('$') && math_like(&body[..end]) => {
                out.push_str(&f(&body[..end]));
                rest = &body[end + 1..];
            }
            _ => {
                out.push('$');
                rest = body;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 読み上げる文字列。数式は記号の羅列になるので「formula」に置き換える。
pub fn speakable(text: &str) -> String {
    if !text.contains('$') {
        return text.to_string();
    }
    map_math(text, |_| "formula".into())
}

/// 画面に出す文字列。数式の組版はできないので、`$…$` の LaTeX を読みやすい Unicode の文字にする。
/// `markdown` なら、添字の `_` などが Markdown の記号とみなされないようにする。
pub fn readable_math(text: &str, markdown: bool) -> Cow<'_, str> {
    if !text.contains('$') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(map_math(text, |tex| {
        let plain = tex_to_text(tex);
        if !markdown {
            return plain;
        }
        plain.chars().fold(String::new(), |mut out, c| {
            if matches!(
                c,
                '\\' | '_' | '*' | '`' | '[' | ']' | '<' | '>' | '|' | '~'
            ) {
                out.push('\\');
            }
            out.push(c);
            out
        })
    }))
}

/// LaTeX の数式を、1 行の読める文字にする（`\pi_{\theta}` → `π_θ`）。
/// 知らない命令は、名前だけを残す。
pub fn tex_to_text(tex: &str) -> String {
    let chars: Vec<char> = tex.chars().collect();
    let mut pos = 0;
    let mut out = String::new();
    while pos < chars.len() {
        out.push_str(&atom(&chars, &mut pos, 0));
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 入れ子の深さの上限（壊れた入力で深く呼びすぎないように）
const MAX_DEPTH: u8 = 24;

/// `{…}` か、命令か、1 文字を読んで変換する。
fn atom(chars: &[char], pos: &mut usize, depth: u8) -> String {
    let Some(&c) = chars.get(*pos) else {
        return String::new();
    };
    *pos += 1;
    if depth >= MAX_DEPTH {
        return c.to_string();
    }
    match c {
        '{' => {
            let mut out = String::new();
            while let Some(&next) = chars.get(*pos) {
                if next == '}' {
                    *pos += 1;
                    break;
                }
                out.push_str(&atom(chars, pos, depth + 1));
            }
            out
        }
        '}' | '&' => String::new(),
        '~' => " ".into(),
        '_' | '^' => {
            let script = argument(chars, pos, depth);
            let script = script.trim();
            if script.chars().count() <= 1 {
                format!("{c}{script}")
            } else if script.chars().all(char::is_alphanumeric) {
                // 「J_ilm_obj」のように次の記号とつながらないように空ける
                let joins = chars
                    .get(*pos)
                    .is_some_and(|&n| n.is_ascii_alphabetic() || n == '\\');
                format!("{c}{script}{}", if joins { " " } else { "" })
            } else {
                format!("{c}({script})")
            }
        }
        '\\' => command(chars, pos, depth),
        c => c.to_string(),
    }
}

/// 命令や添字の引数を 1 つ読む（前の空白は飛ばす）。
fn argument(chars: &[char], pos: &mut usize, depth: u8) -> String {
    while chars.get(*pos).is_some_and(|c| c.is_whitespace()) {
        *pos += 1;
    }
    atom(chars, pos, depth + 1)
}

/// 分子・分母が長ければかっこで囲む。
fn grouped(s: String) -> String {
    let s = s.trim().to_string();
    if s.chars().all(char::is_alphanumeric) {
        s
    } else {
        format!("({s})")
    }
}

/// `\` の後ろを読んで変換する。
fn command(chars: &[char], pos: &mut usize, depth: u8) -> String {
    let start = *pos;
    while chars.get(*pos).is_some_and(char::is_ascii_alphabetic) {
        *pos += 1;
    }
    if *pos == start {
        // 記号の命令（\{ \, \\ など）
        let Some(&c) = chars.get(*pos) else {
            return String::new();
        };
        *pos += 1;
        return match c {
            ',' | ';' | ':' | ' ' => " ".into(),
            '!' => String::new(),
            '\\' => "; ".into(),
            '|' => "‖".into(),
            c => c.to_string(),
        };
    }
    let name: String = chars[start..*pos].iter().collect();
    if let Some(symbol) = symbol(&name) {
        return symbol.into();
    }
    let accent = match name.as_str() {
        "hat" | "widehat" => Some("hat"),
        "bar" | "overline" => Some("bar"),
        "tilde" | "widetilde" => Some("tilde"),
        "vec" => Some("vec"),
        "dot" => Some("dot"),
        _ => None,
    };
    if let Some(accent) = accent {
        return accented(accent, &argument(chars, pos, depth));
    }
    match name.as_str() {
        "frac" | "tfrac" | "dfrac" => {
            let num = grouped(argument(chars, pos, depth));
            let den = grouped(argument(chars, pos, depth));
            format!("{num}/{den}")
        }
        "sqrt" => format!("√{}", grouped(argument(chars, pos, depth))),
        // 書体の指定は中身だけを残す（黒板太字の ℝ などは、画面のフォントに無い）
        "mathbb" | "mathbf" | "mathrm" | "mathit" | "mathsf" | "mathtt" | "mathcal" | "mathscr"
        | "mathfrak" | "boldsymbol" | "bm" | "text" | "textbf" | "textit" | "textrm"
        | "operatorname" | "mbox" => argument(chars, pos, depth),
        // 大きさや並べ方の指定は消す。「\left.」の点も出さない
        "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl" | "Bigr" => {
            if chars.get(*pos) == Some(&'.') {
                *pos += 1;
            }
            String::new()
        }
        "displaystyle" | "textstyle" | "scriptstyle" | "limits" | "nolimits" | "nonumber" => {
            String::new()
        }
        "quad" | "qquad" => " ".into(),
        // min、log などの関数名と、知らない命令
        _ => name,
    }
}

/// 上に記号を付けた文字。1 文字で表せるもの（â など）はその文字、ほかは `hat(x)` の形。
/// 記号を重ねる結合文字は画面のフォントに無いので使わない。
fn accented(accent: &str, base: &str) -> String {
    let table: &[(char, char)] = match accent {
        "hat" => &[
            ('a', 'â'),
            ('e', 'ê'),
            ('i', 'î'),
            ('o', 'ô'),
            ('u', 'û'),
            ('y', 'ŷ'),
        ],
        "bar" => &[('a', 'ā'), ('e', 'ē'), ('i', 'ī'), ('o', 'ō'), ('u', 'ū')],
        "tilde" => &[('a', 'ã'), ('n', 'ñ'), ('o', 'õ')],
        _ => &[],
    };
    let mut chars = base.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && let Some((_, out)) = table.iter().find(|(from, _)| *from == c)
    {
        return out.to_string();
    }
    format!("{accent}({base})")
}

fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" | "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" | "vartheta" => "θ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "rho" => "ρ",
        "sigma" => "σ",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" | "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        "sum" => "∑",
        "prod" => "∏",
        "int" => "∫",
        "leq" | "le" => "≤",
        "geq" | "ge" => "≥",
        "neq" | "ne" => "≠",
        "approx" => "≈",
        "sim" => "∼",
        "equiv" => "≡",
        "propto" => "∝",
        "times" => "×",
        "cdot" => "·",
        "circ" => "∘",
        "oplus" => "⊕",
        "otimes" => "⊗",
        "odot" => "⊙",
        "pm" => "±",
        "to" | "rightarrow" => "→",
        "leftarrow" | "gets" => "←",
        "Rightarrow" => "⇒",
        "mapsto" => "↦",
        "in" => "∈",
        "notin" => "∉",
        "subset" => "⊂",
        "subseteq" => "⊆",
        "cup" => "∪",
        "cap" => "∩",
        "forall" => "∀",
        "exists" => "∃",
        "infty" => "∞",
        "partial" => "∂",
        "nabla" => "∇",
        "ldots" | "cdots" | "dots" => "…",
        "mid" | "vert" => "|",
        "Vert" | "lVert" | "rVert" => "‖",
        "langle" => "⟨",
        "rangle" => "⟩",
        "ell" => "ℓ",
        "top" => "⊤",
        "prime" => "′",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_latexml_alttext() {
        assert_eq!(clean_tex("{\\displaystyle h_{t}}"), "{ h_{t}}");
        assert_eq!(clean_tex("a=b+%\n  c"), "a=b+c");
        assert_eq!(clean_tex("  x \n y  "), "x y");
        // 百分率の \% は残す
        assert_eq!(clean_tex("50\\%\nof"), "50\\% of");
    }

    #[test]
    fn speakable_replaces_only_formulas() {
        assert_eq!(
            speakable("a sequence $\\{a_{i}\\}$ of actions"),
            "a sequence formula of actions"
        );
        assert_eq!(speakable("$x_1$ and $y^2$."), "formula and formula.");
        assert_eq!(
            speakable("it costs $5 or $10 today"),
            "it costs $5 or $10 today"
        );
        assert_eq!(speakable("a $ b"), "a $ b");
        assert_eq!(speakable("plain"), "plain");
        assert_eq!(inline_tex("x"), "$x$");
    }

    #[test]
    fn tex_becomes_readable_text() {
        assert_eq!(tex_to_text(r"\pi_{\theta}"), "π_θ");
        assert_eq!(tex_to_text(r"J_{obj}"), "J_obj");
        assert_eq!(
            tex_to_text(r"\min_{\theta}\,J:=J_{il}+m_{obj}J_{obj}"),
            "min_θ J:=J_il+m_obj J_obj"
        );
        assert_eq!(
            tex_to_text(r"\{\hat{a}_{i}\}_{i=1\ldots N}"),
            "{â_i}_(i=1… N)"
        );
        assert_eq!(
            tex_to_text(r"\frac{1}{N-1}\sum_{i=1}^{N-1}x^{2}"),
            "1/(N-1)∑_(i=1)^(N-1)x^2"
        );
        assert_eq!(
            tex_to_text(r"\left(\mathbf{x}\in\mathbb{R}\right)"),
            "(x∈R)"
        );
        assert_eq!(tex_to_text(r"\hat{\theta}+\vec{x}"), "hat(θ)+vec(x)");
        assert_eq!(tex_to_text(r"\unknowncmd x"), "unknowncmd x");
        // 壊れた入力でも止まらず、落ちない
        assert_eq!(tex_to_text(r"{a_{"), "a_");
        assert_eq!(tex_to_text("\\"), "");
        let deep = "{".repeat(5_000);
        tex_to_text(&deep);
    }

    #[test]
    fn readable_math_converts_only_formulas() {
        assert_eq!(
            readable_math(r"$J$：目的関数。$\pi_{\theta}$ を学習する", false),
            "J：目的関数。π_θ を学習する"
        );
        assert_eq!(
            readable_math(r"**$J_{il}$**：主な目的", true),
            r"**J\_il**：主な目的"
        );
        assert_eq!(
            readable_math("it costs $5 or $10", true),
            "it costs $5 or $10"
        );
        assert_eq!(readable_math("$$x^{2}$$", false), "x^2");
        assert_eq!(
            readable_math("例:\n$$\nJ = 0.03\n$$\n以上", false),
            "例:\nJ = 0.03\n以上"
        );
        assert_eq!(readable_math("$a_1$$b_2$", false), "a_1b_2");
        assert_eq!(readable_math("$$ と $x_1$", false), "$$ と x_1");
        assert!(matches!(readable_math("数式なし", true), Cow::Borrowed(_)));
    }
}
