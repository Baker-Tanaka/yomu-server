//! 英文の解説の「スラッシュリーディング」の節を、チャンクと直訳の組に読み替える（画面で専用の表示にする）。
//!
//! 節は LLM が書く Markdown で、「 / 」で区切った英文の 1 行と、「- **チャンク**：直訳」の箇条書きからなる。
//! 想定と違う形のときは None を返し、画面は Markdown のまま出す（内容を落とさないため）。

use crate::SLASH_READING_HEADING;

/// 意味のまとまり 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashChunk {
    pub en: String,
    /// 直訳（生成中でまだ届いていなければ空）
    pub ja: String,
}

/// スラッシュリーディングの節と、その前後の Markdown。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashReading<'a> {
    /// 節の見出しより前
    pub before: &'a str,
    pub chunks: Vec<SlashChunk>,
    /// 次の見出し以降
    pub after: &'a str,
}

/// `markdown` からスラッシュリーディングの節を取り出す。
/// `streaming` なら、書きかけの最後の行は読み飛ばす。
pub fn parse_slash_reading(markdown: &str, streaming: bool) -> Option<SlashReading<'_>> {
    let mut offset = 0;
    let mut start = None;
    let mut body_start = 0;
    let mut end = markdown.len();
    for line in markdown.split_inclusive('\n') {
        let t = line.trim_end();
        match start {
            None if t == SLASH_READING_HEADING => {
                start = Some(offset);
                body_start = offset + line.len();
            }
            Some(_) if t.starts_with("## ") => {
                end = offset;
                break;
            }
            _ => {}
        }
        offset += line.len();
    }
    let start = start?;
    let body = &markdown[body_start..end];
    // 節が最後で改行で終わっていなければ、最後の行は書きかけ
    let partial_tail = streaming && end == markdown.len() && !body.ends_with('\n');
    let chunks = parse_body(body, partial_tail)?;
    Some(SlashReading {
        before: &markdown[..start],
        chunks,
        after: &markdown[end..],
    })
}

fn parse_body(body: &str, partial_tail: bool) -> Option<Vec<SlashChunk>> {
    let mut sliced: Vec<String> = Vec::new();
    let mut glosses: Vec<SlashChunk> = Vec::new();
    let mut lines = body.lines().peekable();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let partial = partial_tail && lines.peek().is_none();
        if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            match parse_item(item.trim()) {
                Some(c) => glosses.push(c),
                None if partial => {}
                None => return None,
            }
        } else if sliced.is_empty() && glosses.is_empty() {
            let parts = split_slashes(line);
            // 区切りの無い行は、書きかけでなければ想定外の文章
            if parts.len() < 2 && !partial {
                return None;
            }
            sliced = parts;
        } else if !partial {
            return None;
        }
    }
    let chunks = if sliced.is_empty() {
        glosses
    } else {
        attach(sliced, glosses)?
    };
    (!chunks.is_empty()).then_some(chunks)
}

/// 区切った英文の行のチャンクに、箇条書きの直訳を順に対応させる。対応しない直訳があれば None。
fn attach(sliced: Vec<String>, glosses: Vec<SlashChunk>) -> Option<Vec<SlashChunk>> {
    let mut chunks: Vec<SlashChunk> = sliced
        .into_iter()
        .map(|en| SlashChunk {
            en,
            ja: String::new(),
        })
        .collect();
    let mut next = 0;
    for g in glosses {
        let key = letters(&g.en);
        let i = next + chunks[next..].iter().position(|c| letters(&c.en) == key)?;
        chunks[i].ja = g.ja;
        next = i + 1;
    }
    Some(chunks)
}

/// 比べるときは英数字だけを見る（句読点や大文字・小文字の違いを無視する）。
fn letters(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 空白に挟まれた「/」と、全角の「／」で区切る（and/or のような語中の / では区切らない）。
fn split_slashes(line: &str) -> Vec<String> {
    plain(line)
        .replace('／', " / ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .split(|w| *w == "/" || *w == "//")
        .map(|words| words.join(" "))
        .filter(|s| !s.is_empty())
        .collect()
}

/// 「**チャンク**：直訳」（太字でなければ最初のコロンで分ける）。
fn parse_item(item: &str) -> Option<SlashChunk> {
    let (en, rest) = match item.strip_prefix("**") {
        Some(r) => {
            let (en, rest) = r.split_once("**")?;
            // 「**チャンク:** 直訳」の形
            (en.trim_end_matches([':', '：']), rest)
        }
        None => {
            let at = item.find('：').into_iter().chain(item.find(": ")).min()?;
            item.split_at(at)
        }
    };
    let ja = plain(rest.trim_start().trim_start_matches([':', '：', '→', '—']));
    let en = plain(en);
    let ja = ja.trim();
    (!en.trim().is_empty() && !ja.is_empty()).then(|| SlashChunk {
        en: en.trim().to_string(),
        ja: ja.to_string(),
    })
}

/// 太字とコードの記号を外す。
fn plain(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(r: &SlashReading<'_>) -> Vec<(String, String)> {
        r.chunks
            .iter()
            .map(|c| (c.en.clone(), c.ja.clone()))
            .collect()
    }

    fn own(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn parses_line_and_bullets() {
        let md = "## 和訳\n\n訳\n\n## スラッシュリーディング\nHowever, / we use A and/or B / today.\n\n- **However**: しかし\n- **we use A and/or B**：私たちは A か B を使う\n- **today:** 今日\n\n## 文構造\n- 主語: we\n";
        let r = parse_slash_reading(md, false).unwrap();
        assert_eq!(r.before, "## 和訳\n\n訳\n\n");
        assert_eq!(r.after, "## 文構造\n- 主語: we\n");
        assert_eq!(
            pairs(&r),
            own(&[
                ("However,", "しかし"),
                ("we use A and/or B", "私たちは A か B を使う"),
                ("today.", "今日"),
            ])
        );
    }

    #[test]
    fn bullets_only_or_unbolded() {
        let md = "## スラッシュリーディング\n- We propose: 私たちは提案する\n- a method：手法を\n";
        let r = parse_slash_reading(md, false).unwrap();
        assert_eq!(
            pairs(&r),
            own(&[("We propose", "私たちは提案する"), ("a method", "手法を")])
        );
        assert_eq!((r.before, r.after), ("", ""));
    }

    #[test]
    fn streaming_fills_glosses_later() {
        let head = "## スラッシュリーディング\nWe propose / a method";
        // 英文の行が書きかけでも、その分を出す
        let r = parse_slash_reading(head, true).unwrap();
        assert_eq!(pairs(&r), own(&[("We propose", ""), ("a method", "")]));
        let md = format!("{head}\n\n- **We propose**: 提案する\n- **a met");
        let r = parse_slash_reading(&md, true).unwrap();
        assert_eq!(
            pairs(&r),
            own(&[("We propose", "提案する"), ("a method", "")])
        );
        // 生成が終わっても壊れていれば Markdown のまま
        assert!(parse_slash_reading(&md, false).is_none());
        // 見出しだけ
        assert!(parse_slash_reading("## スラッシュリーディング\n", true).is_none());
    }

    #[test]
    fn unexpected_shapes_fall_back() {
        for body in [
            // 直訳が英文の行と対応しない
            "A b / c d\n- **A b**: あ\n- **x y**: い\n",
            // 順番が違う
            "A b / c d\n- **c d**: い\n- **A b**: あ\n",
            // 説明の文章が混じる
            "A b / c d\n- **A b**: あ\n補足の説明です。\n",
            "区切りのない説明だけ\n",
            // 直訳が無い箇条書き
            "- A b c\n",
        ] {
            let md = format!("## スラッシュリーディング\n{body}");
            assert!(parse_slash_reading(&md, false).is_none(), "{body}");
        }
        assert!(parse_slash_reading("## 和訳\n訳\n", false).is_none());
    }
}
