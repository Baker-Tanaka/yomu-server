//! 参考文献の 1 件（LaTeXML の `ltx_bibitem`）の文字列から、引用先を開く・探すための手がかりを取り出す。
//!
//! 書式は論文ごとに違うので、確かなものだけを返す（分からなければ `None`）。

use crate::{is_arxiv_id, normalize_whitespace};

/// 検索語にする題名の上限（`/api/search` の入力の上限より短くする）
const MAX_TITLE_CHARS: usize = 180;
/// これより短いものは題名とみなさない
const MIN_TITLE_CHARS: usize = 8;
/// arXiv ID の直前に付く書き方（小文字）。数字の並びを取り違えないよう、これが付くものだけを拾う
const ID_PREFIXES: &[&str] = &["arxiv:", "arxiv.org/abs/", "arxiv.org/pdf/", "abs/"];

/// 参考文献の文字列（またはリンク先の URL）にある arXiv ID。
pub fn cited_arxiv_id(text: &str) -> Option<String> {
    // 長さの変わらない ASCII の小文字化なので、位置は元の文字列と同じ
    let lower = text.to_ascii_lowercase();
    ID_PREFIXES.iter().find_map(|prefix| {
        lower.match_indices(prefix).find_map(|(at, _)| {
            let rest = text[at + prefix.len()..].trim_start();
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '-')))
                .unwrap_or(rest.len());
            let id = rest[..end].trim_end_matches(['.', '/', '-']);
            let id = id.strip_suffix(".pdf").unwrap_or(id);
            is_arxiv_id(id).then(|| id.to_string())
        })
    })
}

/// 参考文献の題名。`blocks` は 1 件の中の区切り（`ltx_bibblock`）ごとの文字列。
///
/// - 引用符で囲まれた部分があれば、それ（IEEE などの書式）
/// - 区切りが 2 つ以上なら 2 つ目（著者・題名・掲載先の順に分かれる書式）
pub fn cited_title(blocks: &[String]) -> Option<String> {
    let quoted = blocks.iter().find_map(|b| {
        [('“', '”'), ('"', '"')]
            .into_iter()
            .find_map(|(open, close)| {
                let (_, rest) = b.split_once(open)?;
                let (title, _) = rest.split_once(close)?;
                clean_title(title)
            })
    });
    quoted.or_else(|| {
        let second = normalize_whitespace(blocks.get(1)?);
        // 掲載先（"In Proceedings of …"）は題名ではない
        if second.starts_with("In ") || second.to_ascii_lowercase().starts_with("arxiv") {
            return None;
        }
        clean_title(&second)
    })
}

/// 前後の句読点を落とし、長すぎれば語の切れ目で切る。
fn clean_title(s: &str) -> Option<String> {
    let s = normalize_whitespace(s);
    let s = s.trim_matches(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | ';'));
    if s.chars().count() < MIN_TITLE_CHARS {
        return None;
    }
    if s.chars().count() <= MAX_TITLE_CHARS {
        return Some(s.to_string());
    }
    let cut: String = s.chars().take(MAX_TITLE_CHARS).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    Some(cut.to_string())
}

/// 題名を `/api/search` の検索語にする。`:` があると欄の指定とみなされ、題名の語句検索にならないので除く。
pub fn title_search_query(title: &str) -> String {
    normalize_whitespace(&title.replace([':', '"', '“', '”'], " "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_arxiv_ids() {
        for (text, id) in [
            ("arXiv preprint arXiv:2406.09246, 2024.", "2406.09246"),
            ("arXiv:2307.15818v2.", "2307.15818v2"),
            ("CoRR, abs/1810.04805, 2018.", "1810.04805"),
            ("https://arxiv.org/abs/hep-th/9901001", "hep-th/9901001"),
            ("https://arxiv.org/pdf/2301.12345.pdf", "2301.12345"),
            ("arXiv: 1706.03762 [cs.CL]", "1706.03762"),
        ] {
            assert_eq!(cited_arxiv_id(text).as_deref(), Some(id), "{text}");
        }
    }

    #[test]
    fn ignores_other_numbers() {
        for text in [
            "In CVPR, 2024, pp. 26689–26699.",
            "doi:10.1109/5.771073",
            "arXiv preprint, 2024.",
            "https://example.com/abs/summary",
        ] {
            assert_eq!(cited_arxiv_id(text), None, "{text}");
        }
    }

    fn blocks(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn quoted_title() {
        let b = blocks(&[
            "J. Lin and S. Han, “Vila: On pre-training for visual language models,” in Proc. CVPR, 2024.",
        ]);
        assert_eq!(
            cited_title(&b).as_deref(),
            Some("Vila: On pre-training for visual language models")
        );
    }

    #[test]
    fn title_in_second_block() {
        let b = blocks(&[
            "Jimmy Lei Ba, Jamie Ryan Kiros, and Geoffrey E Hinton.",
            "Layer normalization.",
            "arXiv preprint arXiv:1607.06450, 2016.",
        ]);
        assert_eq!(cited_title(&b).as_deref(), Some("Layer normalization"));
    }

    #[test]
    fn unknown_title() {
        assert_eq!(cited_title(&blocks(&["A. Author. Something. 2020."])), None);
        assert_eq!(
            cited_title(&blocks(&["A. Author.", "In Proceedings of X, 2020."])),
            None
        );
        assert_eq!(cited_title(&[]), None);
    }

    #[test]
    fn long_title_is_cut_at_a_word() {
        let long = format!("\"{}\"", "word ".repeat(60));
        let title = cited_title(&[long]).unwrap();
        assert!(title.chars().count() <= MAX_TITLE_CHARS);
        assert!(title.ends_with("word"));
    }

    #[test]
    fn search_query_has_no_field_syntax() {
        assert_eq!(
            title_search_query("Vila: On pre-training"),
            "Vila On pre-training"
        );
    }
}
