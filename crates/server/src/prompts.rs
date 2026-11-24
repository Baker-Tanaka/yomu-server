//! LLM に渡すプロンプト。
//!
//! 文言を変えたら `PROMPT_VERSION` を上げる（フェーズ4の解説キャッシュのキーに含める）。

use paper_reader_shared::{ExplainRequest, Mode};

pub const PROMPT_VERSION: u32 = 1;

const COMMON: &str = "\
あなたは英語論文の読解を助ける日本語の講師です。
- 出力は日本語の Markdown で書いてください。見出しは指定されたものだけを、指定の順番で使ってください。
- 前置き・挨拶・まとめの一言は書かず、最初の見出しから始めてください。
- 推測を含む場合は「（推測）」と明記してください。
- 数式や引用番号（[12] など）は解説の対象から外してかまいません。
- 簡潔に、要点を優先してください。";

const WORD: &str = "\
ユーザーが英語論文の中で選択した単語について、次の見出しで解説してください。

## 文脈での意味
この文脈でどの意味で使われているかを最優先で、1〜3 文で説明する。文脈に合う訳語を太字で示す。

## 品詞と用法
品詞、よく使われる形（コロケーション、前置詞との組み合わせなど）を箇条書きで簡潔に。

## この分野での使われ方
専門用語としての意味や、論文でよく使われる言い回しがあれば書く。特に無ければこの見出しごと省略する。";

const SENTENCE: &str = "\
ユーザーが英語論文の中で選択した英文について、次の見出しで解説してください。

## 和訳
自然で正確な日本語に訳す。

## 文構造
主語・動詞・目的語・補語と、主な修飾関係を箇条書きで示す（例: 「- 主語: The model」）。

## 文法ポイント
関係詞、分詞構文、仮定法、倒置、無生物主語、省略など、該当するものだけを箇条書きで説明する。特に無ければ「特になし」と書く。

## 語句
難しい語や専門用語を「- **英語**: 意味」の形で箇条書きにする。";

/// システムプロンプト。
pub fn system_prompt(mode: Mode) -> String {
    let specific = match mode {
        Mode::Word => WORD,
        Mode::Sentence => SENTENCE,
    };
    format!("{COMMON}\n\n{specific}")
}

/// ユーザーメッセージ。文脈が選択文字列と同じなら省く。
pub fn user_message(req: &ExplainRequest) -> String {
    let has_context = !req.context.trim().is_empty() && req.context.trim() != req.text.trim();
    let target = match req.mode {
        Mode::Word => format!("対象の単語: {}", req.text.trim()),
        Mode::Sentence => format!("対象の英文:\n\"\"\"\n{}\n\"\"\"", req.text.trim()),
    };
    if has_context {
        format!(
            "{target}\n\n文脈（対象を含む段落）:\n\"\"\"\n{}\n\"\"\"",
            req.context.trim()
        )
    } else {
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(mode: Mode, text: &str, context: &str) -> ExplainRequest {
        ExplainRequest {
            mode,
            text: text.into(),
            context: context.into(),
            regenerate: false,
        }
    }

    #[test]
    fn system_prompt_has_sections() {
        let w = system_prompt(Mode::Word);
        assert!(w.contains("## 文脈での意味") && w.contains("## 品詞と用法"));
        assert!(!w.contains("## 和訳"));
        let s = system_prompt(Mode::Sentence);
        for h in ["## 和訳", "## 文構造", "## 文法ポイント", "## 語句"] {
            assert!(s.contains(h), "{h}");
        }
        assert!(s.contains("推測"));
        assert!(s.contains("引用番号"));
    }

    #[test]
    fn user_message_with_and_without_context() {
        let m = user_message(&req(
            Mode::Word,
            "transduction",
            "The dominant sequence transduction models",
        ));
        assert!(m.starts_with("対象の単語: transduction"));
        assert!(m.contains("The dominant sequence transduction models"));

        let m = user_message(&req(Mode::Sentence, "We propose X.", " We propose X. "));
        assert!(m.contains("We propose X."));
        assert!(!m.contains("文脈"), "文脈が同じなら省く: {m}");
    }
}
