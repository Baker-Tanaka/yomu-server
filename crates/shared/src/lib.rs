//! server と web の両方で使う API の型。

mod cite;
mod formula;
mod mathtex;
mod palette;
mod pos;
mod slash;
mod word;

pub use cite::{cited_arxiv_id, cited_title, title_search_query};
pub use formula::{
    clean_tex, inline_tex, normalize_math_delimiters, readable_math, speakable, tex_to_text,
};
pub use mathtex::{AccentKind, AtomClass, MathNode, TableAlign, parse_math};
pub use palette::palette_matches;
pub use pos::{PartOfSpeech, guess_pos, pos_groups, pos_summary};
pub use slash::{SlashChunk, SlashReading, parse_slash_reading};
pub use word::word_at;

use serde::{Deserialize, Serialize};

/// アプリのバージョン（workspace の Cargo.toml の version）。
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// ビルドしたコミットの短い Git ハッシュ（分からなければ空文字）。build.rs が埋め込む。
pub const GIT_HASH: &str = env!("PAPER_READER_GIT_HASH");
/// ビルドしたコミットの日付（YYYY-MM-DD。分からなければ空文字）。
pub const GIT_DATE: &str = env!("PAPER_READER_GIT_DATE");

/// 表示用のバージョン文字列（例: `0.1.0 (535ee91, 2026-11-24)`）。
pub fn version_label(version: &str, git_hash: &str, git_date: &str) -> String {
    let hash = git_hash.trim();
    let date = git_date.trim();
    match (hash.is_empty(), date.is_empty()) {
        (true, _) => format!("{version} (コミット不明)"),
        (false, true) => format!("{version} ({hash})"),
        (false, false) => format!("{version} ({hash}, {date})"),
    }
}

/// このビルドのバージョン文字列。
pub fn build_version_label() -> String {
    version_label(APP_VERSION, GIT_HASH, GIT_DATE)
}

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
    /// この論文に貼った付箋（版違いでも同じ論文として扱う）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<Bookmark>,
}

/// 付箋（どこまで読んだか）。本文のブロック要素（段落・見出し・リスト項目など）の通し番号と、
/// その中の文字の位置（その文字がある行に付ける）、ブロックの書き出し（版が変わって番号がずれたときに
/// 探し直す手がかり）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bookmark {
    pub block: u32,
    /// ブロックの本文の先頭から数えた文字の位置（UTF-16）
    #[serde(default)]
    pub offset: u32,
    pub snippet: String,
    /// 貼った時刻（UNIX 秒）
    pub updated_at: i64,
}

/// `PUT /api/bookmark` の本文。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BookmarkPut {
    pub paper_id: String,
    pub block: u32,
    #[serde(default)]
    pub offset: u32,
    pub snippet: String,
}

/// `DELETE /api/bookmark?paper_id=` のクエリ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookmarkQuery {
    pub paper_id: String,
}

/// 付箋に残す書き出しの最大文字数
pub const MAX_BOOKMARK_SNIPPET_CHARS: usize = 80;

/// 「後で読む」に残せる論文の数の上限
pub const MAX_READ_LATER: i64 = 200;

/// 後で読む論文 1 件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadLaterItem {
    /// 版なしの論文 ID
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    /// 積んだ時刻（UNIX 秒）
    pub added_at: i64,
}

/// `GET /api/read-later` の応答（後から積んだ順）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadLaterResponse {
    pub items: Vec<ReadLaterItem>,
}

/// `PUT /api/read-later` の本文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadLaterPut {
    pub paper_id: String,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
}

/// `DELETE /api/read-later?paper_id=` のクエリ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadLaterQuery {
    pub paper_id: String,
}

/// 見た論文 1 件（`GET /api/papers`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentPaper {
    pub id: String,
    pub title: String,
    pub source_url: String,
    /// 最後に開いた時刻（UNIX 秒）
    pub last_opened_at: i64,
    /// 開いた回数
    pub open_count: i64,
    /// この論文で調べた単語・解説した英文の数（履歴から）
    pub word_count: i64,
    pub sentence_count: i64,
    /// 本文が Mac にキャッシュされているか（されていれば arXiv にアクセスせずに開ける）
    pub cached: bool,
    /// 付箋を貼っているか
    #[serde(default)]
    pub bookmarked: bool,
}

/// 論文キャッシュの使用量。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaperCacheStats {
    pub papers: i64,
    pub html_bytes: i64,
    /// 図（画像）の数と大きさ
    pub assets: i64,
    pub asset_bytes: i64,
}

/// `GET /api/papers` の応答（最後に開いた順）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentPapersResponse {
    pub papers: Vec<RecentPaper>,
    pub cache: PaperCacheStats,
}

/// `DELETE /api/papers?id=&purge=` のクエリ。purge なら本文と図のキャッシュも消す。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletePaperQuery {
    pub id: String,
    #[serde(default)]
    pub purge: bool,
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
    /// サーバーのバージョン（Cargo.toml の version）
    #[serde(default)]
    pub version: String,
    /// サーバーをビルドしたコミットの短い Git ハッシュ（分からなければ空文字）
    #[serde(default)]
    pub git_hash: String,
    /// そのコミットの日付（YYYY-MM-DD。分からなければ空文字）
    #[serde(default)]
    pub git_date: String,
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
    /// 単語帳に保存した情報（カタカナの読み、辞書に無い語の訳、検索回数）。
    /// 見出し語があればその語、無ければ検索語について
    #[serde(default)]
    pub note: Option<WordNote>,
    /// 単語帳の使用量
    #[serde(default)]
    pub store: Option<WordStoreStats>,
}

impl DictResponse {
    /// 単語帳の見出し（辞書にあれば見出し語、無ければ検索語）。
    pub fn note_word(&self) -> &str {
        self.headword.as_deref().unwrap_or(&self.query)
    }

    /// AI に訳を頼むべきか（辞書に無く、保存した訳も無い）。
    pub fn needs_ai_meanings(&self) -> bool {
        self.headword.is_none() && self.note.as_ref().is_none_or(|n| n.meanings.is_empty())
    }

    /// AI に訳・読みを頼むべきか。一度頼んだ語は（結果が空でも）頼み直さない。
    pub fn wants_ai(&self, want_kana: bool) -> bool {
        if !self.query.chars().any(|c| c.is_ascii_alphabetic()) {
            return false;
        }
        if self.note.as_ref().is_some_and(|n| n.ai_checked) {
            return false;
        }
        let no_kana = self.note.as_ref().is_none_or(|n| n.kana.is_none());
        self.needs_ai_meanings() || (want_kana && no_kana)
    }
}

/// 単語帳の情報をだれが書いたか。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum WordSource {
    /// ローカル LLM が推定した
    Llm,
    /// 利用者が登録した
    User,
}

impl WordSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::User => "user",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "llm" => Some(Self::Llm),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// 単語帳の 1 語（検索回数と、辞書を補う情報）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WordNote {
    pub word: String,
    /// カタカナの読み（英語の発音をカタカナで表したもの）
    pub kana: Option<String>,
    pub kana_source: Option<WordSource>,
    /// 辞書に無い語の訳（AI 訳か、自分で登録した訳）
    pub meanings: Vec<String>,
    pub meanings_source: Option<WordSource>,
    /// 辞書で引いた回数
    pub lookup_count: i64,
    /// 最後に引いた時刻（UNIX 秒）
    pub last_looked_at: i64,
    /// AI に訳・読みを問い合わせ済みか
    #[serde(default)]
    pub ai_checked: bool,
    /// ローカル辞書の最初の訳（単語帳の一覧で使う。辞書に無ければ `None`）
    #[serde(default)]
    pub dict_summary: Option<String>,
}

impl WordNote {
    /// 自分で登録した情報があるか（自動では消さない）。
    pub fn is_user(&self) -> bool {
        self.kana_source == Some(WordSource::User) || self.meanings_source == Some(WordSource::User)
    }
}

/// 単語帳の使用量。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WordStoreStats {
    pub count: i64,
    /// 上限（これを超えると、検索回数の少ない語から自動で消す。自分で登録した語は消さない）
    pub max: i64,
}

impl WordStoreStats {
    /// 上限の 9 割を超えた（整理するか確かめる）。
    pub fn near_full(&self) -> bool {
        self.max > 0 && self.count * 10 >= self.max * 9
    }
}

/// `POST /api/dict/ai` の body。辞書に無い語の訳と、カタカナの読みを LLM に推定させて単語帳に保存する。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictAiRequest {
    pub word: String,
    /// 単語を含む段落（訳を文脈に合わせるため。長ければサーバーで切り詰める）
    #[serde(default)]
    pub context: String,
}

/// 単語帳の並び順。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WordSort {
    /// 検索回数の多い順
    #[default]
    Count,
    /// 最近引いた順
    Recent,
    /// アルファベット順
    Alpha,
}

/// `GET /api/words` のクエリ。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WordListQuery {
    /// 見出しの前方一致
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub sort: WordSort,
    #[serde(default)]
    pub offset: i64,
}

/// 単語帳の 1 ページで返す数
pub const WORD_PAGE_SIZE: i64 = 100;

/// `GET /api/words` の応答。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WordListResponse {
    pub items: Vec<WordNote>,
    /// 条件に合う語の数
    pub total: i64,
    pub store: WordStoreStats,
}

/// `POST /api/words` の body（自分で登録・編集する）。空の項目は変えない。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WordUpsert {
    pub word: String,
    #[serde(default)]
    pub kana: String,
    /// 訳（1 行に 1 つ、または「、」「;」区切り）
    #[serde(default)]
    pub meanings: String,
}

/// `POST /api/words/delete` の body。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WordDelete {
    pub words: Vec<String>,
}

/// `POST /api/words/prune` の body。検索回数の少ない順に `count` 語を消す（自分で登録した語は残す）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct WordPrune {
    pub count: i64,
}

/// 削除の結果。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WordDeleteResult {
    pub deleted: i64,
    pub store: WordStoreStats,
}

/// 単語帳の訳の入力を、1 つずつの訳に分ける（改行・「、」「;」「/」区切り）。
pub fn split_user_meanings(s: &str) -> Vec<String> {
    s.split(['\n', '、', ';', '；', '/'])
        .map(|m| normalize_whitespace(m).trim().to_string())
        .filter(|m| !m.is_empty())
        .collect()
}

/// カタカナの読みとして使える文字だけか（カタカナ・長音・中黒・空白）。
pub fn is_kana_reading(s: &str) -> bool {
    !s.trim().is_empty()
        && s.chars()
            .all(|c| matches!(c, '\u{30A1}'..='\u{30FA}' | 'ー' | '・' | ' ' | '\u{3000}'))
}

/// 解説のモード。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Word,
    Sentence,
    /// 数式（`text` は LaTeX）。選び方で決まるので、`detect` はこれを返さない
    Formula,
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
            Mode::Formula => "formula",
        }
    }
}

/// 解説の種類。`Standard` は対象に合わせた解説（単語・英文は英語の解説、数式は数式の解説）、
/// `Academic` は内容（概念・背景・論文の中での役割）の解説で、利用者が選んだときだけ使う。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ExplainStyle {
    #[default]
    Standard,
    Academic,
}

impl ExplainStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            ExplainStyle::Standard => "standard",
            ExplainStyle::Academic => "academic",
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
    /// 英文の解説にスラッシュリーディング（意味のまとまりごとの区切りと直訳）を入れる
    #[serde(default)]
    pub slash: bool,
    /// 解説の種類（省略時は対象に合わせた解説）
    #[serde(default)]
    pub style: ExplainStyle,
}

/// チャットの質問 1 件の上限（文字数）
pub const MAX_CHAT_QUESTION_CHARS: usize = 1_000;
/// チャットの回答 1 件・解説の上限（文字数。LLM に渡す分はサーバーがさらに切り詰める）
pub const MAX_CHAT_ANSWER_CHARS: usize = 12_000;
/// 一度に送るやり取りの上限（件数。質問と回答を 1 件ずつ数える）
pub const MAX_CHAT_MESSAGES: usize = 21;

/// チャットの発言者。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
}

/// チャットの発言 1 件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

/// `POST /api/chat` の body。解説のあとに続けて質問する。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatRequest {
    /// 解説した対象が単語か英文か
    pub mode: Mode,
    /// 解説した対象
    pub text: String,
    #[serde(default)]
    pub context: String,
    /// 表示した解説（Markdown）
    #[serde(default)]
    pub explanation: String,
    /// 表示した解説の種類
    #[serde(default)]
    pub style: ExplainStyle,
    /// これまでのやり取り。質問と回答が交互で、最後が今回の質問
    pub messages: Vec<ChatMessage>,
}

/// 送る件数の上限に収まるよう、古いやり取りから除く（先頭は質問のまま）。
pub fn recent_chat_messages(messages: &[ChatMessage]) -> &[ChatMessage] {
    let mut start = messages.len().saturating_sub(MAX_CHAT_MESSAGES);
    while start < messages.len() && messages[start].role != ChatRole::User {
        start += 1;
    }
    &messages[start..]
}

/// 英文の解説のスラッシュリーディングの見出し。
pub const SLASH_READING_HEADING: &str = "## スラッシュリーディング";

/// Markdown から、`heading` の行から次の「## 」見出しの手前までを取り除く（無ければそのまま）。
/// スラッシュリーディングを表示しない設定のときに使う。
pub fn without_section<'a>(markdown: &'a str, heading: &str) -> std::borrow::Cow<'a, str> {
    let mut lines = markdown.split_inclusive('\n');
    if !markdown
        .split_inclusive('\n')
        .any(|l| l.trim_end() == heading)
    {
        return std::borrow::Cow::Borrowed(markdown);
    }
    let mut out = String::with_capacity(markdown.len());
    let mut skipping = false;
    for line in lines.by_ref() {
        let t = line.trim_end();
        if t == heading {
            skipping = true;
            continue;
        }
        if skipping && t.starts_with("## ") {
            skipping = false;
        }
        if !skipping {
            out.push_str(line);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// 表示できない LaTeX の矢印（LLM が書きがち）を記号に置き換える。
pub fn plain_arrows(markdown: &str) -> std::borrow::Cow<'_, str> {
    const ARROWS: [(&str, &str); 4] = [
        ("$\\rightarrow$", "→"),
        ("$\\to$", "→"),
        ("$\\Rightarrow$", "⇒"),
        ("$\\leftarrow$", "←"),
    ];
    if !markdown.contains('$') {
        return std::borrow::Cow::Borrowed(markdown);
    }
    let mut s = markdown.to_string();
    for (from, to) in ARROWS {
        s = s.replace(from, to);
    }
    std::borrow::Cow::Owned(s)
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
    /// true なら、ラベルとタイトルに加えて本文の文章も LLM が整える（web の既定）
    #[serde(default)]
    pub rewrite: bool,
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
    /// Issue の本文に使った内容（整えたなら整えた文章、そうでなければ書いたまま）
    #[serde(default)]
    pub body: String,
    /// 本文の文章を LLM が整えたか
    #[serde(default)]
    pub rewritten: bool,
    /// 整えたときの、書いたままの文章で組み立てた Issue の本文（元に戻す用。整えていなければ空）
    #[serde(default)]
    pub original_issue_body: String,
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

/// 解説した英文で検出した文法項目（SSE の tags イベント）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExplainTag {
    pub tag_id: String,
    pub name: String,
    pub category: String,
    /// 英文中で該当する部分
    pub span: String,
    /// タグの 1 行説明（grammar_tags.yaml の hint。guide が空のときに表示する）
    #[serde(default)]
    pub hint: String,
    /// 画面に出す解説（grammar_tags.yaml の guide。無ければ空）
    #[serde(default)]
    pub guide: TagGuide,
}

/// 文法項目の解説の例文。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagExample {
    pub en: String,
    pub ja: String,
}

/// 文法項目の解説（TOEIC 対策向け。画面にだけ出し、LLM には渡さない）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TagGuide {
    /// ひとことで言うと
    pub summary: String,
    /// 形
    pub form: String,
    pub examples: Vec<TagExample>,
    /// TOEIC での見分け方・解き方
    pub toeic: String,
    /// 注意（無ければ空）
    pub caution: String,
}

/// 文法タグ 1 件（`GET /api/grammar-tags`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrammarTagInfo {
    pub id: String,
    pub name: String,
    pub category: String,
    pub aliases: Vec<String>,
    pub hint: String,
    #[serde(default)]
    pub guide: TagGuide,
    /// 履歴に出てきた回数（その文法項目を含む英文の数）
    pub count: u64,
}

/// `GET /api/grammar-tags` の応答。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrammarTagsResponse {
    /// カテゴリ名（表示の順番）
    pub categories: Vec<String>,
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
    fn version_labels() {
        assert_eq!(
            version_label("0.1.0", "535ee91", "2026-11-24"),
            "0.1.0 (535ee91, 2026-11-24)"
        );
        assert_eq!(version_label("0.1.0", "535ee91", ""), "0.1.0 (535ee91)");
        assert_eq!(
            version_label("0.1.0", "", "2026-11-24"),
            "0.1.0 (コミット不明)"
        );
        assert_eq!(version_label("0.1.0", " ", " "), "0.1.0 (コミット不明)");
        assert!(build_version_label().starts_with(APP_VERSION));
    }

    #[test]
    fn health_without_version_fields() {
        // 古いサーバーの応答（バージョンの項目なし）も読める
        let h: HealthResponse = serde_json::from_str(r#"{"server":true,"llm":null}"#).unwrap();
        assert_eq!(h.version, "");
        assert_eq!(h.git_hash, "");
    }

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
        assert_eq!(
            serde_json::to_string(&Mode::Formula).unwrap(),
            "\"formula\""
        );
        assert_eq!(Mode::Formula.as_str(), "formula");
    }

    #[test]
    fn explain_style_defaults_to_standard() {
        // 古い web（style を送らない）からの要求は、これまでどおりの解説にする
        let r: ExplainRequest =
            serde_json::from_str(r#"{"mode":"sentence","text":"We propose X."}"#).unwrap();
        assert_eq!(r.style, ExplainStyle::Standard);
        let r: ExplainRequest =
            serde_json::from_str(r#"{"mode":"word","text":"X","style":"academic"}"#).unwrap();
        assert_eq!(r.style, ExplainStyle::Academic);
        assert_eq!(ExplainStyle::Academic.as_str(), "academic");
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
    fn section_removal() {
        let md =
            "## 和訳\n\n訳\n\n## スラッシュリーディング\nA / B\n- A → あ\n\n## 文構造\n- 主語\n";
        assert_eq!(
            without_section(md, SLASH_READING_HEADING),
            "## 和訳\n\n訳\n\n## 文構造\n- 主語\n"
        );
        // 最後の見出しでも消える。無ければそのまま（借用のまま）
        let md2 = "## 和訳\n訳\n## スラッシュリーディング\nA / B";
        assert_eq!(without_section(md2, SLASH_READING_HEADING), "## 和訳\n訳\n");
        assert!(matches!(
            without_section("## 和訳\n訳", SLASH_READING_HEADING),
            std::borrow::Cow::Borrowed(_)
        ));
        // ### の小見出しでは止まらない
        let md3 = "## スラッシュリーディング\nA\n### x\nB\n## 語句\nC";
        assert_eq!(without_section(md3, SLASH_READING_HEADING), "## 語句\nC");
    }

    #[test]
    fn arrows() {
        assert_eq!(plain_arrows("- A $\\rightarrow$ あ"), "- A → あ");
        assert_eq!(plain_arrows("x $\\to$ y $\\Rightarrow$ z"), "x → y ⇒ z");
        assert!(matches!(
            plain_arrows("- A → あ"),
            std::borrow::Cow::Borrowed(_)
        ));
        // 数式そのものは触らない
        assert_eq!(plain_arrows("$x^2$"), "$x^2$");
    }

    #[test]
    fn explain_request_defaults() {
        let r: ExplainRequest = serde_json::from_str(r#"{"mode":"word","text":"x"}"#).unwrap();
        assert_eq!(r.context, "");
        assert!(!r.regenerate);
        assert!(!r.slash);
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

    fn dict(headword: Option<&str>, note: Option<WordNote>) -> DictResponse {
        DictResponse {
            query: "modalities".into(),
            headword: headword.map(str::to_owned),
            meanings: Vec::new(),
            related: Vec::new(),
            note,
            store: None,
        }
    }

    fn note(kana: Option<&str>, meanings: &[&str], ai_checked: bool) -> WordNote {
        WordNote {
            word: "modality".into(),
            kana: kana.map(str::to_owned),
            kana_source: None,
            meanings: meanings.iter().map(|m| m.to_string()).collect(),
            meanings_source: None,
            lookup_count: 1,
            last_looked_at: 0,
            ai_checked,
            dict_summary: None,
        }
    }

    #[test]
    fn wants_ai() {
        // 辞書に無い → 訳を頼む（読みの設定に関係なく）
        assert!(dict(None, None).wants_ai(false));
        assert!(dict(None, Some(note(Some("モダリティ"), &[], false))).wants_ai(false));
        // 辞書にある → 読みが欲しいときだけ
        assert!(!dict(Some("modality"), None).wants_ai(false));
        assert!(dict(Some("modality"), None).wants_ai(true));
        assert!(!dict(Some("modality"), Some(note(Some("モダリティ"), &[], false))).wants_ai(true));
        // 保存した訳がある・問い合わせ済みなら頼まない
        assert!(!dict(None, Some(note(Some("モダリティ"), &["様式"], false))).wants_ai(true));
        assert!(!dict(None, Some(note(None, &[], true))).wants_ai(true));
        // 英字を含まない語は頼まない
        let mut d = dict(None, None);
        d.query = "2024".into();
        assert!(!d.wants_ai(true));
        assert_eq!(dict(None, None).note_word(), "modalities");
        assert_eq!(dict(Some("modality"), None).note_word(), "modality");
    }

    #[test]
    fn store_stats() {
        assert!(!WordStoreStats { count: 0, max: 0 }.near_full());
        assert!(
            !WordStoreStats {
                count: 89,
                max: 100
            }
            .near_full()
        );
        assert!(
            WordStoreStats {
                count: 90,
                max: 100
            }
            .near_full()
        );
    }

    #[test]
    fn user_meanings() {
        assert_eq!(
            split_user_meanings("様式、 様相\n（論文）モダリティ; /"),
            vec!["様式", "様相", "（論文）モダリティ"]
        );
        assert!(split_user_meanings("  \n、").is_empty());
    }

    #[test]
    fn kana_reading() {
        assert!(is_kana_reading("レレヴァント"));
        assert!(is_kana_reading("ステート・オブ・ジ・アート"));
        assert!(!is_kana_reading("relevant"));
        assert!(!is_kana_reading("れればんと"));
        assert!(!is_kana_reading(" "));
    }
}
