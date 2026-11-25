//! 文法タグ（data/grammar_tags.yaml）の読み込みと、LLM によるタグ付けの入出力。
//!
//! LLM に渡すのはタグの id / name / hint（このリポジトリで自作したもの）だけで、point は画面にだけ出す。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use paper_reader_shared::ExplainTag;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::llm::extract_json_object;

/// タグのカテゴリ（表示の順番）。
pub const CATEGORIES: &[&str] = &[
    "時制",
    "仮定法",
    "助動詞",
    "不定詞",
    "動名詞",
    "分詞",
    "受動態",
    "関係詞",
    "比較",
    "冠詞・名詞",
    "名詞",
    "代名詞",
    "形容詞",
    "副詞・語法",
    "前置詞",
    "接続詞",
    "文型・語法",
    "論文構文",
];

/// タグ付けで残す最小の確信度
pub const MIN_CONFIDENCE: f64 = 0.5;
/// タグの最大件数
pub const MAX_TAGS: usize = 5;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TagDef {
    pub id: String,
    pub name: String,
    pub category: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub hint: String,
    /// 画面に出す「つかみ方」。LLM には渡さない
    #[serde(default)]
    pub point: String,
    /// このタグが付いたら一緒に付けるタグ（より一般的な項目）
    #[serde(default)]
    pub implies: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TagsFile {
    tags: Vec<TagDef>,
}

/// 読み込み済みの文法データ。
#[derive(Debug, Clone)]
pub struct GrammarData {
    pub tags: Vec<TagDef>,
    index: HashMap<String, usize>,
    /// grammar_tags.yaml の内容の sha256（解説キャッシュのキーに含める）
    pub tags_hash: String,
}

impl GrammarData {
    /// ファイルから読み込んで検証する。
    pub fn load(path: &Path) -> Result<Self> {
        Self::from_text(&read(path)?, &path.display().to_string())
    }

    pub fn from_text(text: &str, name: &str) -> Result<Self> {
        let tags = parse_tags(text, name)?;
        let index = tags
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.clone(), i))
            .collect();
        Ok(Self {
            tags,
            index,
            tags_hash: sha256_hex(text),
        })
    }

    pub fn tag(&self, id: &str) -> Option<&TagDef> {
        self.index.get(id).map(|&i| &self.tags[i])
    }

    pub fn ids(&self) -> Vec<&str> {
        self.tags.iter().map(|t| t.id.as_str()).collect()
    }

    /// 検出したタグに名前・カテゴリ・説明を付ける（不明な id は捨てる）。
    pub fn resolve(&self, detected: &[DetectedTag]) -> Vec<ExplainTag> {
        detected
            .iter()
            .filter_map(|d| {
                let tag = self.tag(&d.tag_id)?;
                Some(ExplainTag {
                    tag_id: tag.id.clone(),
                    name: tag.name.clone(),
                    category: tag.category.clone(),
                    span: d.span.clone(),
                    hint: tag.hint.clone(),
                    point: tag.point.clone(),
                })
            })
            .collect()
    }
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("{} を読み込めません", path.display()))
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn yaml_error(name: &str, e: serde_yaml_ng::Error) -> anyhow::Error {
    match e.location() {
        Some(loc) => anyhow!("{name}:{}: YAML を解釈できません: {e}", loc.line()),
        None => anyhow!("{name}: YAML を解釈できません: {e}"),
    }
}

fn valid_id(id: &str) -> bool {
    id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn parse_tags(text: &str, name: &str) -> Result<Vec<TagDef>> {
    let file: TagsFile = serde_yaml_ng::from_str(text).map_err(|e| yaml_error(name, e))?;
    let mut seen = HashSet::new();
    for tag in &file.tags {
        let line = find_line(text, &tag.id, 0).unwrap_or(0);
        if !valid_id(&tag.id) {
            bail!(
                "{name}:{line}: タグ id「{}」は英小文字・数字・ハイフンで書いてください",
                tag.id
            );
        }
        if !seen.insert(tag.id.as_str()) {
            bail!("{name}:{line}: タグ id「{}」が重複しています", tag.id);
        }
        if !CATEGORIES.contains(&tag.category.as_str()) {
            bail!(
                "{name}:{line}: タグ「{}」のカテゴリ「{}」は不明です（{}）",
                tag.id,
                tag.category,
                CATEGORIES.join(" / ")
            );
        }
        if tag.name.trim().is_empty() || tag.hint.trim().is_empty() {
            bail!(
                "{name}:{line}: タグ「{}」の name と hint は必須です",
                tag.id
            );
        }
    }
    if file.tags.is_empty() {
        bail!("{name}: タグが 1 つもありません");
    }
    for tag in &file.tags {
        for implied in &tag.implies {
            if !seen.contains(implied.as_str()) || implied == &tag.id {
                let line = find_line(text, implied, 0).unwrap_or(0);
                bail!(
                    "{name}:{line}: タグ「{}」の implies「{implied}」は不明です",
                    tag.id
                );
            }
        }
    }
    Ok(file.tags)
}

/// `from_line`（0 始まりの行）以降で、`word` を語として含む最初のコメント以外の行（1 始まり）。
fn find_line(text: &str, word: &str, from_line: usize) -> Option<usize> {
    text.lines()
        .enumerate()
        .skip(from_line)
        .filter(|(_, line)| !line.trim_start().starts_with('#'))
        .find(|(_, line)| contains_word(line, word))
        .map(|(i, _)| i + 1)
}

fn contains_word(line: &str, word: &str) -> bool {
    let is_id_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    line.match_indices(word).any(|(pos, _)| {
        let before = line[..pos].chars().next_back();
        let after = line[pos + word.len()..].chars().next();
        !before.is_some_and(is_id_char) && !after.is_some_and(is_id_char)
    })
}

// ───────────── タグ付け（LLM） ─────────────

/// LLM が検出したタグ（解説キャッシュにもこの形で保存する）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DetectedTag {
    pub tag_id: String,
    #[serde(default)]
    pub span: String,
    #[serde(default)]
    pub confidence: f64,
}

/// タグ付けのシステムプロンプト。タグの id / name / hint の一覧を渡す。
pub fn tagging_system_prompt(g: &GrammarData) -> String {
    let mut s = String::from(
        "あなたは英文法の専門家です。ユーザーが示す英文（英語論文の 1 文）で使われている文法項目を、下の一覧から選んでください。\n\
         - 一覧にある tag_id だけを使う。文の読解に役立つものをすべて挙げる（1 文に 2〜4 個あるのが普通）。最大 5 個まで\n\
         - 具体的な項目を選んだら、それを含む一般的な項目も挙げる（例: 完了形の分詞構文 を選んだら 分詞構文 も）\n\
         - 主語が物・事柄で、人を主語にして訳し直すと自然になる文には inanimate-subject（無生物主語）も付ける\n\
         - 抽象名詞（fact, idea, assumption など）+ that 節は appositive-that（同格の that）。nominalization（名詞構文）とは区別する\n\
         - span には、その文法項目に当たる部分だけを英文からそのまま短く抜き出す（言い換えない。文全体にしない）\n\
         - confidence は 0.0〜1.0 の確信度。はっきりしないものは低くする\n\
         - 出力は JSON だけ: {\"tags\": [{\"tag_id\": \"...\", \"span\": \"...\", \"confidence\": 0.9}]}\n\
         - 該当が無ければ {\"tags\": []}\n\n\
         例1: The assumption that samples are independent simplifies the analysis.\n\
         → {\"tags\": [{\"tag_id\": \"appositive-that\", \"span\": \"The assumption that samples are independent\", \"confidence\": 0.9}, \
         {\"tag_id\": \"inanimate-subject\", \"span\": \"The assumption that samples are independent simplifies\", \"confidence\": 0.8}]}\n\
         例2: Having collected the data, we built a classifier based on it.\n\
         → {\"tags\": [{\"tag_id\": \"perfect-participle\", \"span\": \"Having collected the data\", \"confidence\": 0.95}, \
         {\"tag_id\": \"participial-construction\", \"span\": \"Having collected the data\", \"confidence\": 0.9}, \
         {\"tag_id\": \"past-participle-postmodifier\", \"span\": \"a classifier based on it\", \"confidence\": 0.7}]}\n\
         例3: Our analysis shows that the bias decreases as the data grow.\n\
         → {\"tags\": [{\"tag_id\": \"noun-clause-that-whether\", \"span\": \"that the bias decreases\", \"confidence\": 0.9}, \
         {\"tag_id\": \"inanimate-subject\", \"span\": \"Our analysis shows\", \"confidence\": 0.85}, \
         {\"tag_id\": \"subordinating-conjunction\", \"span\": \"as the data grow\", \"confidence\": 0.7}]}\n\n\
         文法項目の一覧（tag_id: 名前 — 説明）:\n",
    );
    for t in &g.tags {
        s.push_str(&format!("- {}: {} — {}\n", t.id, t.name, t.hint));
    }
    s
}

pub fn tagging_user_message(text: &str) -> String {
    format!("英文:\n\"\"\"\n{}\n\"\"\"", text.trim())
}

/// 構造化出力（response_format: json_schema）。tag_id はタグ id の enum に限る。
pub fn tagging_schema(g: &GrammarData) -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "grammar_tags",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "tags": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "tag_id": { "type": "string", "enum": g.ids() },
                                "span": { "type": "string" },
                                "confidence": { "type": "number" }
                            },
                            "required": ["tag_id", "span", "confidence"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["tags"],
                "additionalProperties": false
            }
        }
    })
}

/// LLM の応答（JSON 文字列）を検証してタグにする。壊れていれば空。
///
/// - 前後に文章やコードブロックが付いていても、最初の `{` から最後の `}` までを読む
/// - 不明な id、confidence が 0.5 未満のもの、同じ id の重複を捨て、確信度の高い順に最大 5 件
/// - span が英文に含まれなければ空にする（ハイライトできないため）
pub fn parse_tagging_response(content: &str, g: &GrammarData, sentence: &str) -> Vec<DetectedTag> {
    let Some(json) = extract_json_object(content) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(items) = value.get("tags").and_then(Value::as_array) else {
        return Vec::new();
    };
    let sentence_norm = normalize(sentence);
    let mut out: Vec<DetectedTag> = Vec::new();
    for item in items {
        let Ok(mut tag) = serde_json::from_value::<DetectedTag>(item.clone()) else {
            continue;
        };
        tag.tag_id = tag.tag_id.trim().to_string();
        if g.tag(&tag.tag_id).is_none()
            || !tag.confidence.is_finite()
            || tag.confidence < MIN_CONFIDENCE
            || out.iter().any(|t| t.tag_id == tag.tag_id)
        {
            continue;
        }
        tag.confidence = tag.confidence.min(1.0);
        tag.span = tag.span.split_whitespace().collect::<Vec<_>>().join(" ");
        if !sentence_norm.contains(&normalize(&tag.span)) {
            tag.span.clear();
        }
        out.push(tag);
    }
    out.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    // implies で一般的な項目を足す（同じ span・確信度で、元のタグのすぐ後ろ）
    let mut expanded: Vec<DetectedTag> = Vec::new();
    for tag in out {
        let implied: Vec<DetectedTag> = g
            .tag(&tag.tag_id)
            .map(|def| def.implies.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|id| DetectedTag {
                tag_id: id,
                span: tag.span.clone(),
                confidence: tag.confidence,
            })
            .collect();
        if !expanded.iter().any(|t| t.tag_id == tag.tag_id) {
            expanded.push(tag);
        }
        for t in implied {
            if !expanded.iter().any(|e| e.tag_id == t.tag_id) {
                expanded.push(t);
            }
        }
    }
    expanded.truncate(MAX_TAGS);
    expanded
}

fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const TAGS: &str = r#"
tags:
  - id: relative-pronoun
    name: 関係代名詞（主格・目的格）
    category: 関係詞
    aliases: [目的格の省略]
    hint: "who/which/that が節を導く。"
  - id: relative-adverb
    name: 関係副詞
    category: 関係詞
    hint: "where/when/why/how。"
  - id: passive
    name: 受動態
    category: 受動態
    hint: "be + p.p.。"
    point: "つかみ方の文。"
  - id: appositive-that
    name: 同格の that
    category: 論文構文
    hint: "名詞 + that 節。"
  - id: relative-reduced
    name: 目的格の省略
    category: 関係詞
    hint: "省略。"
    implies: [relative-pronoun]
"#;

    pub fn sample() -> GrammarData {
        GrammarData::from_text(TAGS, "tags.yaml").unwrap()
    }

    #[test]
    fn repository_file_is_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/grammar_tags.yaml");
        let g = GrammarData::load(&path).unwrap();
        assert!(g.tags.len() >= 60, "{}", g.tags.len());
        // すべてのカテゴリにタグがある
        for cat in CATEGORIES {
            assert!(g.tags.iter().any(|t| t.category == *cat), "{cat}");
        }
        assert_eq!(g.tags_hash.len(), 64);
    }

    #[test]
    fn invalid_files() {
        let bad = |tags: &str| {
            GrammarData::from_text(tags, "t.yaml")
                .unwrap_err()
                .to_string()
        };
        // 不明なカテゴリ
        let e = bad(&TAGS.replace("category: 受動態", "category: 受け身"));
        assert!(e.contains("t.yaml:") && e.contains("受け身"), "{e}");
        // id の形式
        let e = bad(&TAGS.replace("id: passive", "id: Passive"));
        assert!(e.contains("Passive"), "{e}");
        // id の重複
        let e = bad(&TAGS.replace("id: relative-adverb", "id: passive"));
        assert!(e.contains("重複"), "{e}");
        // YAML の構文エラーは行番号つき
        let e = bad("tags: [\n  - x");
        assert!(e.starts_with("t.yaml:"), "{e}");
    }

    #[test]
    fn tagging_response_parsing() {
        let g = sample();
        let sentence = "The method we propose is evaluated, the fact that X holds.";
        let content = r#"結果です:
```json
{"tags": [
  {"tag_id": "relative-pronoun", "span": "The method  we propose", "confidence": 0.9},
  {"tag_id": "passive", "span": "is evaluated", "confidence": 0.95},
  {"tag_id": "relative-pronoun", "span": "dup", "confidence": 0.8},
  {"tag_id": "no-such-tag", "span": "x", "confidence": 0.99},
  {"tag_id": "appositive-that", "span": "not in sentence", "confidence": 0.6},
  {"tag_id": "relative-adverb", "span": "x", "confidence": 0.3},
  {"tag_id": "passive"}
]}
```"#;
        let tags = parse_tagging_response(content, &g, sentence);
        let ids: Vec<_> = tags.iter().map(|t| t.tag_id.as_str()).collect();
        assert_eq!(ids, ["passive", "relative-pronoun", "appositive-that"]);
        assert_eq!(tags[1].span, "The method we propose");
        // 英文に無い span は空にする
        assert_eq!(tags[2].span, "");
    }

    #[test]
    fn implied_tags_are_added() {
        let g = sample();
        let content = r#"{"tags": [
          {"tag_id": "relative-reduced", "span": "we propose", "confidence": 0.8},
          {"tag_id": "passive", "span": "is evaluated", "confidence": 0.9}
        ]}"#;
        let tags = parse_tagging_response(content, &g, "The method we propose is evaluated.");
        let ids: Vec<_> = tags.iter().map(|t| t.tag_id.as_str()).collect();
        assert_eq!(ids, ["passive", "relative-reduced", "relative-pronoun"]);
        assert_eq!(tags[2].span, "we propose");
        // すでに付いていれば重ねない
        let content = r#"{"tags": [
          {"tag_id": "relative-pronoun", "span": "x", "confidence": 0.9},
          {"tag_id": "relative-reduced", "span": "we propose", "confidence": 0.8}
        ]}"#;
        let tags = parse_tagging_response(content, &g, "The method we propose is evaluated.");
        assert_eq!(tags.len(), 2);
        // implies に不明な id があればエラー
        let bad = TAGS.replace("implies: [relative-pronoun]", "implies: [nope]");
        assert!(GrammarData::from_text(&bad, "t").is_err());
    }

    #[test]
    fn broken_tagging_response_gives_no_tags() {
        let g = sample();
        for content in [
            "",
            "すみません、分かりません",
            "{broken",
            r#"{"tags": "relative-pronoun"}"#,
            r#"{"other": []}"#,
            r#"[{"tag_id": "passive"}]"#,
        ] {
            assert!(
                parse_tagging_response(content, &g, "x").is_empty(),
                "{content}"
            );
        }
    }

    #[test]
    fn at_most_five_tags() {
        let tags_yaml: String = std::iter::once("tags:\n".to_string())
            .chain((0..8).map(|i| {
                format!("  - id: t{i}\n    name: n{i}\n    category: 時制\n    hint: h\n")
            }))
            .collect();
        let g = GrammarData::from_text(&tags_yaml, "t").unwrap();
        let items: Vec<String> = (0..8)
            .map(|i| {
                format!(
                    r#"{{"tag_id":"t{i}","span":"","confidence":0.{}}}"#,
                    5 + i % 5
                )
            })
            .collect();
        let content = format!(r#"{{"tags":[{}]}}"#, items.join(","));
        assert_eq!(parse_tagging_response(&content, &g, "x").len(), MAX_TAGS);
    }

    #[test]
    fn resolve_and_schema() {
        let g = sample();
        let resolved = g.resolve(&[
            DetectedTag {
                tag_id: "relative-pronoun".into(),
                span: "we propose".into(),
                confidence: 0.9,
            },
            DetectedTag {
                tag_id: "gone".into(),
                span: String::new(),
                confidence: 0.9,
            },
            DetectedTag {
                tag_id: "appositive-that".into(),
                span: String::new(),
                confidence: 0.9,
            },
        ]);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].name, "関係代名詞（主格・目的格）");
        assert_eq!(resolved[1].category, "論文構文");

        let schema = tagging_schema(&g);
        let ids = &schema["json_schema"]["schema"]["properties"]["tags"]["items"]["properties"]["tag_id"]
            ["enum"];
        assert_eq!(ids.as_array().unwrap().len(), 5);
        let prompt = tagging_system_prompt(&g);
        assert!(prompt.contains("relative-adverb: 関係副詞"));
        // point は画面用で、タグ付けのプロンプトには入れない
        assert!(!prompt.contains("つかみ方の文"));
        assert_eq!(
            g.resolve(&[DetectedTag {
                tag_id: "passive".into(),
                span: String::new(),
                confidence: 0.9,
            }])[0]
                .point,
            "つかみ方の文。"
        );
    }

    #[test]
    fn line_helpers() {
        let text = "a:\n  # tags: [x]\n  - id: l1\n    tags: [x-y, x]\n";
        assert_eq!(find_line(text, "x", 2), Some(4));
        assert!(contains_word("[x-y, x]", "x"));
        assert!(!contains_word("[x-y]", "x"));
    }
}
