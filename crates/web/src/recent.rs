//! 直近に引いた単語のメモ（英語と短い和訳、最大 10 語）。
//!
//! 思い出すための簡単なメモなので、サーバーやデータベースには送らず、ブラウザの localStorage にだけ置く。

use std::collections::VecDeque;

use paper_reader_shared::DictResponse;
use serde::{Deserialize, Serialize};

/// 残す語数
pub const MAX_RECENT_WORDS: usize = 10;
/// 1 語に残す訳の数
const MEANINGS_PER_WORD: usize = 2;
const STORAGE_KEY: &str = "paper-reader.recent-words";
/// 表示するかの設定を保存するキー
pub const SHOW_KEY: &str = "paper-reader.show-recent-words";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentWord {
    pub word: String,
    /// 短い和訳（最初の 2 つを「；」でつなぐ）
    pub meaning: String,
}

/// 新しい順に並べた直近の単語。
#[derive(Debug, Clone, Default)]
pub struct RecentWords {
    items: VecDeque<RecentWord>,
}

impl RecentWords {
    /// localStorage から読む（壊れていたら空から始める）。
    pub fn load() -> Self {
        let items = storage()
            .and_then(|s| s.get_item(STORAGE_KEY).ok().flatten())
            .and_then(|json| serde_json::from_str::<Vec<RecentWord>>(&json).ok())
            .map(|mut v| {
                v.truncate(MAX_RECENT_WORDS);
                v.into()
            })
            .unwrap_or_default();
        Self { items }
    }

    pub fn items(&self) -> impl Iterator<Item = &RecentWord> + Clone {
        self.items.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 辞書の結果を先頭に足す。同じ語があれば先頭に移して訳を新しくする。訳が無い語は足さない。
    pub fn record(&mut self, r: &DictResponse) {
        let word = r.headword.clone().unwrap_or_else(|| r.query.clone());
        let Some(meaning) = short_meaning(r) else {
            return;
        };
        let entry = RecentWord { word, meaning };
        if self.items.front() == Some(&entry) {
            return;
        }
        self.items.retain(|w| w.word != entry.word);
        self.items.push_front(entry);
        self.items.truncate(MAX_RECENT_WORDS);
        self.save();
    }

    pub fn clear(&mut self) {
        self.items.clear();
        if let Some(s) = storage() {
            let _ = s.remove_item(STORAGE_KEY);
        }
    }

    fn save(&self) {
        let Some(s) = storage() else {
            return;
        };
        let items: Vec<&RecentWord> = self.items.iter().collect();
        if let Ok(json) = serde_json::to_string(&items) {
            let _ = s.set_item(STORAGE_KEY, &json);
        }
    }
}

/// 辞書の訳（無ければ単語帳の AI 訳・自分で登録した訳）から短い和訳を作る。
fn short_meaning(r: &DictResponse) -> Option<String> {
    let meanings: &[String] = if r.headword.is_some() {
        &r.meanings
    } else {
        r.note
            .as_ref()
            .map(|n| n.meanings.as_slice())
            .unwrap_or_default()
    };
    let parts: Vec<String> = meanings
        .iter()
        .take(MEANINGS_PER_WORD)
        .map(|m| crate::app::strip_dict_notes(m))
        .filter(|m| !m.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("；"))
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}
