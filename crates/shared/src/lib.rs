//! server と web の両方で使う API の型。

use serde::{Deserialize, Serialize};

/// エラー応答の共通形式 `{ "error": "..." }`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorResponse {
    pub error: String,
}

/// `GET /api/paper?url=` のクエリ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperQuery {
    pub url: String,
}

/// `GET /api/paper` の応答。`html` はサーバーで無害化済み。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaperResponse {
    /// arXiv の論文 ID（例: `2301.00001v2`）
    pub id: String,
    pub title: String,
    pub html: String,
    /// 実際に取得した HTML 版の URL
    pub source_url: String,
}

/// `GET /api/health` の応答。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub server: bool,
    /// LLM ランタイムに疎通できたか（未確認なら `None`）
    pub llm: Option<bool>,
}

/// 解説のモード。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Word,
    Sentence,
}

impl Mode {
    /// 選択文字列から mode を判定する。空白で区切った語が 1 つなら `Word`。
    pub fn detect(text: &str) -> Mode {
        if text.split_whitespace().count() <= 1 {
            Mode::Word
        } else {
            Mode::Sentence
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Word => "word",
            Mode::Sentence => "sentence",
        }
    }
}

/// 連続する空白（改行含む）を 1 つの半角スペースにまとめ、前後を削る。
pub fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_mode() {
        assert_eq!(Mode::detect("transformer"), Mode::Word);
        assert_eq!(Mode::detect("  attention\n"), Mode::Word);
        assert_eq!(Mode::detect("state-of-the-art"), Mode::Word);
        assert_eq!(Mode::detect("We propose a model."), Mode::Sentence);
        assert_eq!(Mode::detect("large\nlanguage"), Mode::Sentence);
    }

    #[test]
    fn mode_serde() {
        assert_eq!(serde_json::to_string(&Mode::Word).unwrap(), "\"word\"");
        let m: Mode = serde_json::from_str("\"sentence\"").unwrap();
        assert_eq!(m, Mode::Sentence);
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_whitespace("  a \n\t b  c "), "a b c");
    }
}
