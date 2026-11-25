//! LLM に渡すプロンプト。
//!
//! 文言を変えたら `PROMPT_VERSION` を上げる（解説キャッシュのキーに含める）。
//!
//! 市販の教材・講座の内容はプロンプトに入れない。
//! 入れるのは、このリポジトリで自作した文法タグの名前と、英文中の該当箇所（span）だけ。

use paper_reader_shared::{ExplainRequest, Mode};

pub const PROMPT_VERSION: u32 = 3;

const COMMON: &str = "\
あなたは英語論文の読解を助ける日本語の講師です。
- 出力は日本語の Markdown で書いてください。見出しは指定されたものだけを、指定の順番で使ってください。
- 前置き・挨拶・まとめの一言は書かず、最初の見出しから始めてください。
- 推測を含む場合は「（推測）」と明記してください。
- 数式や引用番号（[12] など）は解説の対象から外してかまいません。
- 簡潔に、要点を優先してください。
- 文法用語は日本の学校文法の呼び方に揃えてください（例: 第5文型、分詞構文、関係副詞、仮定法過去完了、同格の that、無生物主語）。";

const WORD: &str = "\
ユーザーが英語論文の中で選択した単語について、次の見出しで解説してください。

## 文脈での意味
この文脈でどの意味で使われているかを最優先で、1〜3 文で説明する。文脈に合う訳語を太字で示す。

## 品詞と用法
品詞、よく使われる形（コロケーション、前置詞との組み合わせなど）を箇条書きで簡潔に。

## この分野での使われ方
専門用語としての意味や、論文でよく使われる言い回しがあれば書く。特に無ければこの見出しごと省略する。";

/// 英文の和訳（解説より先に、短いプロンプトで流す。最初の文字が出るまでを短くするため）。
const TRANSLATION: &str = "\
あなたは英語論文の翻訳者です。ユーザーが選択した英文を、文脈を踏まえて自然で正確な日本語に訳してください。
- 訳すのは「対象の英文」だけです。文脈（段落）は意味を決める参考にするだけで、対象の英文以外の文は訳さないでください。
- 訳文だけを出力してください。見出し・前置き・解説・原文は書かないでください。
- 専門用語は分野で一般的な訳語を使い、定訳が無ければカタカナ語か原語のまま残してください。
- 数式や引用番号（[12] など）はそのまま残してください。";

/// 文法タグが検出されなかったときの「## 文法ポイント」の指示。
const NO_GRAMMAR_NOTE: &str = "検出した文法項目: なし（「## 文法ポイント」では、関係詞、分詞構文、仮定法、倒置、無生物主語、省略など該当するものだけを箇条書きで説明し、特に無ければ「特になし」と書く）";

/// 和訳の見出し（和訳はサーバーが見出しを付けて先に流す）。
pub const TRANSLATION_HEADING: &str = "## 和訳\n\n";

/// 英文の解説の書き出し。`{first}` は最初の見出し。
const SENTENCE_INTRO: &str = "\
ユーザーが英語論文の中で選択した英文について、次の見出しで解説してください。
「## 和訳」は先に表示済みなので書かず、「{first}」から始めてください。説明はユーザーメッセージの和訳と食い違わないようにしてください。";

/// スラッシュリーディングの見出し（web で表示・非表示を切り替えるときにも使う）。
pub const SLASH_HEADING: &str = paper_reader_shared::SLASH_READING_HEADING;

/// スラッシュリーディング（意味のまとまりごとに区切り、英語の語順のまま読む補助）。
const SLASH: &str = "\
## スラッシュリーディング
英文を意味のまとまり（チャンク）ごとに「 / 」で区切って 1 行で示し、その下にチャンクごとの直訳を、英文の語順のまま「- **チャンク**：直訳」の箇条書きで示す。
区切る位置の目安: 長い主部の後、前置詞句・to 不定詞・分詞（分詞構文）・関係詞節・接続詞で始まる節の前、カンマの後。1 チャンクは 2〜7 語ほどにし、冠詞と名詞、助動詞と動詞のように強く結びつく語の間では区切らない。";

const SENTENCE: &str = "\
## 文構造
主語・動詞・目的語・補語と、主な修飾関係を箇条書きで示す（例: 「- 主語: The model」）。

## 文法ポイント
ユーザーメッセージの「検出した文法項目」ごとに「### 項目名」の小見出しを立て、英文のどの部分（該当箇所）がどう働いているかを説明する。項目名はそのまま使い、言い換えない。一覧に無い重要な文法事項があれば、最後に箇条書きで補う。

## 語句
難しい語や専門用語を「- **英語**: 意味」の形で箇条書きにする。";

/// システムプロンプト。`slash` なら英文の解説の先頭にスラッシュリーディングを入れる（単語では無視）。
pub fn system_prompt(mode: Mode, slash: bool) -> String {
    match mode {
        Mode::Word => format!("{COMMON}\n\n{WORD}"),
        Mode::Sentence => {
            let first = if slash { SLASH_HEADING } else { "## 文構造" };
            let intro = SENTENCE_INTRO.replace("{first}", first);
            if slash {
                format!("{COMMON}\n\n{intro}\n\n{SLASH}\n\n{SENTENCE}")
            } else {
                format!("{COMMON}\n\n{intro}\n\n{SENTENCE}")
            }
        }
    }
}

/// 英文の和訳のシステムプロンプト。
pub fn translation_system_prompt() -> String {
    TRANSLATION.to_string()
}

/// 解説に添える、検出した文法項目（名前と英文中の該当箇所）。
pub struct GrammarNote<'a> {
    pub name: &'a str,
    pub span: &'a str,
}

/// 和訳のユーザーメッセージ。文脈は参考として先に置き、訳す英文を最後に置く
/// （文脈を後ろに置くと、続きの文まで訳してしまうことがあるため）。
pub fn translation_user_message(req: &ExplainRequest) -> String {
    let text = req.text.trim();
    let context = req.context.trim();
    if context.is_empty() || context == text {
        format!("訳す英文:\n\"\"\"\n{text}\n\"\"\"")
    } else {
        format!(
            "文脈（参考。訳さない）:\n\"\"\"\n{context}\n\"\"\"\n\n訳す英文（この文だけを訳す）:\n\"\"\"\n{text}\n\"\"\""
        )
    }
}

/// ユーザーメッセージ。文脈が選択文字列と同じなら省く。
/// 英文なら、先に流した和訳と、検出した文法項目を添える（和訳の問い合わせではどちらも無し）。
pub fn user_message(
    req: &ExplainRequest,
    grammar: &[GrammarNote<'_>],
    translation: Option<&str>,
) -> String {
    let has_context = !req.context.trim().is_empty() && req.context.trim() != req.text.trim();
    let target = match req.mode {
        Mode::Word => format!("対象の単語: {}", req.text.trim()),
        Mode::Sentence => format!("対象の英文:\n\"\"\"\n{}\n\"\"\"", req.text.trim()),
    };
    let mut msg = if has_context {
        format!(
            "{target}\n\n文脈（対象を含む段落）:\n\"\"\"\n{}\n\"\"\"",
            req.context.trim()
        )
    } else {
        target
    };
    if let Some(t) = translation.map(str::trim).filter(|t| !t.is_empty())
        && req.mode == Mode::Sentence
    {
        msg.push_str(&format!("\n\n和訳（表示済み）:\n\"\"\"\n{t}\n\"\"\""));
    }
    if req.mode == Mode::Sentence && grammar.is_empty() {
        // タグが無いときだけの指示（システムプロンプトに入れると、タグがあっても「特になし」と書きがちなため）
        msg.push_str(&format!("\n\n{NO_GRAMMAR_NOTE}"));
    }
    if req.mode == Mode::Sentence && !grammar.is_empty() {
        msg.push_str(
            "\n\n検出した文法項目（「## 文法ポイント」ではこの名前を「### 項目名」の見出しに使う）:",
        );
        for g in grammar {
            if g.span.is_empty() {
                msg.push_str(&format!("\n- {}", g.name));
            } else {
                msg.push_str(&format!("\n- {}（該当箇所: \"{}\"）", g.name, g.span));
            }
        }
    }
    msg
}

/// 単語帳: 辞書に無い語の訳と、カタカナの読みを推定させるシステムプロンプト。
/// `need_meanings` が false なら読みだけ（訳は辞書にある）。
pub fn word_note_system_prompt(need_meanings: bool) -> String {
    let mut s = String::from(
        "あなたは英和辞典の編集者です。英語論文に出てきた英単語について、JSON だけを出力してください（前置きやコードブロックは不要）。
- \"kana\": 英語の発音に近いカタカナ表記。アクセント記号や発音記号は付けない（例: relevant → レレバント、modality → モダリティ、data → データ、algorithm → アルゴリズム）。",
    );
    if need_meanings {
        s.push_str(
            "
- \"meanings\": 日本語の訳語を 1〜4 個、よく使う意味から順に。1 つ 30 文字以内で短く。品詞が分かるときは先頭に「（名）」「（動）」「（形）」「（副）」を付ける。文脈が与えられたら、その文脈での意味を最初に書く。
- 略語・固有名詞・造語などで意味に自信が無いときは、その訳の末尾に「（推測）」と付ける。英単語でなければ meanings は空の配列にする。",
        );
    } else {
        s.push_str("\n- \"meanings\": 空の配列にする（訳は辞書にあるため不要）。");
    }
    s
}

/// 単語帳の問い合わせのユーザーメッセージ。文脈は訳を頼むときだけ付ける。
pub fn word_note_user_message(word: &str, context: &str, need_meanings: bool) -> String {
    let context = context.trim();
    if need_meanings && !context.is_empty() {
        format!("単語: {word}\n\n文脈（単語を含む段落）:\n\"\"\"\n{context}\n\"\"\"")
    } else {
        format!("単語: {word}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_note_prompts() {
        let with = word_note_system_prompt(true);
        assert!(with.contains("\"kana\"") && with.contains("（推測）"));
        assert!(word_note_system_prompt(false).contains("空の配列"));
        let m = word_note_user_message("modalities", " multiple modalities ", true);
        assert!(m.contains("単語: modalities") && m.contains("multiple modalities"));
        // 読みだけのときは文脈を送らない
        assert_eq!(
            word_note_user_message("relevant", "ctx", false),
            "単語: relevant"
        );
    }

    fn req(mode: Mode, text: &str, context: &str) -> ExplainRequest {
        ExplainRequest {
            mode,
            text: text.into(),
            context: context.into(),
            regenerate: false,
            slash: false,
        }
    }

    #[test]
    fn system_prompt_has_sections() {
        let w = system_prompt(Mode::Word, false);
        assert!(w.contains("## 文脈での意味") && w.contains("## 品詞と用法"));
        assert!(!w.contains("## 和訳"));
        let s = system_prompt(Mode::Sentence, false);
        for h in ["## 文構造", "## 文法ポイント", "## 語句"] {
            assert!(s.contains(h), "{h}");
        }
        // 和訳は先に別の問い合わせで流すので、解説では書かせない
        assert!(s.contains("書かず"));
        let t = translation_system_prompt();
        assert!(t.contains("訳文だけ") && t.contains("以外の文は訳さない") && !t.contains("## "));
        assert!(s.contains("推測"));
        assert!(s.contains("引用番号"));
    }

    #[test]
    fn user_message_with_and_without_context() {
        let m = user_message(
            &req(
                Mode::Word,
                "transduction",
                "The dominant sequence transduction models",
            ),
            &[],
            None,
        );
        assert!(m.starts_with("対象の単語: transduction"));
        assert!(m.contains("The dominant sequence transduction models"));

        let m = user_message(
            &req(Mode::Sentence, "We propose X.", " We propose X. "),
            &[],
            None,
        );
        assert!(m.contains("We propose X."));
        assert!(!m.contains("文脈"), "文脈が同じなら省く: {m}");
        assert!(m.contains("検出した文法項目: なし"));
    }

    #[test]
    fn translation_message_puts_target_last() {
        let m = translation_user_message(&req(
            Mode::Sentence,
            "We propose X.",
            "We propose X. We also show Y.",
        ));
        assert!(m.starts_with("文脈（参考。訳さない）"), "{m}");
        assert!(
            m.ends_with("訳す英文（この文だけを訳す）:\n\"\"\"\nWe propose X.\n\"\"\""),
            "{m}"
        );
        let m = translation_user_message(&req(Mode::Sentence, "We propose X.", "We propose X."));
        assert_eq!(m, "訳す英文:\n\"\"\"\nWe propose X.\n\"\"\"");
    }

    #[test]
    fn user_message_with_grammar() {
        let notes = [
            GrammarNote {
                name: "分詞構文",
                span: "Having trained the model",
            },
            GrammarNote {
                name: "無生物主語",
                span: "",
            },
        ];
        let m = user_message(
            &req(Mode::Sentence, "Having trained the model, we ...", ""),
            &notes,
            Some(" モデルを訓練したあと、私たちは… "),
        );
        assert!(
            m.contains("和訳（表示済み）:\n\"\"\"\nモデルを訓練したあと、私たちは…\n\"\"\""),
            "{m}"
        );
        assert!(
            m.contains("- 分詞構文（該当箇所: \"Having trained the model\"）"),
            "{m}"
        );
        assert!(
            m.contains("- 無生物主語\n") || m.ends_with("- 無生物主語"),
            "{m}"
        );
        assert!(!m.contains("特になし"));
        assert!(!system_prompt(Mode::Sentence, false).contains("特になし"));
        // 単語のときは付けない
        let m = user_message(&req(Mode::Word, "model", ""), &notes, Some("モデル"));
        assert!(!m.contains("分詞構文") && !m.contains("和訳"));
        assert!(system_prompt(Mode::Sentence, false).contains("### 項目名"));
    }

    #[test]
    fn slash_reading_section() {
        let off = system_prompt(Mode::Sentence, false);
        assert!(!off.contains(SLASH_HEADING));
        assert!(off.contains("「## 文構造」から始めて"));
        let on = system_prompt(Mode::Sentence, true);
        assert!(on.contains("「## スラッシュリーディング」から始めて"));
        // スラッシュリーディングは文構造より前
        let (a, b) = (
            on.find("\n## スラッシュリーディング").unwrap(),
            on.find("\n## 文構造").unwrap(),
        );
        assert!(a < b);
        // 単語では付けない
        assert_eq!(
            system_prompt(Mode::Word, true),
            system_prompt(Mode::Word, false)
        );
    }
}
