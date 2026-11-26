//! コマンドパレットの絞り込み。

/// `query` の語（空白区切り）がすべて `haystack` に含まれるか。大文字小文字は区別しない。
/// 空の `query` はすべてに一致する。
pub fn palette_matches(query: &str, haystack: &str) -> bool {
    let haystack = haystack.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| haystack.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_words_in_any_order() {
        let item = "節へ移る III-A OmniVLA Architecture";
        assert!(palette_matches("", item));
        assert!(palette_matches("  ", item));
        assert!(palette_matches("omnivla", item));
        assert!(palette_matches("architecture 節", item));
        // 全角の空白でも区切る
        assert!(palette_matches("移る　III-A", item));
        assert!(!palette_matches("omnivla training", item));
    }
}
