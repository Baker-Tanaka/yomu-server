//! 本文の長押しで選ぶ単語の範囲（web の `touch_select.rs` で使う）。
//! 位置はブラウザの文字列と同じ UTF-16 の単位で数える。

/// 単語を作る文字か。
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// 前後が単語の文字なら、単語の一部とみなす文字（state-of-the-art、don't、i.i.d の中の点など）。
fn joins_word(c: char) -> bool {
    matches!(c, '-' | '‐' | '‑' | '\'' | '’' | '_' | '.')
}

/// `index`（UTF-16）の位置の文字を含む単語の範囲 `[start, end)`。
/// 空白なら None。記号なら、その 1 文字だけ。
pub fn word_at(text: &str, index: u32) -> Option<(u32, u32)> {
    // (UTF-16 の位置, 文字)
    let mut offset = 0;
    let chars: Vec<(u32, char)> = text
        .chars()
        .map(|c| {
            let at = offset;
            offset += c.len_utf16() as u32;
            (at, c)
        })
        .collect();
    let end_of = |i: usize| chars[i].0 + chars[i].1.len_utf16() as u32;
    let hit = chars
        .iter()
        .enumerate()
        .position(|(i, &(at, _))| at <= index && index < end_of(i))?;
    if chars[hit].1.is_whitespace() {
        return None;
    }
    let part = |i: usize| {
        let c = chars[i].1;
        is_word_char(c)
            || (joins_word(c)
                && i > 0
                && is_word_char(chars[i - 1].1)
                && chars.get(i + 1).is_some_and(|n| is_word_char(n.1)))
    };
    if !part(hit) {
        return Some((chars[hit].0, end_of(hit)));
    }
    let mut first = hit;
    while first > 0 && part(first - 1) {
        first -= 1;
    }
    let mut last = hit;
    while last + 1 < chars.len() && part(last + 1) {
        last += 1;
    }
    Some((chars[first].0, end_of(last)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, index: u32) -> Option<String> {
        let (s, e) = word_at(text, index)?;
        let units: Vec<u16> = text.encode_utf16().collect();
        Some(String::from_utf16_lossy(&units[s as usize..e as usize]))
    }

    #[test]
    fn picks_whole_word_from_any_position() {
        let text = "We train a policy.";
        assert_eq!(word(text, 3).as_deref(), Some("train"));
        assert_eq!(word(text, 7).as_deref(), Some("train"));
        // 文末の点は単語に含めない
        assert_eq!(word(text, 16).as_deref(), Some("policy"));
        assert_eq!(word(text, 17).as_deref(), Some("."));
    }

    #[test]
    fn keeps_inner_hyphens_apostrophes_and_dots() {
        assert_eq!(
            word("a state-of-the-art model", 8).as_deref(),
            Some("state-of-the-art")
        );
        assert_eq!(word("we don’t know", 4).as_deref(), Some("don’t"));
        assert_eq!(word("i.i.d. samples", 0).as_deref(), Some("i.i.d"));
        assert_eq!(word("(pre- and post-)", 2).as_deref(), Some("pre"));
        assert_eq!(word("'quoted'", 3).as_deref(), Some("quoted"));
    }

    #[test]
    fn whitespace_and_out_of_range_are_none() {
        assert_eq!(word_at("a b", 1), None);
        assert_eq!(word_at("abc", 3), None);
        assert_eq!(word_at("", 0), None);
    }

    #[test]
    fn offsets_are_utf16_units() {
        // 数式用の文字（𝑥）は UTF-16 で 2 単位
        let text = "let 𝑥 be small";
        assert_eq!(word_at(text, 4), Some((4, 6)));
        assert_eq!(word_at(text, 5), Some((4, 6)));
        assert_eq!(word(text, 8).as_deref(), Some("be"));
    }
}
