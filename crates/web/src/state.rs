//! 右ペインの状態と、DOM のイベント・非同期処理から状態を更新する処理。

use std::cell::RefCell;
use std::rc::Rc;

use eframe::egui;
use paper_reader_shared::{
    DictResponse, ExplainRequest, ExplainTag, FeedbackContext, FeedbackDraft, FeedbackItem,
    FeedbackListResponse, FeedbackPreview, FeedbackSubmit, GrammarMapResponse, HealthResponse,
    HistoryEntry, HistoryRecord, MAX_CONTEXT_CHARS, MAX_TEXT_CHARS, Mode, SEARCH_PAGE_SIZE,
    SearchItem, SearchSort, WordListResponse, WordNote, WordSort, WordStoreStats, WordUpsert,
    clip_context, paper_url_from_input, sse_event,
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

#[derive(Default)]
pub struct Explain {
    pub phase: ExplainPhase,
    /// 受信済みの Markdown
    pub markdown: String,
    /// 解説の対象（表示用）
    pub target: Option<(Mode, String)>,
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
        self.from_cache = false;
        self.clear_tags();
    }

    fn clear_tags(&mut self) {
        self.tags.clear();
        self.tags_received = false;
        self.selected_tag = None;
        self.highlight_found = false;
        dom::clear_highlight();
    }
}

/// 文法項目タブのデータの状態。
#[derive(Debug, Clone, Default)]
pub enum GrammarStatus {
    #[default]
    NotLoaded,
    Loading,
    Loaded(GrammarMapResponse),
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
            error: None,
            list: None,
        }
    }
}

/// 「講座の案内を表示する」設定の localStorage のキー
const SHOW_COURSE_KEY: &str = "paper-reader.show-course";
/// 「カタカナの読みを表示する」設定の localStorage のキー
const SHOW_KANA_KEY: &str = "paper-reader.show-kana";

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
    /// 見た論文の一覧を読み直す
    pub papers_stale: bool,
    pub grammar: GrammarStatus,
    pub grammar_order: GrammarOrder,
    /// 出現回数などが変わったので、次に文法項目タブを表示するときに読み直す
    pub grammar_stale: bool,
    /// 設定: 講座（パーフェクト英文法）の案内を表示する
    pub show_course: bool,
    pub feedback: Feedback,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            show_course: dom::load_bool(SHOW_COURSE_KEY, true),
            show_kana: dom::load_bool(SHOW_KANA_KEY, true),
            words: Words {
                stale: true,
                prune_count: 1_000,
                ..Words::default()
            },
            grammar_stale: true,
            papers_stale: true,
            ..Self::default()
        }
    }

    pub fn set_show_course(&mut self, on: bool) {
        self.show_course = on;
        dom::save_bool(SHOW_COURSE_KEY, on);
    }

    pub fn set_show_kana(&mut self, on: bool) {
        self.show_kana = on;
        dom::save_bool(SHOW_KANA_KEY, on);
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
        self.dict = DictStatus::Loaded(r);
    }

    /// 講座の短い名前（文法データを読み込む前は既定の名前）。
    pub fn course_short(&self) -> String {
        match &self.grammar {
            GrammarStatus::Loaded(g) if !g.course_short.is_empty() => g.course_short.clone(),
            _ => "パーフェクト英文法".into(),
        }
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
    if info.mode == Mode::Word && !auto {
        // スマホでは単語はシートを小さくしたまま、上に和訳だけを出す（本文を隠さない）。
        // 全体まで上げているときはそのまま
        if dom::is_mobile() && dom::sheet() == dom::Sheet::Half {
            dom::set_sheet(dom::Sheet::Peek);
        }
    } else {
        // 英文は解説シートを半分まで上げて結果を見せる
        dom::raise_sheet(dom::Sheet::Half);
    }
    apply_selection(this, ctx, info, auto);
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
    let (req, seq, signal) = {
        let mut s = this.borrow_mut();
        let Some(sel) = s.selection.clone() else {
            return;
        };
        s.explain.abort();
        s.explain.markdown.clear();
        s.explain.from_cache = false;
        s.explain.clear_tags();
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

/// 進行中の解説を止める（受信済みの部分は残す）。
pub fn stop_explain(this: &SharedRef) {
    let mut s = this.borrow_mut();
    if s.explain.phase.is_running() {
        s.explain.abort();
        s.explain.phase = ExplainPhase::Error("中止しました".into());
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

/// 論文を検索する（1 ページ目から）。
pub fn search(this: &SharedRef, ctx: &egui::Context, query: String, sort: SearchSort) {
    dom::raise_sheet(dom::Sheet::Full);
    let seq = {
        let mut s = this.borrow_mut();
        s.tab = Tab::Search;
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
                let ready = s.selection.is_some() && !s.explain.phase.is_running();
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

/// 文法項目の一覧（タグ・レッスン・出現回数）を読み込む。
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
        let result = crate::api::fetch_grammar_map().await;
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
