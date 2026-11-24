//! 解説キャッシュのキー。

use paper_reader_shared::Mode;
use sha2::{Digest, Sha256};

/// sha256(モデル + mode + text + context + プロンプトのバージョン) の 16 進表記。
///
/// 各要素は長さを前に付けて連結するので、区切りの位置がずれて別の入力と衝突することはない
/// （例: text="ab", context="c" と text="a", context="bc"）。
pub fn cache_key(
    model: &str,
    mode: Mode,
    text: &str,
    context: &str,
    prompt_version: u32,
) -> String {
    let mut h = Sha256::new();
    for part in [
        model,
        mode.as_str(),
        text,
        context,
        &prompt_version.to_string(),
    ] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_hex() {
        let a = cache_key("m", Mode::Sentence, "We propose X.", "ctx", 1);
        assert_eq!(a, cache_key("m", Mode::Sentence, "We propose X.", "ctx", 1));
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn every_part_matters() {
        let base = cache_key("m", Mode::Sentence, "t", "c", 1);
        assert_ne!(base, cache_key("m2", Mode::Sentence, "t", "c", 1));
        assert_ne!(base, cache_key("m", Mode::Word, "t", "c", 1));
        assert_ne!(base, cache_key("m", Mode::Sentence, "t2", "c", 1));
        assert_ne!(base, cache_key("m", Mode::Sentence, "t", "c2", 1));
        assert_ne!(base, cache_key("m", Mode::Sentence, "t", "c", 2));
    }

    #[test]
    fn no_boundary_collision() {
        assert_ne!(
            cache_key("m", Mode::Word, "ab", "c", 1),
            cache_key("m", Mode::Word, "a", "bc", 1)
        );
    }
}
