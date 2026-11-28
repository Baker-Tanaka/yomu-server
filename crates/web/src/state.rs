//! 右ペインの状態と、DOM のイベント・非同期処理から状態を更新する処理。

use std::cell::RefCell;
use std::rc::Rc;

use eframe::egui;
use paper_reader_shared::{
    ChatMessage, ChatRequest, ChatRole, DictResponse, ExplainRequest, ExplainStyle, ExplainTag,
    FeedbackContext, FeedbackDraft, FeedbackItem, FeedbackListResponse, FeedbackPreview,
    FeedbackSubmit, GrammarTagsResponse, HealthResponse, HistoryEntry, HistoryRecord, KnowledgeAdd,
    KnowledgeListResponse, KnowledgeRef, MAX_CHAT_ANSWER_CHARS, MAX_CHAT_QUESTION_CHARS,
    MAX_CONTEXT_CHARS, MAX_TEXT_CHARS, Mode, ReadLaterItem, SEARCH_PAGE_SIZE, SearchItem,
    SearchSort, WordListResponse, WordNote, WordSort, WordStoreStats, WordUpsert, clip_context,
    paper_url_from_input, recent_chat_messages, sse_event,
};
use web_sys::AbortController;

use crate::dom::{self, SelectionInfo};

pub type SharedRef = Rc<RefCell<Shared>>;

/// 論文の読み込み状態。
#[derive(Debug, Clone, Default)]
pub enum PaperStatus {
    #[default]
    Empty,
    Loading,
    Loaded {
        id: String,
        title: String,
        source_url: String,
    },
    Error(String),
}

/// 辞書の検索状態。
#[derive(Debug, Clone, Default)]
pub enum DictStatus {
    /// 単語が選択されていない
    #[default]
    Idle,
    Loading,
    Loaded(DictResponse),
    Error(String),
}

/// 単語の AI 訳・カタカナの読みの問い合わせ状態。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DictAiPhase {
    #[default]
    Idle,
    Loading,
    Error(String),
}

/// 単語帳の一覧の状態。
#[derive(Debug, Clone, Default)]
pub enum WordsStatus {
    #[default]
    NotLoaded,
    Loading,
    Loaded(WordListResponse),
    Error(String),
}

/// 単語帳タブの状態。
#[derive(Debug, Clone, Default)]
pub struct Words {
    pub status: WordsStatus,
    /// 次に単語帳タブを表示するときに読み直す
    pub stale: bool,
    /// 見出しの前方一致で絞り込む
    pub query: String,
    pub sort: WordSort,
    /// 削除するために選んだ語
    pub selected: std::collections::BTreeSet<String>,
    /// 追加・編集の入力欄
    pub form_open: bool,
    pub form_word: String,
    pub form_kana: String,
    pub form_meanings: String,
    /// 操作の結果（成功なら true）
    pub message: Option<(bool, String)>,
    pub busy: bool,
    /// 続きを読み込み中
    pub loading_more: bool,
    /// 「検索回数の少ない語を削除」の語数
    pub prune_count: i64,
    seq: u64,
}

/// 解説の進み具合。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ExplainPhase {
    #[default]
    Idle,
    /// サーバーへ接続中、または最初の文字を待っている
    Connecting,
    /// 他の解説の生成待ち
    Queued,
    Streaming,
    Done,
    Error(String),
}

impl ExplainPhase {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Connecting | Self::Queued | Self::Streaming)
    }
}

/// 解説のあとのチャット（続けて質問する）。対象の解説と一緒に消え、保存はしない。
#[derive(Default)]
pub struct Chat {
    pub open: bool,
    /// 質問と回答が交互。回答待ち・失敗のあいだは最後が質問
    pub turns: Vec<ChatMessage>,
    /// 生成中の回答（失敗したときは受信できた分）
    pub answer: String,
    pub phase: ExplainPhase,
    /// 次の描画で最後の質問が上に来るようにスクロールする（質問がまだ無ければチャットを見せる）
    pub scroll_to_question: bool,
    /// 次の描画で質問欄にフォーカスを移す
    pub focus_input: bool,
    /// 質問欄に戻す文字（回答が届く前に止めた質問）
    pub restore_input: Option<String>,
    /// 質問ごとに増える番号（中止した回答の遅れて届いたイベントを捨てる）
    seq: u64,
    controller: Option<AbortController>,
}

impl Chat {
    fn abort(&mut self) {
        if let Some(c) = self.controller.take() {
            c.abort();
        }
        self.seq += 1;
    }

    fn reset(&mut self) {
        self.abort();
        *self = Self {
            seq: self.seq,
            ..Self::default()
        };
    }

    /// 回答を確定する。
    fn finish(&mut self) {
        self.controller = None;
        if self.answer.trim().is_empty() {
            self.phase = ExplainPhase::Error("回答が空でした".into());
            return;
        }
        self.turns.push(ChatMessage {
            role: ChatRole::Assistant,
            content: std::mem::take(&mut self.answer),
        });
        self.phase = ExplainPhase::Idle;
    }
}

#[derive(Default)]
pub struct Explain {
    pub phase: ExplainPhase,
    /// 受信済みの Markdown
    pub markdown: String,
    /// 解説の対象（表示用）
    pub target: Option<(Mode, String)>,
    /// 解説の種類。新しく選択したら対象に合わせた解説（英語・数式）に戻る
    pub style: ExplainStyle,
    /// サーバーのキャッシュから返された解説か
    pub from_cache: bool,
    /// 英文で検出した文法項目（tags イベント）
    pub tags: Vec<ExplainTag>,
    /// tags イベントを受け取ったか（英文のとき、受け取るまでは「判定中」）
    pub tags_received: bool,
    /// クリックで詳しく表示している文法項目
    pub selected_tag: Option<usize>,
    /// 選んだ文法項目を本文で見つけてハイライトできたか
    pub highlight_found: bool,
    /// 解説に添えた知識（knowledge イベント）
    pub knowledge: Vec<KnowledgeRef>,
    /// この解説を知識に登録する操作の状態
    pub knowledge_add: KnowledgeAddPhase,
    pub chat: Chat,
    /// 解説ごとに増える番号。中止した解説から遅れて届いたイベントを捨てる
    seq: u64,
    controller: Option<AbortController>,
}

impl Explain {
    /// 進行中の解説を中止する。
    fn abort(&mut self) {
        if let Some(c) = self.controller.take() {
            c.abort();
        }
        self.seq += 1;
    }

    /// 中止して表示も空にする。
    fn reset(&mut self) {
        self.abort();
        self.phase = ExplainPhase::Idle;
        self.markdown.clear();
        self.target = None;
        self.style = ExplainStyle::Standard;
        self.from_cache = false;
        self.clear_tags();
        self.knowledge.clear();
        self.knowledge_add = KnowledgeAddPhase::Idle;
        self.chat.reset();
    }

    fn clear_tags(&mut self) {
        self.tags.clear();
        self.tags_received = false;
        self.selected_tag = None;
        self.highlight_found = false;
        dom::clear_highlight();
    }
}

/// 解説を知識に登録する操作の状態。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum KnowledgeAddPhase {
    #[default]
    Idle,
    Adding,
    /// 登録した用語
    Added(String),
    Error(String),
}

/// 知識（RAG）の一覧（設定画面）。
#[derive(Default)]
pub struct Knowledge {
    pub list: Option<KnowledgeListResponse>,
    pub loading: bool,
    pub error: Option<String>,
    /// 次に設定画面を開いたときに読み直す
    pub stale: bool,
}

/// 文法項目タブのデータの状態。
#[derive(Debug, Clone, Default)]
pub enum GrammarStatus {
    #[default]
    NotLoaded,
    Loading,
    Loaded(GrammarTagsResponse),
    Error(String),
}

/// 文法項目タブの並び順。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GrammarOrder {
    #[default]
    Category,
    Frequency,
}

/// フィードバック（不具合・要望を GitHub の Issue にする）の段階。
#[derive(Debug, Clone, Default)]
pub enum FeedbackStage {
    /// 書いている
    #[default]
    Edit,
    /// LLM がラベルとタイトルを付けている
    Labeling,
    /// 送る内容を確認している
    Review(FeedbackPreview),
    Sending,
    /// 保存した（Issue を作ったか、未送信）
    Done(FeedbackItem),
}

/// フィードバックの画面の状態。
#[derive(Debug, Clone)]
pub struct Feedback {
    pub open: bool,
    pub stage: FeedbackStage,
    pub title: String,
    pub body: String,
    /// 開いている論文・選択中のテキスト・ブラウザの種類を添付する
    pub include_context: bool,
    /// 確認画面で選んでいるラベル
    pub labels: Vec<String>,
    /// AI 補完で本文の文章も整える（既定）。false ならラベルとタイトルだけ
    pub rewrite: bool,
    /// AI が整える前の、書いたままの本文（確認画面で元に戻す用）
    pub original_body: String,
    /// 確認画面で、整えた文章ではなく書いたままの文章を送る
    pub use_original: bool,
    pub error: Option<String>,
    /// 送信先の設定と、送ったフィードバックの一覧
    pub list: Option<FeedbackListResponse>,
}

impl Default for Feedback {
    fn default() -> Self {
        Self {
            open: false,
            stage: FeedbackStage::Edit,
            title: String::new(),
            body: String::new(),
            include_context: true,
            labels: Vec::new(),
            rewrite: true,
            original_body: String::new(),
            use_original: false,
            error: None,
            list: None,
        }
    }
}

/// 「カタカナの読みを表示する」設定の localStorage のキー
const SHOW_KANA_KEY: &str = "paper-reader.show-kana";
/// スラッシュリーディングを表示するかの設定を保存する localStorage のキー
const SHOW_SLASH_KEY: &str = "paper-reader.show-slash";

/// 履歴の読み込み状態。
#[derive(Debug, Clone, Default)]
pub enum HistoryStatus {
    #[default]
    NotLoaded,
    Loading,
    Loaded(Vec<HistoryEntry>),
    Error(String),
}

/// 右ペインのタブ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Explain,
    Search,
    History,
    Grammar,
    Words,
}

/// 論文検索の状態。
#[derive(Debug, Clone, Default)]
pub struct Search {
    /// 検索した語（結果が表示されているもの）
    pub query: String,
    pub sort: SearchSort,
    pub items: Vec<SearchItem>,
    pub total: u64,
    pub loading: bool,
    pub error: Option<String>,
    /// 検索ごとに増える番号（古い応答を捨てる）
    seq: u64,
}

impl Search {
    pub fn has_more(&self) -> bool {
        (self.items.len() as u64) < self.total && self.items.len() < 1_000
    }
}

/// 履歴タブで表示する範囲。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HistoryScope {
    /// 見た論文の一覧
    #[default]
    Papers,
    /// 表示中の論文の単語・英文
    ThisPaper,
    /// すべての論文の単語・英文
    AllPapers,
}

/// 見た論文の一覧の状態。
#[derive(Debug, Clone, Default)]
pub enum PapersStatus {
    #[default]
    NotLoaded,
    Loading,
    Loaded(paper_reader_shared::RecentPapersResponse),
    Error(String),
}

/// 検索タブの中の切り替え。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchView {
    /// 検索結果
    #[default]
    Results,
    /// 後で読む論文
    Later,
}

/// 後で読む論文。保存の正はサーバーで、操作は先に画面へ反映してから読み直して合わせる。
#[derive(Debug, Clone, Default)]
pub struct ReadLater {
    /// 後から積んだ順
    pub items: Vec<ReadLaterItem>,
    /// 一度でも読み込めた
    pub loaded: bool,
    loading: bool,
    /// 次に表示するときに読み直す
    pub stale: bool,
    /// 操作ごとに増える番号（操作の前に始めた読み込みの結果を捨てる）
    seq: u64,
    /// 読み込みの失敗
    pub load_error: Option<String>,
    /// 追加・削除の失敗
    pub error: Option<String>,
}

impl ReadLater {
    pub fn contains(&self, id: &str) -> bool {
        self.items.iter().any(|i| i.id == id)
    }
}

/// LLM の疎通状態。
#[derive(Debug, Clone, Default)]
pub enum HealthStatus {
    #[default]
    Checking,
    Ok(HealthResponse),
    Error(String),
}

/// DOM のイベントや非同期処理と共有する状態。
#[derive(Default)]
pub struct Shared {
    pub tab: Tab,
    pub search: Search,
    pub paper: PaperStatus,
    pub selection: Option<SelectionInfo>,
    pub dict: DictStatus,
    /// 単語の AI 訳・読みの問い合わせ
    pub dict_ai: DictAiPhase,
    dict_ai_controller: Option<AbortController>,
    /// 設定: カタカナの読み（AI の推定）を表示する
    pub show_kana: bool,
    /// 直近に引いた単語のメモ（ブラウザにだけ保存する）
    pub recent: crate::recent::RecentWords,
    /// 設定: 直近の単語のメモを表示する
    pub show_recent: bool,
    /// 設定: 英文の解説にスラッシュリーディングを付けて表示する
    pub show_slash: bool,
    /// 実験モード: 端末の LLM（WebGPU）
    pub local: crate::local_llm::LocalLlm,
    pub words: Words,
    /// 単語帳が上限に近い（確かめる画面を出す）
    pub store_warning: Option<WordStoreStats>,
    /// このページを開いている間は、上限に近い知らせをもう出さない
    store_warning_dismissed: bool,
    pub explain: Explain,
    pub health: HealthStatus,
    /// 選択を確定したら自動で解説する（既定は手動）
    pub auto_explain: bool,
    /// 選択ごとに増える番号。古い選択に対する辞書の応答を捨てるために使う
    selection_seq: u64,
    pub history: HistoryStatus,
    pub history_scope: HistoryScope,
    /// 履歴が変わったので、次に履歴タブを表示するときに読み直す
    pub history_stale: bool,
    /// 履歴の読み込みごとに増える番号（古い応答を捨てる）
    history_seq: u64,
    pub papers: PapersStatus,
    pub read_later: ReadLater,
    pub search_view: SearchView,
    /// コマンドパレットで選んだ配色（次の描画で画面に反映する）
    pub requested_theme: Option<dom::ThemeChoice>,
    /// 表示中の論文の付箋
    pub bookmark: Option<paper_reader_shared::Bookmark>,
    /// 付箋の操作の失敗・注意
    pub bookmark_error: Option<String>,
    /// 見た論文の一覧を読み直す
    pub papers_stale: bool,
    pub grammar: GrammarStatus,
    pub grammar_order: GrammarOrder,
    /// 出現回数などが変わったので、次に文法項目タブを表示するときに読み直す
    pub grammar_stale: bool,
    pub feedback: Feedback,
    pub knowledge: Knowledge,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            show_kana: dom::load_bool(SHOW_KANA_KEY, true),
            recent: crate::recent::RecentWords::load(),
            show_recent: dom::load_bool(crate::recent::SHOW_KEY, true),
            show_slash: dom::load_bool(SHOW_SLASH_KEY, true),
            local: crate::local_llm::LocalLlm::load_settings(),
            words: Words {
                stale: true,
                prune_count: 1_000,
                ..Words::default()
            },
            grammar_stale: true,
            papers_stale: true,
            knowledge: Knowledge {
                stale: true,
                ..Knowledge::default()
            },
            read_later: ReadLater {
                stale: true,
                ..ReadLater::default()
            },
            ..Self::default()
        }
    }

    pub fn set_show_slash(&mut self, on: bool) {
        self.show_slash = on;
        dom::save_bool(SHOW_SLASH_KEY, on);
    }

    pub fn set_show_kana(&mut self, on: bool) {
        self.show_kana = on;
        dom::save_bool(SHOW_KANA_KEY, on);
    }

    pub fn set_show_recent(&mut self, on: bool) {
        self.show_recent = on;
        dom::save_bool(crate::recent::SHOW_KEY, on);
    }

    /// 進行中の AI 訳の問い合わせを中止する。
    fn abort_dict_ai(&mut self) {
        if let Some(c) = self.dict_ai_controller.take() {
            c.abort();
        }
        self.dict_ai = DictAiPhase::Idle;
    }

    /// 辞書の結果を反映する。単語帳が上限に近ければ知らせる。
    fn set_dict(&mut self, r: DictResponse) {
        if let Some(store) = r.store {
            if store.near_full() && !self.store_warning_dismissed {
                self.store_warning = Some(store);
            }
            if let WordsStatus::Loaded(list) = &mut self.words.status {
                list.store = store;
            }
        }
        self.words.stale = true;
        self.recent.record(&r);
        self.dict = DictStatus::Loaded(r);
    }

    /// 表示中の論文の (ID, タイトル)。
    pub fn paper_ref(&self) -> Option<(String, String)> {
        match &self.paper {
            PaperStatus::Loaded { id, title, .. } => Some((id.clone(), title.clone())),
            _ => None,
        }
    }
}

/// 本文での新しい選択を反映する。単語なら辞書を引いて履歴に残し、自動解説が有効なら解説も始める。
pub fn on_select(this: &SharedRef, ctx: &egui::Context, info: SelectionInfo) {
    if info.mode == Mode::Word {
        record_history(this, ctx, &info, Vec::new());
    }
    let auto = {
        let mut s = this.borrow_mut();
        s.tab = Tab::Explain;
        s.auto_explain
    };
    // スマホでは選んでいる間はシートを上げない（つまみで範囲を調整している本文が隠れるため）。
    // 開いていたら「少し」に下げ、上の小さな表示（単語は和訳、英文は「解説する」）にする
    if dom::is_mobile() && dom::sheet() > dom::Sheet::Peek {
        dom::set_sheet(dom::Sheet::Peek);
    }
    apply_selection(this, ctx, info, false);
    if auto {
        // 自動の解説も、シートは下げたまま始める（「見る」で開く）
        begin_explain(this, ctx, false);
    }
}

/// 履歴の項目を再表示する。解説がキャッシュにあればそれも表示する（LLM は呼ばれない）。
pub fn show_history_entry(this: &SharedRef, ctx: &egui::Context, entry: &HistoryEntry) {
    dom::clear_selection();
    let info = SelectionInfo {
        text: entry.text.clone(),
        context: entry.context.clone(),
        mode: entry.mode,
    };
    this.borrow_mut().tab = Tab::Explain;
    apply_selection(this, ctx, info, entry.has_explanation);
}

/// 直近の単語のメモから、その語をもう一度引く（本文の選択は外す）。
pub fn look_up_word(this: &SharedRef, ctx: &egui::Context, word: &str) {
    dom::clear_selection();
    let info = SelectionInfo {
        text: word.to_string(),
        context: word.to_string(),
        mode: Mode::Word,
    };
    this.borrow_mut().tab = Tab::Explain;
    apply_selection(this, ctx, info, false);
}

/// 選択を解除する（本文の選択と右ペインの表示の両方）。
pub fn clear_selection(this: &SharedRef, ctx: &egui::Context) {
    dom::clear_selection();
    let mut s = this.borrow_mut();
    s.selection_seq += 1;
    s.selection = None;
    s.dict = DictStatus::Idle;
    s.abort_dict_ai();
    s.explain.reset();
    ctx.request_repaint();
}

fn apply_selection(this: &SharedRef, ctx: &egui::Context, info: SelectionInfo, explain: bool) {
    let (seq, word) = {
        let mut s = this.borrow_mut();
        s.selection_seq += 1;
        // 新しい選択が来たら進行中の解説と AI 訳は中止する
        s.explain.reset();
        s.abort_dict_ai();
        let word = (info.mode == Mode::Word).then(|| info.text.clone());
        s.dict = if word.is_some() {
            DictStatus::Loading
        } else {
            DictStatus::Idle
        };
        s.selection = Some(info);
        (s.selection_seq, word)
    };
    if explain {
        start_explain(this, ctx, false);
    }
    ctx.request_repaint();

    let Some(word) = word else {
        return;
    };
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_dict(&word).await;
        let wants_ai = {
            let mut s = this.borrow_mut();
            // 応答待ちの間に別の選択が来ていたら、この結果は捨てる
            if s.selection_seq != seq {
                return;
            }
            let wants_ai = match result {
                Ok(r) => {
                    let wants = r.wants_ai(s.show_kana);
                    s.set_dict(r);
                    wants
                }
                Err(e) => {
                    s.dict = DictStatus::Error(e);
                    false
                }
            };
            ctx.request_repaint();
            wants_ai
        };
        if wants_ai {
            fetch_dict_ai(&this, &ctx);
        }
    });
}

/// 辞書に無い語の訳と、カタカナの読みを AI に頼む（結果は単語帳に保存され、辞書の表示に足す）。
pub fn fetch_dict_ai(this: &SharedRef, ctx: &egui::Context) {
    let (req, seq, signal) = {
        let mut s = this.borrow_mut();
        let Some(sel) = s.selection.clone() else {
            return;
        };
        if sel.mode != Mode::Word {
            return;
        }
        s.abort_dict_ai();
        let Ok(controller) = AbortController::new() else {
            return;
        };
        let signal = controller.signal();
        s.dict_ai_controller = Some(controller);
        s.dict_ai = DictAiPhase::Loading;
        let req = paper_reader_shared::DictAiRequest {
            word: sel.text.clone(),
            context: clip_context(&sel.context, &sel.text, 600),
        };
        (req, s.selection_seq, signal)
    };
    ctx.request_repaint();
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_dict_ai(&req, &signal).await;
        let mut s = this.borrow_mut();
        if s.selection_seq != seq || signal.aborted() {
            return;
        }
        s.dict_ai_controller = None;
        match result {
            Ok(r) => {
                s.dict_ai = DictAiPhase::Idle;
                s.set_dict(r);
            }
            Err(e) => s.dict_ai = DictAiPhase::Error(e),
        }
        ctx.request_repaint();
    });
}

/// 単語帳が上限に近い知らせを閉じる（このページを開いている間は出さない）。
pub fn dismiss_store_warning(this: &SharedRef) {
    let mut s = this.borrow_mut();
    s.store_warning = None;
    s.store_warning_dismissed = true;
}

// ───────────── 単語帳 ─────────────

/// 単語帳を読み込む（1 ページ目から）。
pub fn refresh_words(this: &SharedRef, ctx: &egui::Context) {
    let (seq, q, sort) = {
        let mut s = this.borrow_mut();
        let w = &mut s.words;
        w.stale = false;
        w.seq += 1;
        w.loading_more = false;
        if !matches!(w.status, WordsStatus::Loaded(_)) {
            w.status = WordsStatus::Loading;
        }
        (w.seq, w.query.trim().to_string(), w.sort)
    };
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_words(&q, sort, 0).await;
        let mut s = this.borrow_mut();
        let w = &mut s.words;
        if w.seq != seq {
            return;
        }
        w.status = match result {
            Ok(r) => {
                // 一覧から消えた語は選択からも外す
                w.selected
                    .retain(|word| r.items.iter().any(|i| &i.word == word));
                WordsStatus::Loaded(r)
            }
            Err(e) => WordsStatus::Error(e),
        };
        ctx.request_repaint();
    });
}

/// 単語帳の続きを読み込む。
pub fn words_more(this: &SharedRef, ctx: &egui::Context) {
    let (seq, q, sort, offset) = {
        let mut s = this.borrow_mut();
        let w = &mut s.words;
        let WordsStatus::Loaded(list) = &w.status else {
            return;
        };
        if w.loading_more || list.items.len() as i64 >= list.total {
            return;
        }
        let offset = list.items.len() as i64;
        w.loading_more = true;
        (w.seq, w.query.trim().to_string(), w.sort, offset)
    };
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_words(&q, sort, offset).await;
        let mut s = this.borrow_mut();
        let w = &mut s.words;
        if w.seq != seq {
            return;
        }
        w.loading_more = false;
        match (result, &mut w.status) {
            (Ok(r), WordsStatus::Loaded(list)) => {
                for item in r.items {
                    if !list.items.iter().any(|i| i.word == item.word) {
                        list.items.push(item);
                    }
                }
                list.total = r.total;
                list.store = r.store;
            }
            (Err(e), _) => w.message = Some((false, e)),
            _ => {}
        }
        ctx.request_repaint();
    });
}

/// 単語帳の語を入力欄に入れて編集する。
pub fn edit_word(this: &SharedRef, note: &WordNote) {
    let mut s = this.borrow_mut();
    let w = &mut s.words;
    w.form_open = true;
    w.form_word = note.word.clone();
    w.form_kana = note.kana.clone().unwrap_or_default();
    w.form_meanings = note.meanings.join("\n");
    w.message = None;
}

/// 入力欄の語を登録する（自分で登録した語は自動では消えない）。
pub fn save_word(this: &SharedRef, ctx: &egui::Context) {
    let upsert = {
        let mut s = this.borrow_mut();
        let w = &mut s.words;
        if w.busy {
            return;
        }
        w.busy = true;
        w.message = None;
        WordUpsert {
            word: w.form_word.clone(),
            kana: w.form_kana.clone(),
            meanings: w.form_meanings.clone(),
        }
    };
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::save_word(&upsert).await;
        {
            let mut s = this.borrow_mut();
            let w = &mut s.words;
            w.busy = false;
            match result {
                Ok(note) => {
                    w.message = Some((true, format!("「{}」を登録しました", note.word)));
                    w.form_word.clear();
                    w.form_kana.clear();
                    w.form_meanings.clear();
                    w.form_open = false;
                    w.stale = true;
                    let word = note.word.clone();
                    update_shown_note(&mut s, &word, Some(note));
                }
                Err(e) => w.message = Some((false, e)),
            }
        }
        ctx.request_repaint();
    });
}

/// 選んだ語を単語帳から削除する。
pub fn delete_selected_words(this: &SharedRef, ctx: &egui::Context) {
    let words: Vec<String> = {
        let mut s = this.borrow_mut();
        let w = &mut s.words;
        if w.busy || w.selected.is_empty() {
            return;
        }
        w.busy = true;
        w.message = None;
        w.selected.iter().cloned().collect()
    };
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::delete_words(words.clone()).await;
        if result.is_ok() {
            let mut s = this.borrow_mut();
            for w in &words {
                update_shown_note(&mut s, w, None);
            }
        }
        after_delete(&this, &ctx, result);
    });
}

/// 検索回数の少ない語から `count` 語を削除する（自分で登録した語は残す）。
pub fn prune_words(this: &SharedRef, ctx: &egui::Context, count: i64) {
    {
        let mut s = this.borrow_mut();
        if s.words.busy {
            return;
        }
        s.words.busy = true;
        s.words.message = None;
        s.store_warning = None;
    }
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::prune_words(count).await;
        after_delete(&this, &ctx, result);
    });
}

fn after_delete(
    this: &SharedRef,
    ctx: &egui::Context,
    result: Result<paper_reader_shared::WordDeleteResult, String>,
) {
    {
        let mut s = this.borrow_mut();
        let w = &mut s.words;
        w.busy = false;
        match result {
            Ok(r) => {
                w.selected.clear();
                w.message = Some((true, format!("{} 語を削除しました", r.deleted)));
                if let WordsStatus::Loaded(list) = &mut w.status {
                    list.store = r.store;
                }
                w.stale = true;
            }
            Err(e) => w.message = Some((false, e)),
        }
    }
    ctx.request_repaint();
}

/// 単語帳を変えたら、表示中の辞書の結果にも反映する（`note` が None なら消した）。
fn update_shown_note(s: &mut Shared, word: &str, note: Option<WordNote>) {
    if let DictStatus::Loaded(r) = &mut s.dict
        && r.note_word() == word
    {
        r.note = note;
    }
}

/// 現在の選択の解説を始める（進行中のものがあれば中止してから）。
/// `regenerate` が true ならキャッシュを使わずに生成し直す。
pub fn start_explain(this: &SharedRef, ctx: &egui::Context, regenerate: bool) {
    // スマホでは解説を読みやすいようシートを全体まで上げる
    dom::raise_sheet(dom::Sheet::Full);
    begin_explain(this, ctx, regenerate);
}

/// 解説を始める（シートの高さは変えない）。
fn begin_explain(this: &SharedRef, ctx: &egui::Context, regenerate: bool) {
    let (req, seq, signal) = {
        let mut s = this.borrow_mut();
        let Some(sel) = s.selection.clone() else {
            return;
        };
        s.explain.abort();
        s.explain.markdown.clear();
        s.explain.from_cache = false;
        s.explain.clear_tags();
        s.explain.knowledge.clear();
        s.explain.knowledge_add = KnowledgeAddPhase::Idle;
        s.explain.chat.reset();
        s.explain.target = Some((sel.mode, sel.text.clone()));

        if sel.text.chars().count() > MAX_TEXT_CHARS {
            s.explain.phase = ExplainPhase::Error(format!(
                "選択が長すぎます（上限 {MAX_TEXT_CHARS} 文字）。もう少し短く選択してください。"
            ));
            return;
        }
        let controller = match AbortController::new() {
            Ok(c) => c,
            Err(_) => {
                s.explain.phase = ExplainPhase::Error("AbortController を作成できません".into());
                return;
            }
        };
        let signal = controller.signal();
        s.explain.controller = Some(controller);
        s.explain.phase = ExplainPhase::Connecting;
        let req = ExplainRequest {
            mode: sel.mode,
            context: clip_context(&sel.context, &sel.text, MAX_CONTEXT_CHARS),
            text: sel.text,
            regenerate,
            slash: s.show_slash,
            style: s.explain.style,
        };
        (req, s.explain.seq, signal)
    };
    ctx.request_repaint();
    let req_info = SelectionInfo {
        text: req.text.clone(),
        context: req.context.clone(),
        mode: req.mode,
    };
    record_history(this, ctx, &req_info, Vec::new());

    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let on_event = {
            let this = this.clone();
            let ctx = ctx.clone();
            move |ev: paper_reader_shared::SseEvent| {
                if ev.event == sse_event::KNOWLEDGE {
                    let refs: Vec<KnowledgeRef> =
                        serde_json::from_str(&ev.data).unwrap_or_default();
                    let mut s = this.borrow_mut();
                    if s.explain.seq == seq {
                        s.explain.knowledge = refs;
                        ctx.request_repaint();
                    }
                    return;
                }
                if ev.event == sse_event::TAGS {
                    let tags: Vec<ExplainTag> = serde_json::from_str(&ev.data).unwrap_or_default();
                    let ids: Vec<String> = tags.iter().map(|t| t.tag_id.clone()).collect();
                    {
                        let mut s = this.borrow_mut();
                        if s.explain.seq != seq {
                            return;
                        }
                        let e = &mut s.explain;
                        e.tags = tags;
                        e.tags_received = true;
                        e.selected_tag = None;
                        s.grammar_stale = true;
                    }
                    // 履歴に文法項目を残す（出現回数の集計に使う）
                    if !ids.is_empty() {
                        record_history(&this, &ctx, &req_info, ids);
                    }
                    ctx.request_repaint();
                    return;
                }
                let mut s = this.borrow_mut();
                if s.explain.seq != seq {
                    return;
                }
                let e = &mut s.explain;
                match ev.event.as_str() {
                    sse_event::QUEUED => e.phase = ExplainPhase::Queued,
                    sse_event::STARTED => e.phase = ExplainPhase::Connecting,
                    sse_event::CACHED => e.from_cache = true,
                    sse_event::TOKEN => {
                        e.phase = ExplainPhase::Streaming;
                        e.markdown.push_str(&ev.data);
                    }
                    sse_event::DONE => {
                        e.phase = ExplainPhase::Done;
                        e.controller = None;
                    }
                    sse_event::ERROR => {
                        e.phase = ExplainPhase::Error(ev.data);
                        e.controller = None;
                    }
                    _ => {}
                }
                ctx.request_repaint();
            }
        };
        let result = crate::api::stream_explain(&req, &signal, on_event).await;
        // 完了した解説はキャッシュされ、履歴の「解説あり」が変わる
        this.borrow_mut().history_stale = true;

        let failed = {
            let mut s = this.borrow_mut();
            if s.explain.seq != seq || signal.aborted() {
                return;
            }
            let e = &mut s.explain;
            e.controller = None;
            match result {
                Err(msg) => {
                    e.phase = ExplainPhase::Error(msg);
                    true
                }
                // done も error も来ないまま閉じられた
                Ok(()) if e.phase.is_running() => {
                    e.phase = ExplainPhase::Error("サーバーとの接続が途中で切れました".into());
                    true
                }
                Ok(()) => matches!(e.phase, ExplainPhase::Error(_)),
            }
        };
        ctx.request_repaint();
        if failed {
            // LLM が止まっている可能性があるのでステータスを更新する
            refresh_health(&this, &ctx);
        }
    });
}

/// 表示中の解説から、LLM が用語と要点を整理して知識に登録する。
pub fn add_knowledge(this: &SharedRef, ctx: &egui::Context) {
    let (req, seq) = {
        let mut s = this.borrow_mut();
        let paper_id = s.paper_ref().map(|(id, _)| id);
        let Some(sel) = s.selection.clone() else {
            return;
        };
        let e = &mut s.explain;
        if e.phase != ExplainPhase::Done
            || e.markdown.trim().is_empty()
            || e.knowledge_add == KnowledgeAddPhase::Adding
        {
            return;
        }
        e.knowledge_add = KnowledgeAddPhase::Adding;
        let req = KnowledgeAdd {
            mode: sel.mode,
            context: clip_context(&sel.context, &sel.text, MAX_CONTEXT_CHARS),
            text: sel.text,
            explanation: e.markdown.chars().take(MAX_CHAT_ANSWER_CHARS).collect(),
            paper_id,
        };
        (req, e.seq)
    };
    ctx.request_repaint();
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::add_knowledge(&req).await;
        let mut s = this.borrow_mut();
        s.knowledge.stale = true;
        // 別の解説に移っていたら、結果は一覧にだけ反映する
        if s.explain.seq != seq {
            return;
        }
        s.explain.knowledge_add = match result {
            Ok(item) => KnowledgeAddPhase::Added(item.term),
            Err(e) => KnowledgeAddPhase::Error(e),
        };
        ctx.request_repaint();
    });
}

/// 知識の一覧を読み直す。
pub fn refresh_knowledge(this: &SharedRef, ctx: &egui::Context) {
    {
        let mut s = this.borrow_mut();
        if s.knowledge.loading {
            return;
        }
        s.knowledge.loading = true;
        s.knowledge.stale = false;
    }
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_knowledge().await;
        let mut s = this.borrow_mut();
        s.knowledge.loading = false;
        match result {
            Ok(list) => {
                s.knowledge.list = Some(list);
                s.knowledge.error = None;
            }
            Err(e) => s.knowledge.error = Some(e),
        }
        ctx.request_repaint();
    });
}

/// 知識を 1 件消す。
pub fn delete_knowledge(this: &SharedRef, ctx: &egui::Context, id: i64) {
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::delete_knowledge(id).await;
        let mut s = this.borrow_mut();
        match result {
            Ok(()) => {
                if let Some(list) = &mut s.knowledge.list {
                    list.items.retain(|i| i.id != id);
                }
                s.knowledge.error = None;
            }
            Err(e) => s.knowledge.error = Some(e),
        }
        ctx.request_repaint();
    });
}

/// 解説の種類を切り替え、その種類で解説し直す（保存済みならすぐに出る）。同じ種類なら何もしない。
pub fn set_explain_style(this: &SharedRef, ctx: &egui::Context, style: ExplainStyle) {
    {
        let mut s = this.borrow_mut();
        if s.explain.style == style || s.selection.is_none() {
            return;
        }
        s.explain.style = style;
    }
    start_explain(this, ctx, false);
}

/// 進行中の解説を止める（受信済みの部分は残す）。
pub fn stop_explain(this: &SharedRef) {
    let mut s = this.borrow_mut();
    if s.explain.phase.is_running() {
        s.explain.abort();
        s.explain.phase = ExplainPhase::Error("中止しました".into());
    }
}

/// チャットを開く（閉じても会話は残る）。
pub fn open_chat(this: &SharedRef) {
    // スマホでは会話を読みやすいようシートを全体まで上げる
    dom::raise_sheet(dom::Sheet::Full);
    let mut s = this.borrow_mut();
    let chat = &mut s.explain.chat;
    chat.open = true;
    chat.focus_input = true;
    chat.scroll_to_question = true;
}

pub fn close_chat(this: &SharedRef) {
    this.borrow_mut().explain.chat.open = false;
}

/// 続けて質問する。受け付けたら true（回答の生成中・空の質問は受け付けない）。
/// 直前の質問が失敗していたら、その質問は置き換える。
pub fn send_chat(this: &SharedRef, ctx: &egui::Context, question: &str) -> bool {
    let question: String = question
        .trim()
        .chars()
        .take(MAX_CHAT_QUESTION_CHARS)
        .collect();
    let (req, seq, signal) = {
        let mut s = this.borrow_mut();
        let Some(sel) = s.selection.clone() else {
            return false;
        };
        if question.is_empty() || s.explain.phase.is_running() || s.explain.chat.phase.is_running()
        {
            return false;
        }
        let Ok(controller) = AbortController::new() else {
            return false;
        };
        let signal = controller.signal();
        let explanation: String = s
            .explain
            .markdown
            .chars()
            .take(MAX_CHAT_ANSWER_CHARS)
            .collect();
        let style = s.explain.style;
        let chat = &mut s.explain.chat;
        chat.abort();
        if chat.turns.last().is_some_and(|t| t.role == ChatRole::User) {
            chat.turns.pop();
        }
        chat.turns.push(ChatMessage {
            role: ChatRole::User,
            content: question,
        });
        chat.answer.clear();
        chat.phase = ExplainPhase::Connecting;
        chat.controller = Some(controller);
        chat.scroll_to_question = true;
        let req = ChatRequest {
            mode: sel.mode,
            context: clip_context(&sel.context, &sel.text, MAX_CONTEXT_CHARS),
            text: sel.text,
            explanation,
            style,
            messages: recent_chat_messages(&chat.turns).to_vec(),
        };
        (req, chat.seq, signal)
    };
    ctx.request_repaint();

    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let on_event = {
            let this = this.clone();
            let ctx = ctx.clone();
            move |ev: paper_reader_shared::SseEvent| {
                let mut s = this.borrow_mut();
                let chat = &mut s.explain.chat;
                if chat.seq != seq {
                    return;
                }
                match ev.event.as_str() {
                    sse_event::QUEUED => chat.phase = ExplainPhase::Queued,
                    sse_event::STARTED => chat.phase = ExplainPhase::Connecting,
                    sse_event::TOKEN => {
                        chat.phase = ExplainPhase::Streaming;
                        chat.answer.push_str(&ev.data);
                    }
                    sse_event::DONE => chat.finish(),
                    sse_event::ERROR => {
                        chat.phase = ExplainPhase::Error(ev.data);
                        chat.controller = None;
                    }
                    _ => {}
                }
                ctx.request_repaint();
            }
        };
        let result = crate::api::stream_chat(&req, &signal, on_event).await;
        let failed = {
            let mut s = this.borrow_mut();
            let chat = &mut s.explain.chat;
            if chat.seq != seq || signal.aborted() {
                return;
            }
            chat.controller = None;
            match result {
                Err(msg) => chat.phase = ExplainPhase::Error(msg),
                // done も error も来ないまま閉じられた
                Ok(()) if chat.phase.is_running() => {
                    chat.phase = ExplainPhase::Error("サーバーとの接続が途中で切れました".into());
                }
                Ok(()) => {}
            }
            matches!(chat.phase, ExplainPhase::Error(_))
        };
        ctx.request_repaint();
        if failed {
            refresh_health(&this, &ctx);
        }
    });
    true
}

/// 回答の生成を止める。受信できた分は残し、まだ何も無ければ質問を入力欄に戻す。
pub fn stop_chat(this: &SharedRef) {
    let mut s = this.borrow_mut();
    let chat = &mut s.explain.chat;
    if !chat.phase.is_running() {
        return;
    }
    chat.abort();
    if chat.answer.trim().is_empty() {
        chat.restore_input = chat.turns.pop().map(|t| t.content);
        chat.answer.clear();
        chat.phase = ExplainPhase::Idle;
    } else {
        chat.finish();
    }
}

/// サーバーと LLM の疎通を確認する。
pub fn refresh_health(this: &SharedRef, ctx: &egui::Context) {
    this.borrow_mut().health = HealthStatus::Checking;
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let status = match crate::api::fetch_health().await {
            Ok(h) => HealthStatus::Ok(h),
            Err(e) => HealthStatus::Error(e),
        };
        this.borrow_mut().health = status;
        ctx.request_repaint();
    });
}

/// 入力欄の内容が論文の URL / arXiv ID なら開き、それ以外なら検索する。
pub fn submit_input(this: &SharedRef, ctx: &egui::Context, input: &str) {
    let input = input.trim();
    if input.is_empty() {
        return;
    }
    match paper_url_from_input(input) {
        Some(url) => open_paper(this, ctx, url),
        None => {
            let sort = this.borrow().search.sort;
            search(this, ctx, input.to_string(), sort);
        }
    }
}

/// タブを切り替える。切り替えた先の内容は読み直す。
pub fn show_tab(this: &SharedRef, tab: Tab) {
    let mut s = this.borrow_mut();
    s.tab = tab;
    match tab {
        Tab::Words => s.words.stale = true,
        Tab::History => s.history_stale = true,
        Tab::Grammar => s.grammar_stale = true,
        // ほかの端末で積んだ分も出るように読み直す
        Tab::Search | Tab::Explain => s.read_later.stale = true,
    }
}

/// 引用文献のカードのボタン（論文を開く・題名で探す）。
pub fn on_cite_action(this: &SharedRef, ctx: &egui::Context, action: crate::cite_peek::CiteAction) {
    use crate::cite_peek::CiteAction;
    match action {
        CiteAction::Open(id) => open_paper(this, ctx, format!("https://arxiv.org/abs/{id}")),
        CiteAction::Search(title) => search(
            this,
            ctx,
            paper_reader_shared::title_search_query(&title),
            SearchSort::Relevance,
        ),
    }
    ctx.request_repaint();
}

/// 論文を検索する（1 ページ目から）。
pub fn search(this: &SharedRef, ctx: &egui::Context, query: String, sort: SearchSort) {
    dom::raise_sheet(dom::Sheet::Full);
    let seq = {
        let mut s = this.borrow_mut();
        s.tab = Tab::Search;
        s.search_view = SearchView::Results;
        let st = &mut s.search;
        st.seq += 1;
        st.query = query.clone();
        st.sort = sort;
        st.items.clear();
        st.total = 0;
        st.error = None;
        st.loading = true;
        st.seq
    };
    ctx.request_repaint();
    fetch_search_page(this, ctx, seq, query, 0, sort);
}

/// 検索結果の続きを読み込む。
pub fn search_more(this: &SharedRef, ctx: &egui::Context) {
    let (seq, query, start, sort) = {
        let mut s = this.borrow_mut();
        let st = &mut s.search;
        if st.loading || !st.has_more() {
            return;
        }
        st.loading = true;
        st.error = None;
        (st.seq, st.query.clone(), st.items.len() as u32, st.sort)
    };
    fetch_search_page(this, ctx, seq, query, start, sort);
}

fn fetch_search_page(
    this: &SharedRef,
    ctx: &egui::Context,
    seq: u64,
    query: String,
    start: u32,
    sort: SearchSort,
) {
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_search(&query, start, sort).await;
        let mut s = this.borrow_mut();
        let st = &mut s.search;
        if st.seq != seq {
            return;
        }
        st.loading = false;
        match result {
            Ok(r) => {
                st.total = r.total;
                // ページの境目で同じ論文が重なることがあるので除く
                for item in r.items {
                    if !st.items.iter().any(|i| i.id == item.id) {
                        st.items.push(item);
                    }
                }
                // 件数が足りないページが返ったら打ち切る（API の total は概算のことがある）
                if (st.items.len() as u32) < start + SEARCH_PAGE_SIZE / 2 {
                    st.total = st.items.len() as u64;
                }
            }
            Err(e) => st.error = Some(e),
        }
        ctx.request_repaint();
    });
}

/// いま読んでいる段落（本文で選択中ならその段落、無ければ画面のいちばん上の段落）に付箋を貼る。
/// 印はすぐに付け、保存は後から行う（失敗したら元に戻す）。
pub fn place_bookmark(this: &SharedRef, ctx: &egui::Context) {
    let Some((paper_id, _)) = this.borrow().paper_ref() else {
        return;
    };
    // 本文の選択を覚えていても、アプリ側で解除していれば使わない
    let use_selection = this.borrow().selection.is_some();
    let Some(dom::ReadingLine {
        block,
        offset,
        snippet,
    }) = dom::reading_line(use_selection)
    else {
        this.borrow_mut().bookmark_error = Some("付箋を貼る行が見つかりません".into());
        return;
    };
    dom::show_bookmark(block, offset, &snippet, dom::BookmarkScroll::None);
    let prev = {
        let mut s = this.borrow_mut();
        s.bookmark_error = None;
        s.papers_stale = true;
        s.bookmark.replace(paper_reader_shared::Bookmark {
            block,
            offset,
            snippet: snippet.clone(),
            updated_at: (js_sys::Date::now() / 1000.0) as i64,
        })
    };
    ctx.request_repaint();
    let req = paper_reader_shared::BookmarkPut {
        paper_id,
        block,
        offset,
        snippet,
    };
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = crate::api::put_bookmark(&req).await {
            restore_bookmark(&this, prev, format!("付箋を保存できません: {e}"));
            ctx.request_repaint();
        }
    });
}

/// 付箋の位置まで移る。
pub fn jump_to_bookmark(this: &SharedRef) {
    let mut s = this.borrow_mut();
    let Some(b) = &s.bookmark else {
        return;
    };
    if !dom::show_bookmark(b.block, b.offset, &b.snippet, dom::BookmarkScroll::Smooth) {
        s.bookmark_error =
            Some("付箋の行が見つかりません（論文の版が変わった可能性があります）".into());
    }
}

/// 付箋をはがす。
pub fn remove_bookmark(this: &SharedRef, ctx: &egui::Context) {
    let Some((paper_id, _)) = this.borrow().paper_ref() else {
        return;
    };
    dom::clear_bookmark();
    let prev = {
        let mut s = this.borrow_mut();
        s.bookmark_error = None;
        s.papers_stale = true;
        s.bookmark.take()
    };
    ctx.request_repaint();
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = crate::api::delete_bookmark(&paper_id).await {
            restore_bookmark(&this, prev, format!("付箋をはがせません: {e}"));
            ctx.request_repaint();
        }
    });
}

/// 保存に失敗した付箋の操作を元に戻す。
fn restore_bookmark(this: &SharedRef, prev: Option<paper_reader_shared::Bookmark>, error: String) {
    let mut s = this.borrow_mut();
    match &prev {
        Some(b) => {
            dom::show_bookmark(b.block, b.offset, &b.snippet, dom::BookmarkScroll::None);
        }
        None => dom::clear_bookmark(),
    }
    s.bookmark = prev;
    s.bookmark_error = Some(error);
}

/// 論文を読み込んで左ペインに表示する。
pub fn open_paper(this: &SharedRef, ctx: &egui::Context, url: String) {
    this.borrow_mut().paper = PaperStatus::Loading;
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_paper(&url).await;
        let mut s = this.borrow_mut();
        match result {
            Ok(paper) => {
                dom::set_paper_html(&paper.html, &paper.title);
                // 付箋があれば、続きから読めるようその位置まで移る
                s.bookmark_error = None;
                if let Some(b) = &paper.bookmark
                    && !dom::show_bookmark(
                        b.block,
                        b.offset,
                        &b.snippet,
                        dom::BookmarkScroll::Instant,
                    )
                {
                    s.bookmark_error = Some(
                        "付箋の行が見つかりません（論文の版が変わった可能性があります）".into(),
                    );
                }
                s.bookmark = paper.bookmark.clone();
                s.selection = None;
                s.dict = DictStatus::Idle;
                s.abort_dict_ai();
                s.selection_seq += 1;
                s.explain.reset();
                s.history_stale = true;
                s.papers_stale = true;
                s.tab = Tab::Explain;
                // スマホでは本文を広く見せる
                if dom::is_mobile() {
                    dom::set_sheet(dom::Sheet::Peek);
                }
                s.paper = PaperStatus::Loaded {
                    id: paper.id,
                    title: paper.title,
                    source_url: paper.source_url,
                };
            }
            // 失敗しても表示中の論文と選択はそのまま残す
            Err(e) => s.paper = PaperStatus::Error(e),
        }
        ctx.request_repaint();
    });
}

/// 表示中の論文の履歴に追加する（論文を開いていなければ何もしない）。
/// `tags` は英文で検出した文法タグの id（空なら既存の値を残す）。
fn record_history(this: &SharedRef, ctx: &egui::Context, info: &SelectionInfo, tags: Vec<String>) {
    let Some((paper_id, paper_title)) = this.borrow().paper_ref() else {
        return;
    };
    let record = HistoryRecord {
        paper_id,
        paper_title,
        mode: info.mode,
        // 解説のキャッシュキーと揃えるため、解説で送るのと同じ切り詰めをする
        context: clip_context(&info.context, &info.text, MAX_CONTEXT_CHARS),
        text: info.text.clone(),
        tags,
    };
    if record.text.chars().count() > MAX_TEXT_CHARS {
        return;
    }
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = crate::api::post_history(&record).await {
            log::warn!("履歴を保存できません: {e}");
        }
        this.borrow_mut().history_stale = true;
        ctx.request_repaint();
    });
}

/// 履歴を読み込む。
pub fn refresh_history(this: &SharedRef, ctx: &egui::Context) {
    let (seq, paper) = {
        let mut s = this.borrow_mut();
        s.history_stale = false;
        s.history_seq += 1;
        let paper = match s.history_scope {
            // 見た論文の一覧は refresh_papers で読む
            HistoryScope::Papers => {
                s.papers_stale = true;
                return;
            }
            HistoryScope::ThisPaper => match s.paper_ref() {
                Some((id, _)) => Some(id),
                None => {
                    s.history = HistoryStatus::Loaded(Vec::new());
                    return;
                }
            },
            HistoryScope::AllPapers => None,
        };
        // 読み直しの間も一覧を残す（ちらつかないように）
        if !matches!(s.history, HistoryStatus::Loaded(_)) {
            s.history = HistoryStatus::Loading;
        }
        (s.history_seq, paper)
    };
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_history(paper.as_deref()).await;
        let mut s = this.borrow_mut();
        if s.history_seq != seq {
            return;
        }
        s.history = match result {
            Ok(r) => HistoryStatus::Loaded(r.entries),
            Err(e) => HistoryStatus::Error(e),
        };
        ctx.request_repaint();
    });
}

/// 履歴を 1 件削除する。
pub fn delete_history_entry(this: &SharedRef, ctx: &egui::Context, id: i64) {
    // 先に一覧から消して、すぐ反映されるようにする
    if let HistoryStatus::Loaded(entries) = &mut this.borrow_mut().history {
        entries.retain(|e| e.id != id);
    }
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = crate::api::delete_history(id).await {
            log::warn!("履歴を削除できません: {e}");
            this.borrow_mut().history_stale = true;
        }
        ctx.request_repaint();
    });
}

/// キーボードショートカット（E: 解説, Esc: 選択解除）。
pub fn on_shortcut(this: &SharedRef, ctx: &egui::Context, shortcut: dom::Shortcut) {
    match shortcut {
        dom::Shortcut::Explain => {
            let ready = {
                let mut s = this.borrow_mut();
                // 会話がある解説は E キーで作り直さない（会話が消えるため）
                let ready = s.selection.is_some()
                    && !s.explain.phase.is_running()
                    && s.explain.chat.turns.is_empty();
                if ready {
                    s.tab = Tab::Explain;
                }
                ready
            };
            if ready {
                start_explain(this, ctx, false);
            }
        }
        dom::Shortcut::ClearSelection => clear_selection(this, ctx),
    }
}

/// 文法項目のチップをクリックしたとき。もう一度押すと閉じる。
pub fn select_tag(this: &SharedRef, index: usize) {
    let mut s = this.borrow_mut();
    let anchor = s
        .selection
        .as_ref()
        .map(|sel| sel.text.clone())
        .unwrap_or_default();
    let e = &mut s.explain;
    if e.selected_tag == Some(index) {
        e.selected_tag = None;
        e.highlight_found = false;
        dom::clear_highlight();
        return;
    }
    let Some(tag) = e.tags.get(index) else {
        return;
    };
    e.highlight_found = dom::highlight_span(&tag.span, &anchor);
    e.selected_tag = Some(index);
}

/// 文法項目の一覧（タグ・出現回数）を読み込む。
pub fn refresh_grammar(this: &SharedRef, ctx: &egui::Context) {
    {
        let mut s = this.borrow_mut();
        s.grammar_stale = false;
        if !matches!(s.grammar, GrammarStatus::Loaded(_)) {
            s.grammar = GrammarStatus::Loading;
        }
    }
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_grammar_tags().await;
        this.borrow_mut().grammar = match result {
            Ok(g) => GrammarStatus::Loaded(g),
            Err(e) => GrammarStatus::Error(e),
        };
        ctx.request_repaint();
    });
}

/// フィードバックの画面を開く（書きかけの内容は残す）。
pub fn open_feedback(this: &SharedRef, ctx: &egui::Context) {
    dom::raise_sheet(dom::Sheet::Full);
    {
        let mut s = this.borrow_mut();
        let f = &mut s.feedback;
        f.open = true;
        f.error = None;
        if matches!(f.stage, FeedbackStage::Done(_)) {
            f.stage = FeedbackStage::Edit;
        }
    }
    refresh_feedback_list(this, ctx);
}

pub fn refresh_feedback_list(this: &SharedRef, ctx: &egui::Context) {
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_feedback().await;
        let mut s = this.borrow_mut();
        match result {
            Ok(list) => s.feedback.list = Some(list),
            Err(e) => log::warn!("フィードバックの一覧を取得できません: {e}"),
        }
        ctx.request_repaint();
    });
}

/// 送信時の状況（添付する場合だけ Issue に載る）。
fn feedback_context(s: &Shared) -> FeedbackContext {
    let (paper_id, paper_title) = match s.paper_ref() {
        Some((id, title)) => (Some(id), Some(title)),
        None => (None, None),
    };
    let tab = match s.tab {
        Tab::Explain => "解説",
        Tab::Search => "検索",
        Tab::History => "履歴",
        Tab::Grammar => "文法項目",
        Tab::Words => "単語帳",
    };
    FeedbackContext {
        paper_id,
        paper_title,
        selection: s.selection.as_ref().map(|sel| sel.text.clone()),
        tab: tab.into(),
        user_agent: web_sys::window()
            .and_then(|w| w.navigator().user_agent().ok())
            .unwrap_or_default(),
    }
}

/// LLM にラベルとタイトルを付けてもらい、確認画面にする。
pub fn preview_feedback(this: &SharedRef, ctx: &egui::Context) {
    let draft = {
        let mut s = this.borrow_mut();
        let context = feedback_context(&s);
        let f = &mut s.feedback;
        if f.body.trim().is_empty() {
            f.error = Some("内容を書いてください".into());
            return;
        }
        f.error = None;
        f.stage = FeedbackStage::Labeling;
        FeedbackDraft {
            title: f.title.clone(),
            body: f.body.clone(),
            include_context: f.include_context,
            context,
            rewrite: f.rewrite,
        }
    };
    ctx.request_repaint();
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::preview_feedback(&draft).await;
        let mut s = this.borrow_mut();
        let f = &mut s.feedback;
        if !matches!(f.stage, FeedbackStage::Labeling) {
            return;
        }
        match result {
            Ok(p) => {
                f.title = p.title.clone();
                f.labels = p.labels.clone();
                // 整えた文章を送る内容にし、書いたままの文章は戻せるように残す
                f.original_body = draft.body;
                f.use_original = false;
                if p.rewritten {
                    f.body = p.body.clone();
                }
                f.stage = FeedbackStage::Review(p);
            }
            Err(e) => {
                f.error = Some(e);
                f.stage = FeedbackStage::Edit;
            }
        }
        ctx.request_repaint();
    });
}

/// 確認した内容で送る（GitHub が未設定なら保存だけ）。
pub fn submit_feedback(this: &SharedRef, ctx: &egui::Context) {
    let sub = {
        let mut s = this.borrow_mut();
        let context = feedback_context(&s);
        let f = &mut s.feedback;
        f.error = None;
        f.stage = FeedbackStage::Sending;
        FeedbackSubmit {
            title: f.title.clone(),
            body: f.body.clone(),
            labels: f.labels.clone(),
            include_context: f.include_context,
            context,
        }
    };
    ctx.request_repaint();
    let this = this.clone();
    let ctx2 = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::submit_feedback(&sub).await;
        {
            let mut s = this.borrow_mut();
            let f = &mut s.feedback;
            match result {
                Ok(item) => {
                    // 送れたら（保存できたら）書いた内容は消す
                    f.title.clear();
                    f.body.clear();
                    f.original_body.clear();
                    f.labels.clear();
                    f.stage = FeedbackStage::Done(item);
                }
                Err(e) => {
                    f.error = Some(e);
                    f.stage = FeedbackStage::Edit;
                }
            }
        }
        refresh_feedback_list(&this, &ctx2);
        ctx2.request_repaint();
    });
}

/// 未送信のものを送り直す。
pub fn retry_feedback(this: &SharedRef, ctx: &egui::Context, id: i64) {
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::retry_feedback(id).await;
        {
            let mut s = this.borrow_mut();
            match result {
                Ok(item) => {
                    if let FeedbackStage::Done(done) = &mut s.feedback.stage
                        && done.id == item.id
                    {
                        *done = item;
                    }
                }
                Err(e) => s.feedback.error = Some(e),
            }
        }
        refresh_feedback_list(&this, &ctx);
    });
}

/// 見た論文の一覧を読み込む。
pub fn refresh_papers(this: &SharedRef, ctx: &egui::Context) {
    {
        let mut s = this.borrow_mut();
        s.papers_stale = false;
        if !matches!(s.papers, PapersStatus::Loaded(_)) {
            s.papers = PapersStatus::Loading;
        }
    }
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_papers().await;
        this.borrow_mut().papers = match result {
            Ok(r) => PapersStatus::Loaded(r),
            Err(e) => PapersStatus::Error(e),
        };
        ctx.request_repaint();
    });
}

/// 後で読む論文を読み込む。
pub fn refresh_read_later(this: &SharedRef, ctx: &egui::Context) {
    let seq = {
        let mut s = this.borrow_mut();
        let r = &mut s.read_later;
        if r.loading {
            return;
        }
        r.stale = false;
        r.loading = true;
        r.seq
    };
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_read_later().await;
        {
            let mut s = this.borrow_mut();
            let r = &mut s.read_later;
            r.loading = false;
            if r.seq != seq {
                // 読み込み中に追加・削除したので、読み直す
                r.stale = true;
            } else {
                match result {
                    Ok(resp) => {
                        r.items = resp.items;
                        r.loaded = true;
                        r.load_error = None;
                    }
                    Err(e) => r.load_error = Some(e),
                }
            }
        }
        ctx.request_repaint();
    });
}

/// 後で読むの一番上に積む。
pub fn add_read_later(
    this: &SharedRef,
    ctx: &egui::Context,
    req: paper_reader_shared::ReadLaterPut,
) {
    {
        let mut s = this.borrow_mut();
        let r = &mut s.read_later;
        r.seq += 1;
        r.error = None;
        r.items.retain(|i| i.id != req.paper_id);
        r.items.insert(
            0,
            ReadLaterItem {
                id: req.paper_id.clone(),
                title: req.title.clone(),
                authors: req.authors.clone(),
                added_at: (js_sys::Date::now() / 1000.0) as i64,
            },
        );
    }
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::put_read_later(&req).await;
        finish_read_later_change(
            &this,
            result.map_err(|e| format!("後で読むに追加できません: {e}")),
        );
        ctx.request_repaint();
    });
}

/// 後で読むから外す。
pub fn remove_read_later(this: &SharedRef, ctx: &egui::Context, id: String) {
    {
        let mut s = this.borrow_mut();
        let r = &mut s.read_later;
        r.seq += 1;
        r.error = None;
        r.items.retain(|i| i.id != id);
    }
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::delete_read_later(&id).await;
        finish_read_later_change(
            &this,
            result.map_err(|e| format!("後で読むから外せません: {e}")),
        );
        ctx.request_repaint();
    });
}

/// 追加・削除のあとは、成否にかかわらずサーバーの内容に合わせる。
fn finish_read_later_change(this: &SharedRef, result: Result<(), String>) {
    let mut s = this.borrow_mut();
    let r = &mut s.read_later;
    r.seq += 1;
    r.stale = true;
    if let Err(e) = result {
        r.error = Some(e);
    }
}

/// 見た論文から外す（`purge` なら保存した本文と図も消す）。
pub fn delete_recent_paper(this: &SharedRef, ctx: &egui::Context, id: String, purge: bool) {
    if let PapersStatus::Loaded(r) = &mut this.borrow_mut().papers {
        r.papers.retain(|p| p.id != id);
    }
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = crate::api::delete_paper(&id, purge).await {
            log::warn!("論文を一覧から外せません: {e}");
        }
        // キャッシュの使用量も変わるので読み直す
        this.borrow_mut().papers_stale = true;
        ctx.request_repaint();
    });
}
