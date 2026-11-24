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
    /// 設定されているモデル名（LLM_MODEL）
    #[serde(default)]
    pub model: String,
    /// LLM ランタイムにそのモデルがあるか（疎通できなければ `None`）
    #[serde(default)]
    pub model_available: Option<bool>,
}

/// `GET /api/dict?word=` のクエリ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictQuery {
    pub word: String,
}

/// 辞書の 1 項目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DictEntry {
    pub headword: String,
    /// 元データの " / " 区切りで分割した訳。`『』` は重要な訳を示す（元データの記法）
    pub meanings: Vec<String>,
}

/// `GET /api/dict` の応答。見つからなければ `headword` は `None`、`meanings` は空。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DictResponse {
    /// 正規化（記号除去・小文字化）した検索語
    pub query: String,
    pub headword: Option<String>,
    pub meanings: Vec<String>,
    /// 関連する見出し語（"running" に対する "run"、"went" に対する "go" など）
    #[serde(default)]
    pub related: Vec<DictEntry>,
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

/// LLM に渡す選択文字列の上限（文字数）
pub const MAX_TEXT_CHARS: usize = 2_000;
/// LLM に渡す文脈の上限（文字数）
pub const MAX_CONTEXT_CHARS: usize = 4_000;

/// `POST /api/explain` の body。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExplainRequest {
    pub mode: Mode,
    pub text: String,
    #[serde(default)]
    pub context: String,
    /// true ならキャッシュを使わずに生成し直す
    #[serde(default)]
    pub regenerate: bool,
}

/// 検索の並び順。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SearchSort {
    /// 関連度順
    #[default]
    Relevance,
    /// 投稿日の新しい順
    Newest,
}

/// 検索結果 1 ページの件数
pub const SEARCH_PAGE_SIZE: u32 = 20;

/// `GET /api/search?q=&start=&sort=` のクエリ。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub start: u32,
    #[serde(default)]
    pub sort: SearchSort,
}

/// 検索結果の 1 件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchItem {
    /// バージョンを除いた論文 ID（例: `1706.03762`）
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    /// アブストラクト（空白をまとめたもの）
    pub summary: String,
    /// 初版の投稿日（YYYY-MM-DD）
    pub published: String,
    /// 主分類（例: `cs.CL`）
    pub primary_category: String,
}

/// `GET /api/search` の応答。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchResponse {
    pub total: u64,
    pub start: u32,
    pub items: Vec<SearchItem>,
}

/// 入力欄の文字列が論文の URL か arXiv ID なら、`/api/paper` に渡す URL を返す。
/// それ以外（キーワード）なら `None`（検索する）。
pub fn paper_url_from_input(input: &str) -> Option<String> {
    let s = input.trim();
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("arxiv.org/")
    {
        return Some(if lower.starts_with("arxiv.org/") {
            format!("https://{s}")
        } else {
            s.to_string()
        });
    }
    let id = if lower.starts_with("arxiv:") {
        &s[6..]
    } else {
        s
    };
    is_arxiv_id(id).then(|| format!("https://arxiv.org/abs/{id}"))
}

/// `1706.03762`, `2301.12345v2`, `hep-th/9901001`, `math.AG/0601001v1` のような arXiv ID か。
pub fn is_arxiv_id(s: &str) -> bool {
    let (base, _versioned) = match s.rsplit_once('v') {
        Some((b, v)) if !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) => (b, true),
        _ => (s, false),
    };
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit());
    if let Some((yymm, num)) = base.split_once('.')
        && yymm.len() == 4
        && digits(yymm)
        && (num.len() == 4 || num.len() == 5)
        && digits(num)
    {
        return true;
    }
    if let Some((archive, num)) = base.split_once('/') {
        let archive_ok = !archive.is_empty()
            && archive
                .bytes()
                .all(|c| c.is_ascii_alphabetic() || c == b'-' || c == b'.');
        return archive_ok && num.len() == 7 && digits(num);
    }
    false
}

/// フィードバックに添える、アプリの状態（「状況を添付する」をオンにしたときだけ Issue に載る）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackContext {
    #[serde(default)]
    pub paper_id: Option<String>,
    #[serde(default)]
    pub paper_title: Option<String>,
    /// 選択中のテキスト
    #[serde(default)]
    pub selection: Option<String>,
    /// 表示中のタブ
    #[serde(default)]
    pub tab: String,
    /// ブラウザの User-Agent
    #[serde(default)]
    pub user_agent: String,
}

/// `POST /api/feedback/preview` の body（LLM がラベルとタイトルを付ける前の下書き）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackDraft {
    /// 空なら LLM が付ける
    #[serde(default)]
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub include_context: bool,
    #[serde(default)]
    pub context: FeedbackContext,
}

/// `POST /api/feedback/preview` の応答。この内容で Issue を作る。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackPreview {
    pub title: String,
    /// LLM が選んだラベル（from-app などの固定のものを含む）
    pub labels: Vec<String>,
    /// Issue の本文（Markdown）
    pub issue_body: String,
    /// ラベルを LLM が付けたか（失敗したら needs-triage だけ）
    pub llm_labeled: bool,
}

/// `POST /api/feedback` の body（プレビューを確認・修正したもの）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackSubmit {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    #[serde(default)]
    pub include_context: bool,
    #[serde(default)]
    pub context: FeedbackContext,
}

/// 送ったフィードバック 1 件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackItem {
    pub id: i64,
    pub created_at: i64,
    pub title: String,
    pub labels: Vec<String>,
    /// "created"（Issue を作った）か "pending"（未送信。GitHub 未設定や送信失敗）
    pub status: String,
    #[serde(default)]
    pub issue_url: Option<String>,
    #[serde(default)]
    pub issue_number: Option<i64>,
    /// 最後に送信に失敗した理由
    #[serde(default)]
    pub error: Option<String>,
}

/// ラベルの定義（表示用）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackLabel {
    pub name: String,
    /// 16 進の色（# なし）
    pub color: String,
    pub description: String,
    /// "type"（種類）, "area"（機能の場所）, "meta"（自動で付くもの）
    pub kind: String,
}

/// `GET /api/feedback` の応答。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackListResponse {
    /// GitHub への送信が設定されているか（GITHUB_REPO と GITHUB_TOKEN）
    pub configured: bool,
    /// 送信先（owner/name）
    #[serde(default)]
    pub repo: Option<String>,
    pub labels: Vec<FeedbackLabel>,
    /// 新しい順
    pub items: Vec<FeedbackItem>,
}

/// フィードバック本文の上限（文字数）
pub const MAX_FEEDBACK_BODY_CHARS: usize = 4_000;
/// フィードバックのタイトルの上限（文字数）
pub const MAX_FEEDBACK_TITLE_CHARS: usize = 120;

/// 講座の範囲外として扱うカテゴリ（論文に多い構文）
pub const OUT_OF_COURSE_CATEGORY: &str = "論文構文";

/// 講座のレッスン（タイトルはユーザーが書き写したもの）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LessonRef {
    pub id: String,
    pub category: String,
    pub title: String,
}

/// 解説した英文で検出した文法項目（SSE の tags イベント）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExplainTag {
    pub tag_id: String,
    pub name: String,
    pub category: String,
    /// 英文中で該当する部分
    pub span: String,
    /// 講座の範囲内か（範囲内ならレッスンかカテゴリ単位の案内を出す）
    pub in_scope: bool,
    /// 対応するレッスン（未記入なら空）
    pub lessons: Vec<LessonRef>,
    /// タグの説明（grammar_tags.yaml の hint）
    #[serde(default)]
    pub hint: String,
}

/// 文法タグ 1 件（`GET /api/grammar-map`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrammarTagInfo {
    pub id: String,
    pub name: String,
    pub category: String,
    pub aliases: Vec<String>,
    pub hint: String,
    pub in_scope: bool,
    pub lessons: Vec<LessonRef>,
    /// 履歴に出てきた回数（その文法項目を含む英文の数）
    pub count: u64,
}

/// カテゴリ 1 件（`GET /api/grammar-map`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrammarCategoryInfo {
    pub name: String,
    /// 講座のカテゴリか（論文構文は false）
    pub in_course: bool,
    /// 記入済みのレッスン数
    pub lesson_count: usize,
}

/// `GET /api/grammar-map` の応答。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrammarMapResponse {
    /// 講座名
    pub course: String,
    /// 講座の短い名前（チップに出す）
    pub course_short: String,
    /// レッスン対応を読んだファイル（"local" か "template"）
    pub source: String,
    pub categories: Vec<GrammarCategoryInfo>,
    pub tags: Vec<GrammarTagInfo>,
}

/// 履歴の 1 件（調べた単語・解説した文）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: i64,
    pub paper_id: String,
    pub paper_title: String,
    pub mode: Mode,
    pub text: String,
    /// 解説に使った文脈（切り詰め済み）
    pub context: String,
    /// 最後に調べた時刻（UNIX 秒）
    pub updated_at: i64,
    /// 現在のモデルで解説がキャッシュされているか
    pub has_explanation: bool,
    /// 英文で検出した文法タグの id
    #[serde(default)]
    pub tags: Vec<String>,
}

/// `POST /api/history` の body。同じ論文・mode・text なら時刻と文脈を更新する。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryRecord {
    pub paper_id: String,
    pub paper_title: String,
    pub mode: Mode,
    pub text: String,
    #[serde(default)]
    pub context: String,
    /// 英文で検出した文法タグの id。空なら既存の値を残す
    #[serde(default)]
    pub tags: Vec<String>,
}

/// `GET /api/history?paper=` のクエリ。`paper` が無ければ全論文。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub paper: Option<String>,
}

/// `GET /api/history` の応答（新しい順）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryResponse {
    pub entries: Vec<HistoryEntry>,
}

/// `/api/explain` の SSE イベント名。
pub mod sse_event {
    /// 解説の断片。data は追記する文字列
    pub const TOKEN: &str = "token";
    /// 正常終了
    pub const DONE: &str = "done";
    /// 失敗。data はエラーメッセージ
    pub const ERROR: &str = "error";
    /// 他の解説の生成待ち
    pub const QUEUED: &str = "queued";
    /// 生成開始（順番待ちが終わった）
    pub const STARTED: &str = "started";
    /// キャッシュから返す（このあと tags（英文のみ）と token と done が続く）
    pub const CACHED: &str = "cached";
    /// 英文で使われている文法項目。data は `Vec<ExplainTag>` の JSON（mode=sentence のときだけ、解説の前に送る）
    pub const TAGS: &str = "tags";
}

/// SSE の 1 イベント。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` 行（無ければ "message"）
    pub event: String,
    /// `data:` 行を改行でつないだもの
    pub data: String,
}

/// 受信したバイト列（文字列化済み）を少しずつ渡すと、完成したイベントを返す SSE パーサー。
/// `\n`, `\r\n`, `\r` の改行に対応する。コメント行（`:` 始まり）は無視する。
#[derive(Debug, Default)]
pub struct SseParser {
    buf: String,
    event: Option<String>,
    data: Vec<String>,
    /// 直前のチャンクが `\r` で終わったか（次の `\n` を読み飛ばす）
    pending_cr: bool,
}

impl SseParser {
    pub fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        let mut chunk = chunk;
        if self.pending_cr {
            chunk = chunk.strip_prefix('\n').unwrap_or(chunk);
            self.pending_cr = false;
        }
        self.buf.push_str(chunk);

        let mut out = Vec::new();
        while let Some(pos) = self.buf.find(['\n', '\r']) {
            let line: String = self.buf[..pos].to_string();
            let is_cr = self.buf.as_bytes()[pos] == b'\r';
            let mut consumed = pos + 1;
            if is_cr {
                if self.buf.len() > consumed {
                    if self.buf.as_bytes()[consumed] == b'\n' {
                        consumed += 1;
                    }
                } else {
                    self.pending_cr = true;
                }
            }
            self.buf.drain(..consumed);
            if let Some(ev) = self.line(&line) {
                out.push(ev);
            }
        }
        out
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            if self.data.is_empty() && self.event.is_none() {
                return None;
            }
            let ev = SseEvent {
                event: self.event.take().unwrap_or_else(|| "message".into()),
                data: std::mem::take(&mut self.data).join("\n"),
            };
            return Some(ev);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        None
    }
}

/// 文脈が `max_chars` を超えるとき、選択文字列の位置を中心に切り詰める（前後に "…" を付ける）。
pub fn clip_context(context: &str, text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = context.chars().collect();
    if chars.len() <= max_chars {
        return context.to_string();
    }
    let budget = max_chars.saturating_sub(2);
    let center = context
        .find(text)
        .map(|byte| context[..byte].chars().count() + text.chars().count() / 2)
        .unwrap_or(0);
    let start = center
        .saturating_sub(budget / 2)
        .min(chars.len().saturating_sub(budget));
    let end = (start + budget).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
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

    fn feed(p: &mut SseParser, chunks: &[&str]) -> Vec<SseEvent> {
        chunks.iter().flat_map(|c| p.push(c)).collect()
    }

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
        }
    }

    #[test]
    fn sse_basic_and_split_chunks() {
        let mut p = SseParser::default();
        let got = feed(
            &mut p,
            &[
                "event: tok",
                "en\ndata: こん",
                "にちは\n",
                "\nevent: done\ndata:\n\n",
            ],
        );
        assert_eq!(got, vec![ev("token", "こんにちは"), ev("done", "")]);
    }

    #[test]
    fn sse_multiline_data_comments_crlf() {
        let mut p = SseParser::default();
        let got = feed(
            &mut p,
            &[
                ": keep-alive\r\n\r\n",
                "data: a\r",
                "\ndata: b\r\n\r\n",
                "data:  x\r\r",
            ],
        );
        assert_eq!(got, vec![ev("message", "a\nb"), ev("message", " x")]);
    }

    #[test]
    fn sse_incomplete_event_is_held() {
        let mut p = SseParser::default();
        assert!(p.push("event: token\ndata: a").is_empty());
        assert_eq!(p.push("\n\n"), vec![ev("token", "a")]);
    }

    #[test]
    fn clip() {
        assert_eq!(clip_context("short", "s", 10), "short");
        let ctx = format!("{}TARGET{}", "a".repeat(100), "b".repeat(100));
        let clipped = clip_context(&ctx, "TARGET", 22);
        assert_eq!(clipped.chars().count(), 22);
        assert!(clipped.contains("TARGET"), "{clipped}");
        assert!(clipped.starts_with('…') && clipped.ends_with('…'));
        // 見つからなければ先頭から
        let clipped = clip_context(&"x".repeat(50), "zzz", 10);
        assert!(clipped.starts_with("xxxx") && clipped.ends_with('…'));
        assert_eq!(clipped.chars().count(), 9);
    }

    #[test]
    fn explain_request_defaults() {
        let r: ExplainRequest = serde_json::from_str(r#"{"mode":"word","text":"x"}"#).unwrap();
        assert_eq!(r.context, "");
        assert!(!r.regenerate);
    }

    #[test]
    fn paper_input() {
        let u = |s: &str| paper_url_from_input(s);
        assert_eq!(
            u(" https://arxiv.org/abs/1706.03762 ").as_deref(),
            Some("https://arxiv.org/abs/1706.03762")
        );
        assert_eq!(
            u("arxiv.org/pdf/1706.03762").as_deref(),
            Some("https://arxiv.org/pdf/1706.03762")
        );
        assert_eq!(
            u("1706.03762").as_deref(),
            Some("https://arxiv.org/abs/1706.03762")
        );
        assert_eq!(
            u("arXiv:2301.12345v2").as_deref(),
            Some("https://arxiv.org/abs/2301.12345v2")
        );
        assert_eq!(
            u("hep-th/9901001").as_deref(),
            Some("https://arxiv.org/abs/hep-th/9901001")
        );
        // キーワードは検索
        assert_eq!(u("attention is all you need"), None);
        assert_eq!(u("transformer"), None);
        assert_eq!(u("1706"), None);
        assert_eq!(u("12345.6789"), None);
        assert_eq!(u("vision/transformer"), None);
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_whitespace("  a \n\t b  c "), "a b c");
    }
}
