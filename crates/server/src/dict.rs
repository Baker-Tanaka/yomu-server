//! `GET /api/dict?word=` — ローカル辞書（EJDict-hand）で単語を引く。

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use paper_reader_shared::{DictEntry, DictQuery, DictResponse, normalize_whitespace};
use sqlx::SqlitePool;

use crate::AppState;
use crate::error::{AppError, AppResult};

/// 検索語の最大文字数
const MAX_QUERY_CHARS: usize = 64;
/// 関連見出し語の最大数
const MAX_RELATED: usize = 2;

pub async fn get_dict(
    State(state): State<AppState>,
    query: Result<Query<DictQuery>, QueryRejection>,
) -> AppResult<Json<DictResponse>> {
    let Query(q) =
        query.map_err(|_| AppError::BadRequest("word パラメータを指定してください".into()))?;
    let word = normalize_query(&q.word)
        .ok_or_else(|| AppError::BadRequest("辞書で引ける単語ではありません".into()))?;
    Ok(Json(lookup_word(&state.db, word).await?))
}

async fn lookup_word(pool: &SqlitePool, query: String) -> AppResult<DictResponse> {
    let candidates = lemma_candidates(&query);

    // 1. 完全一致 → 2. 語形変化を還元した候補を順に
    let (headword, meaning, exact) = match lookup(pool, &query).await? {
        Some(m) => (query.clone(), m, true),
        None => {
            let mut found = None;
            for c in &candidates {
                if let Some(m) = lookup(pool, &c.word).await? {
                    found = Some((c.word.clone(), m));
                    break;
                }
            }
            match found {
                Some((w, m)) => (w, m, false),
                None => {
                    if !has_entries(pool).await? {
                        return Err(AppError::ServiceUnavailable(
                            "辞書データがインポートされていません（README の「辞書データ」を参照）"
                                .into(),
                        ));
                    }
                    return Ok(DictResponse {
                        query,
                        headword: None,
                        meanings: Vec::new(),
                        related: Vec::new(),
                    });
                }
            }
        }
    };

    // 関連見出し語: "running" → "run"、"went"（goの過去）→ "go" など
    let mut related_words: Vec<String> = Vec::new();
    if exact {
        for c in candidates.iter().filter(|c| c.suggest_for_exact) {
            if lookup(pool, &c.word).await?.is_some() {
                related_words.push(c.word.clone());
                break;
            }
        }
    }
    if let Some(lemma) = inflection_pointer(&meaning) {
        related_words.insert(0, lemma);
    }
    let mut related = Vec::new();
    for w in related_words {
        if w == headword || related.iter().any(|e: &DictEntry| e.headword == w) {
            continue;
        }
        if let Some(m) = lookup(pool, &w).await? {
            related.push(DictEntry {
                headword: w,
                meanings: split_meanings(&m),
            });
        }
        if related.len() >= MAX_RELATED {
            break;
        }
    }

    Ok(DictResponse {
        query,
        headword: Some(headword),
        meanings: split_meanings(&meaning),
        related,
    })
}

async fn lookup(pool: &SqlitePool, word: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT meaning FROM dict WHERE word = ?")
        .bind(word)
        .fetch_optional(pool)
        .await
}

async fn has_entries(pool: &SqlitePool) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dict)")
        .fetch_one(pool)
        .await
}

/// 前後の記号を除去して小文字化する。所有格の 's も外す。
/// 辞書で引けない入力（空、長すぎる）なら `None`。
pub fn normalize_query(raw: &str) -> Option<String> {
    let s = raw.replace(['\u{2019}', '\u{2018}', '\u{02BC}'], "'");
    let s = normalize_whitespace(&s);
    let s = s.trim_matches(|c: char| !c.is_alphanumeric());
    let s = s.strip_suffix("'s").unwrap_or(s);
    let s = s.trim_matches(|c: char| !c.is_alphanumeric());
    let s = s.to_lowercase();
    (!s.is_empty() && s.chars().count() <= MAX_QUERY_CHARS).then_some(s)
}

/// 還元の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 複数形・三人称単数（-s, -es, -ies, -ves）
    Plural,
    /// 過去形・過去分詞・現在分詞（-ed, -ied, -ing）
    Verb,
    /// 比較級・最上級（-er, -est, -ier, -iest）
    Compare,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub word: String,
    pub kind: Kind,
    /// 入力語そのものが見出し語にあるときも「原形の候補」として示してよいか。
    /// 動詞の変化（-ed, -ing）で、語幹が 3 文字以上か、-ed → e で語幹に母音を含む場合
    /// （used → use, aged → age は可、thing → the, shed → she は不可）。
    pub suggest_for_exact: bool,
}

/// 語形変化を還元した原形の候補を、試す順に返す。
///
/// - -ies → -y, -s → (なし), -es → (なし), -ves → -f / -fe
/// - -ied → -y, -ed → e / (なし), -ing → e / (なし), 子音重ね（running → run）
/// - -ier / -iest → -y, -er / -est → e / (なし), 子音重ね（bigger → big）
///
/// "-ed → e" を "-ed → (なし)" より先に試すのは、"used" → "us" や "based" → "bas" を避けるため。
/// 同様に "-s" を "-es" より先に試す（"uses" → "us" を避ける）。
pub fn lemma_candidates(word: &str) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut push = |stem: &str, add: &str, kind: Kind, suffix: &str| {
        let stem_len = stem.chars().count();
        if stem_len < 2 || !stem.chars().all(|c| c.is_ascii_alphabetic()) {
            return;
        }
        let w = format!("{stem}{add}");
        let has_vowel = stem.chars().any(|c| "aeiouy".contains(c));
        let suggest_for_exact =
            kind == Kind::Verb && (stem_len >= 3 || (suffix == "ed" && add == "e" && has_vowel));
        if w != word && !out.iter().any(|c| c.word == w) {
            out.push(Candidate {
                word: w,
                kind,
                suggest_for_exact,
            });
        }
    };

    // 複数形・三人称単数
    if let Some(stem) = word.strip_suffix("ies") {
        push(stem, "y", Kind::Plural, "ies");
    }
    if let Some(stem) = word.strip_suffix('s')
        && !word.ends_with("ss")
    {
        push(stem, "", Kind::Plural, "s");
    }
    if let Some(stem) = word.strip_suffix("es") {
        push(stem, "", Kind::Plural, "es");
    }
    if let Some(stem) = word.strip_suffix("ves") {
        push(stem, "f", Kind::Plural, "ves");
        push(stem, "fe", Kind::Plural, "ves");
    }

    // 過去形・過去分詞・現在分詞
    if let Some(stem) = word.strip_suffix("ied") {
        push(stem, "y", Kind::Verb, "ied");
    }
    for suffix in ["ed", "ing"] {
        if let Some(stem) = word.strip_suffix(suffix) {
            push(stem, "e", Kind::Verb, suffix);
            push(stem, "", Kind::Verb, suffix);
            if let Some(undoubled) = undouble(stem) {
                push(undoubled, "", Kind::Verb, suffix);
            }
        }
    }

    // 比較級・最上級
    for (suffix, y_suffix) in [("er", "ier"), ("est", "iest")] {
        if let Some(stem) = word.strip_suffix(y_suffix) {
            push(stem, "y", Kind::Compare, y_suffix);
        }
        if let Some(stem) = word.strip_suffix(suffix) {
            push(stem, "e", Kind::Compare, suffix);
            push(stem, "", Kind::Compare, suffix);
            if let Some(undoubled) = undouble(stem) {
                push(undoubled, "", Kind::Compare, suffix);
            }
        }
    }

    out
}

/// 末尾の子音が重なっていれば 1 つ外す（"runn" → "run"）。
fn undouble(stem: &str) -> Option<&str> {
    let bytes = stem.as_bytes();
    let n = bytes.len();
    if n < 3 {
        return None;
    }
    let (a, b) = (bytes[n - 1], bytes[n - 2]);
    let is_consonant = |c: u8| c.is_ascii_lowercase() && !b"aeiou".contains(&c);
    // "ll", "ss", "zz" は原形でも重なることが多い（call, pass, buzz）ので対象外
    (a == b && is_consonant(a) && !b"lsz".contains(&a)).then(|| &stem[..n - 1])
}

/// "leadの過去・過去分詞" や "childの複数形" から原形（lead, child）を取り出す。
pub fn inflection_pointer(meaning: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        "過去",
        "分詞",
        "複数",
        "三人称",
        "一人称",
        "比較級",
        "最上級",
    ];
    let first = meaning.split(" / ").next()?.trim();
    let (lemma, rest) = first.split_once('の')?;
    let lemma = lemma.trim();
    let valid = !lemma.is_empty()
        && lemma
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == '-' || c == ' ');
    (valid && MARKERS.iter().any(|m| rest.contains(m))).then(|| lemma.to_lowercase())
}

/// 元データの " / " 区切りで訳を分割する。
pub fn split_meanings(meaning: &str) -> Vec<String> {
    meaning
        .split(" / ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &str) -> Vec<String> {
        lemma_candidates(w).into_iter().map(|c| c.word).collect()
    }

    /// 候補のうち `dict` に存在する最初の語（実際の検索と同じ順序）。
    fn first_hit(w: &str, dict: &[&str]) -> Option<String> {
        words(w).into_iter().find(|c| dict.contains(&c.as_str()))
    }

    const DICT: &[&str] = &[
        "study", "propose", "run", "use", "us", "base", "watch", "go", "box", "leaf", "life",
        "big", "large", "fast", "happy", "plan", "call", "need", "make", "see", "model", "add",
        "pass", "hop", "hope", "early",
    ];

    #[test]
    fn normalize() {
        assert_eq!(normalize_query("Studies,"), Some("studies".into()));
        assert_eq!(
            normalize_query("(Transformer)."),
            Some("transformer".into())
        );
        assert_eq!(normalize_query("“model”"), Some("model".into()));
        assert_eq!(normalize_query("model's"), Some("model".into()));
        assert_eq!(normalize_query("model’s"), Some("model".into()));
        assert_eq!(
            normalize_query("state-of-the-art"),
            Some("state-of-the-art".into())
        );
        assert_eq!(normalize_query("  A\n level "), Some("a level".into()));
        assert_eq!(normalize_query("..."), None);
        assert_eq!(normalize_query(""), None);
        assert_eq!(normalize_query(&"a".repeat(65)), None);
    }

    #[test]
    fn required_examples() {
        assert_eq!(first_hit("studies", DICT), Some("study".into()));
        assert_eq!(first_hit("proposed", DICT), Some("propose".into()));
        assert_eq!(first_hit("running", DICT), Some("run".into()));
    }

    #[test]
    fn plurals() {
        assert_eq!(first_hit("models", DICT), Some("model".into()));
        assert_eq!(
            first_hit("uses", DICT),
            Some("use".into()),
            "us を先に引かない"
        );
        assert_eq!(first_hit("watches", DICT), Some("watch".into()));
        assert_eq!(first_hit("boxes", DICT), Some("box".into()));
        assert_eq!(first_hit("goes", DICT), Some("go".into()));
        assert_eq!(first_hit("leaves", DICT), Some("leaf".into()));
        assert_eq!(first_hit("lives", DICT), Some("life".into()));
        assert!(
            words("pass").iter().all(|w| w != "pas"),
            "-ss は -s を外さない"
        );
    }

    #[test]
    fn verbs() {
        assert_eq!(first_hit("studied", DICT), Some("study".into()));
        assert_eq!(
            first_hit("used", DICT),
            Some("use".into()),
            "us を先に引かない"
        );
        assert_eq!(first_hit("based", DICT), Some("base".into()));
        assert_eq!(first_hit("needed", DICT), Some("need".into()));
        assert_eq!(first_hit("added", DICT), Some("add".into()));
        assert_eq!(first_hit("planned", DICT), Some("plan".into()));
        assert_eq!(first_hit("called", DICT), Some("call".into()));
        assert_eq!(first_hit("passed", DICT), Some("pass".into()));
        assert_eq!(first_hit("hoped", DICT), Some("hope".into()));
        assert_eq!(first_hit("hopped", DICT), Some("hop".into()));
        assert_eq!(first_hit("making", DICT), Some("make".into()));
        assert_eq!(first_hit("seeing", DICT), Some("see".into()));
        assert_eq!(first_hit("using", DICT), Some("use".into()));
    }

    #[test]
    fn comparatives() {
        assert_eq!(first_hit("bigger", DICT), Some("big".into()));
        assert_eq!(first_hit("biggest", DICT), Some("big".into()));
        assert_eq!(first_hit("larger", DICT), Some("large".into()));
        assert_eq!(first_hit("largest", DICT), Some("large".into()));
        assert_eq!(first_hit("faster", DICT), Some("fast".into()));
        assert_eq!(first_hit("happier", DICT), Some("happy".into()));
        assert_eq!(first_hit("earliest", DICT), Some("early".into()));
    }

    #[test]
    fn suggest_for_exact() {
        let suggested = |w: &str| -> Vec<String> {
            lemma_candidates(w)
                .into_iter()
                .filter(|c| c.suggest_for_exact)
                .map(|c| c.word)
                .collect()
        };
        assert!(suggested("running").contains(&"run".to_string()));
        assert!(suggested("used").contains(&"use".to_string()));
        assert!(suggested("aged").contains(&"age".to_string()));
        assert!(suggested("studied").contains(&"study".to_string()));
        for w in ["thing", "shed", "being", "doing", "bring"] {
            assert!(suggested(w).is_empty(), "{w}: {:?}", suggested(w));
        }
        // 複数形・比較級は完全一致時には示さない（news → new, paper → pap を避ける）
        assert!(suggested("news").is_empty());
        assert!(suggested("paper").is_empty());
        let run = lemma_candidates("running")
            .into_iter()
            .find(|c| c.word == "run")
            .unwrap();
        assert_eq!(run.kind, Kind::Verb);
    }

    #[test]
    fn no_bogus_candidates() {
        assert!(words("is").is_empty());
        assert!(words("a").is_empty());
        assert!(words("x-ray's").iter().all(|w| !w.contains('\'')));
        assert!(!words("running").contains(&"running".to_string()));
    }

    #[test]
    fn pointers() {
        assert_eq!(
            inflection_pointer("leadの過去・過去分詞"),
            Some("lead".into())
        );
        assert_eq!(inflection_pointer("childの複数形"), Some("child".into()));
        assert_eq!(
            inflection_pointer("beの一人称および三人称単数過去形"),
            Some("be".into())
        );
        assert_eq!(
            inflection_pointer("goodの比較級 / よりよい"),
            Some("good".into())
        );
        assert_eq!(inflection_pointer("『資料』,事実;情報"), None);
        assert_eq!(inflection_pointer("(新しい)『報道』の記事"), None);
    }

    #[test]
    fn split() {
        assert_eq!(
            split_meanings("『走る』,駆ける / 〈人が〉急ぐ /  / 逃げる"),
            vec!["『走る』,駆ける", "〈人が〉急ぐ", "逃げる"]
        );
    }
}
