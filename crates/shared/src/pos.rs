//! 辞書の訳から品詞を推定する。
//!
//! EJDict-hand には品詞の記号が無いので、訳語の形（「…を〜する」なら動詞、「〜な」「〜の」なら形容詞、
//! 〈C〉〈U〉なら名詞など）と英単語の語尾（-tion, -ous, -ly など）から推定する。
//! 辞書全体の無作為抽出で 9 割強が合う程度の精度なので、UI では「推定」と明記する。
//! 文字列の処理だけで済むので、表示のたびに呼んでも十分速い。

/// 品詞。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartOfSpeech {
    Noun,
    Verb,
    Adjective,
    Adverb,
    Preposition,
    Conjunction,
}

impl PartOfSpeech {
    /// 学校文法の呼び方。
    pub fn label(self) -> &'static str {
        match self {
            Self::Noun => "名詞",
            Self::Verb => "動詞",
            Self::Adjective => "形容詞",
            Self::Adverb => "副詞",
            Self::Preposition => "前置詞",
            Self::Conjunction => "接続詞",
        }
    }

    /// 1 文字の略号（辞書の記法）。
    pub fn short(self) -> &'static str {
        match self {
            Self::Noun => "名",
            Self::Verb => "動",
            Self::Adjective => "形",
            Self::Adverb => "副",
            Self::Preposition => "前",
            Self::Conjunction => "接",
        }
    }
}

/// 1 文字＋「い」「く」で形容詞（とその副詞形）になる漢字（「笑い」「書く」などと見分ける）。
const ADJ_KANJI: &str = "速早軽重固高低深浅広狭強弱長短遅近遠多古良悪暗熱厚薄鋭鈍安堅硬太細丸白黒赤青快甘辛痛若清温寒暑緩濃淡荒粗醜偉憎怖酸渋苦惜狡賢眠痒";

/// 前置詞の用法を示す注記（《場所》《時間》など）。
const PREP_NOTES: [&str; 10] = [
    "場所", "時間", "程度", "方向", "手段", "原因", "目的", "範囲", "関連", "所有",
];

/// 1 つの訳（EJDict の " / " で区切った 1 項目）の品詞を推定する。`headword` は英語の見出し語。
pub fn guess_pos(headword: &str, sense: &str) -> Option<PartOfSpeech> {
    use PartOfSpeech::*;
    let sense = sense.trim();
    if sense.starts_with("〈C〉") || sense.starts_with("〈U〉") {
        return Some(Noun);
    }
    if sense.contains("《補語にのみ用いて》") || sense.contains("《名詞の前にのみ用いて》")
    {
        return Some(Adjective);
    }
    let s = remove_between(sense, '[', ']');
    let prep_note = s
        .strip_prefix('《')
        .is_some_and(|rest| PREP_NOTES.iter().any(|n| rest.starts_with(n)));
    let s = remove_between(&s, '《', '》').replace(['『', '』'], "");
    let s = s.trim();
    let verb_mark = s.contains("(…を)") || s.contains("が〉");
    let s = remove_between(&remove_between(s, '(', ')'), '（', '）');
    let s = blank_between(&s, '〈', '〉');

    let mut votes = Votes::default();
    let alts = s
        .split([',', ';', '，', '；', '・', '、'])
        .map(str::trim)
        .filter(|a| !a.is_empty());
    for (i, alt) in alts.enumerate() {
        let mut pos = alt_pos(alt);
        if alt.starts_with('…') {
            if is_conjunction_ending(alt) {
                pos = Some(Conjunction);
            } else if pos == Some(Adverb) {
                pos = Some(Preposition);
            }
        }
        if let Some(pos) = pos {
            let weight = if i == 0 { 1.5 } else { 1.0 };
            votes.add(pos, if pos == Noun { weight * 0.8 } else { weight });
        }
    }
    if prep_note {
        votes.add(Preposition, 3.0);
    }
    let has_object = s.contains("…を") || s.contains("〈〉を");
    if verb_mark || (has_object && votes.get(Preposition) == 0.0) {
        votes.add(Verb, 2.0);
    }
    if let Some((pos, weight)) = suffix_prior(headword) {
        votes.add(pos, weight);
    }
    votes.best()
}

/// 訳の並びを、品詞ごとのまとまりに分ける（EJDict は品詞の順に訳を並べている）。
/// 推定できない訳は直前のまとまりに含める。戻り値は（品詞, 訳の範囲）。
pub fn pos_groups(
    headword: &str,
    meanings: &[String],
) -> Vec<(Option<PartOfSpeech>, std::ops::Range<usize>)> {
    let mut groups: Vec<(Option<PartOfSpeech>, std::ops::Range<usize>)> = Vec::new();
    for (i, m) in meanings.iter().enumerate() {
        let pos = guess_pos(headword, m);
        match groups.last_mut() {
            Some((prev, range)) if pos.is_none() || *prev == pos => range.end = i + 1,
            Some((prev @ None, range)) => {
                *prev = pos;
                range.end = i + 1;
            }
            _ => groups.push((pos, i..i + 1)),
        }
    }
    groups
}

/// 訳に出てくる品詞（出てくる順、重複なし）。
pub fn pos_summary(headword: &str, meanings: &[String]) -> Vec<PartOfSpeech> {
    let mut out = Vec::new();
    for (pos, _) in pos_groups(headword, meanings) {
        if let Some(pos) = pos
            && !out.contains(&pos)
        {
            out.push(pos);
        }
    }
    out
}

/// 品詞ごとの票（入れた順を覚えておき、同点なら先に入れたものを選ぶ）。
#[derive(Default)]
struct Votes(Vec<(PartOfSpeech, f32)>);

impl Votes {
    fn add(&mut self, pos: PartOfSpeech, weight: f32) {
        match self.0.iter_mut().find(|(p, _)| *p == pos) {
            Some((_, w)) => *w += weight,
            None => self.0.push((pos, weight)),
        }
    }

    fn get(&self, pos: PartOfSpeech) -> f32 {
        self.0
            .iter()
            .find(|(p, _)| *p == pos)
            .map_or(0.0, |(_, w)| *w)
    }

    fn best(&self) -> Option<PartOfSpeech> {
        let mut best: Option<(PartOfSpeech, f32)> = None;
        for &(pos, w) in &self.0 {
            if best.is_none_or(|(_, bw)| w > bw) {
                best = Some((pos, w));
            }
        }
        best.map(|(p, _)| p)
    }
}

/// 訳語 1 つ（「,」などで区切った 1 語）の語尾から品詞を推定する。
fn alt_pos(a: &str) -> Option<PartOfSpeech> {
    use PartOfSpeech::*;
    let last = a.chars().last()?;
    let prev = a.chars().rev().nth(1);
    let ends = |list: &[&str]| list.iter().any(|e| a.ends_with(e));

    if ends(&[
        "こと", "もの", "人", "者", "物", "事", "さ", "み", "状態", "性",
    ]) {
        return Some(Noun);
    }
    if ends(&[
        "する",
        "させる",
        "される",
        "できる",
        "になる",
        "れる",
        "せる",
    ]) {
        return Some(Verb);
    }
    if ends(&[
        "のある",
        "がある",
        "のない",
        "ない",
        "た",
        "ている",
        "でいる",
        "べき",
        "な",
        "の",
        "的",
    ]) {
        return Some(Adjective);
    }
    if last == 'い' {
        let adj = prev.is_some_and(|c| ADJ_KANJI.contains(c))
            || ends(&[
                "しい", "たい", "かい", "らい", "さい", "るい", "ろい", "ぽい", "ごい", "どい",
                "まい", "わい", "ぶい", "ずい", "つい", "すい", "よい", "いい", "うい", "がい",
                "ばい", "ぐい",
            ]);
        return Some(if adj { Adjective } else { Noun });
    }
    if ends(&["に", "と", "て", "で", "へ", "から", "まで", "より", "には"]) {
        return Some(Adverb);
    }
    if last == 'く' {
        let adverb = prev.is_some_and(|c| ADJ_KANJI.contains(c))
            || ends(&[
                "しく", "なく", "やく", "よく", "まく", "たく", "かく", "さく", "るく", "ろく",
                "ごく", "どく", "ずく", "ぶく",
            ]);
        return Some(if adverb { Adverb } else { Verb });
    }
    if "うくぐすつぬぶむる".contains(last) {
        return Some(Verb);
    }
    Some(Noun)
}

/// 「…が」「…ので」のように、節を導く訳か。
fn is_conjunction_ending(a: &str) -> bool {
    [
        "が",
        "けれども",
        "けれど",
        "ので",
        "ならば",
        "なら",
        "ても",
        "でも",
        "とき",
        "時",
        "間に",
        "以来",
        "ように",
        "ために",
        "かどうか",
    ]
    .iter()
    .any(|e| a.ends_with(e))
}

/// 英単語の語尾から見た品詞と、その重み（-tion, -ness などは強く、-al, -er などは弱く効かせる）。
fn suffix_prior(headword: &str) -> Option<(PartOfSpeech, f32)> {
    use PartOfSpeech::*;
    let h = headword.trim().to_ascii_lowercase();
    if h.is_empty() || !h.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let table: [(PartOfSpeech, f32, &[&str]); 5] = [
        (
            Noun,
            1.6,
            &["tion", "sion", "ness", "ity", "ment", "ism", "ship", "hood"],
        ),
        (Adjective, 1.6, &["ous", "able", "ible", "less", "worthy"]),
        (Adverb, 0.9, &["ly", "wise", "ward", "wards"]),
        (
            Noun,
            0.9,
            &["ist", "er", "or", "ance", "ence", "age", "ure", "dom"],
        ),
        (
            Adjective,
            0.9,
            &[
                "ful", "ive", "ic", "ical", "ent", "ant", "ary", "ish", "ese", "an", "al",
            ],
        ),
    ];
    table.into_iter().find_map(|(pos, weight, suffixes)| {
        suffixes
            .iter()
            .any(|s| h.len() > s.len() + 2 && h.ends_with(s))
            .then_some((pos, weight))
    })
}

/// open〜close で囲まれた部分（入れ子を含む）を取り除く。
fn remove_between(s: &str, open: char, close: char) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0u32;
    for c in s.chars() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            out.push(c);
        }
    }
    out
}

/// open〜close の中身だけを空にする（「〈人〉を」→「〈〉を」。中の「・」で区切らないため）。
fn blank_between(s: &str, open: char, close: char) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0u32;
    for c in s.chars() {
        if c == open {
            if depth == 0 {
                out.push(c);
            }
            depth += 1;
        } else if c == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                out.push(c);
            }
        } else if depth == 0 {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::PartOfSpeech::*;
    use super::*;

    fn pos(head: &str, sense: &str) -> Option<PartOfSpeech> {
        guess_pos(head, sense)
    }

    #[test]
    fn nouns() {
        assert_eq!(pos("light", "〈U〉『光,光線』;明るさ"), Some(Noun));
        assert_eq!(pos("well", "『井戸』"), Some(Noun));
        assert_eq!(
            pos("snicker", "(人をばかにした)忍び笑い[擬声語]"),
            Some(Noun)
        );
        assert_eq!(
            pos(
                "nonconformity",
                "(規則などに)従わないこと,協調(順応)しないこと"
            ),
            Some(Noun)
        );
        assert_eq!(pos("raunchiness", "わいせつ"), Some(Noun));
    }

    #[test]
    fn verbs() {
        assert_eq!(
            pos(
                "adapt",
                "(…に)…を『適応させる』,適合させる《+『名』+『to』+『名』(do『ing』)》"
            ),
            Some(Verb)
        );
        assert_eq!(pos("run", "『走る』,駆ける"), Some(Verb));
        assert_eq!(
            pos("rustle", "〈葉・紙などが〉『サラサラ音を立てる』"),
            Some(Verb)
        );
        assert_eq!(pos("limit", "…を『制限する』"), Some(Verb));
        assert_eq!(
            pos("well", "〈水などが〉わき出る,噴出する《+out》"),
            Some(Verb)
        );
    }

    #[test]
    fn adjectives() {
        assert_eq!(
            pos(
                "capable",
                "『手腕のある』,有能な,才能のある(able, competent)"
            ),
            Some(Adjective)
        );
        assert_eq!(pos("relevant", "適切な"), Some(Adjective));
        assert_eq!(
            pos("fast", "『速い』,急速な,すばやい,敏速な"),
            Some(Adjective)
        );
        assert_eq!(pos("abominable", "憎むべき,いまわしい"), Some(Adjective));
        assert_eq!(
            pos("cloud-capped", "(山・丘など)頂上が雲に包まれた"),
            Some(Adjective)
        );
    }

    #[test]
    fn adverbs_prepositions_conjunctions() {
        assert_eq!(pos("fast", "『速く』;すばやく(quickly)"), Some(Adverb));
        assert_eq!(
            pos("quickly", "『速く』,すみやかに,機敏に,ただちに"),
            Some(Adverb)
        );
        assert_eq!(
            pos("beyond", "《場所》…『を越えて』,『の向こうに』(『へ』)"),
            Some(Preposition)
        );
        assert_eq!(pos("through", "『…を貫いて,通って』"), Some(Preposition));
        assert_eq!(
            pos("although", "…『ではあるが』,…とはいいながら,…だが(though)"),
            Some(Conjunction)
        );
    }

    #[test]
    fn groups_follow_order() {
        let meanings: Vec<String> = [
            "『速い』,急速な",
            "(時計が)『進んでいる』",
            "『速く』;すばやく(quickly)",
            "断食(だんじき),絶食",
            "断食する,絶食する",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let groups = pos_groups("fast", &meanings);
        assert_eq!(
            groups,
            vec![
                (Some(Adjective), 0..2),
                (Some(Adverb), 2..3),
                (Some(Noun), 3..4),
                (Some(Verb), 4..5),
            ]
        );
        assert_eq!(
            pos_summary("fast", &meanings),
            vec![Adjective, Adverb, Noun, Verb]
        );
    }

    #[test]
    fn unknown_joins_previous() {
        let meanings: Vec<String> = ["『走る』", "", "=run"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(pos("run", ""), None);
        let groups = pos_groups("run", &meanings);
        assert_eq!(groups[0], (Some(Verb), 0..2));
    }

    #[test]
    fn user_meanings_with_japanese_separators() {
        assert_eq!(pos("modality", "様式、様相"), Some(Noun));
        assert_eq!(pos("tokenize", "トークンに分割する"), Some(Verb));
    }
}
