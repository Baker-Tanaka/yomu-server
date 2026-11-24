//! 右ペインの状態と、DOM のイベント・非同期処理から状態を更新する処理。

use std::cell::RefCell;
use std::rc::Rc;

use eframe::egui;
use paper_reader_shared::{
    DictResponse, ExplainRequest, HealthResponse, MAX_CONTEXT_CHARS, MAX_TEXT_CHARS, Mode,
    clip_context, sse_event,
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
    pub paper: PaperStatus,
    pub selection: Option<SelectionInfo>,
    pub dict: DictStatus,
    pub explain: Explain,
    pub health: HealthStatus,
    /// 選択を確定したら自動で解説する（既定は手動）
    pub auto_explain: bool,
    /// 選択ごとに増える番号。古い選択に対する辞書の応答を捨てるために使う
    selection_seq: u64,
}

/// 新しい選択を反映する。単語なら辞書を引き、自動解説が有効なら解説も始める。
pub fn on_select(this: &SharedRef, ctx: &egui::Context, info: SelectionInfo) {
    let (seq, word, auto) = {
        let mut s = this.borrow_mut();
        s.selection_seq += 1;
        // 新しい選択が来たら進行中の解説は中止する
        s.explain.abort();
        s.explain.phase = ExplainPhase::Idle;
        s.explain.markdown.clear();
        s.explain.target = None;
        let word = (info.mode == Mode::Word).then(|| info.text.clone());
        s.dict = if word.is_some() {
            DictStatus::Loading
        } else {
            DictStatus::Idle
        };
        s.selection = Some(info);
        (s.selection_seq, word, s.auto_explain)
    };
    if auto {
        start_explain(this, ctx);
    }
    ctx.request_repaint();

    let Some(word) = word else {
        return;
    };
    let this = this.clone();
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = crate::api::fetch_dict(&word).await;
        let mut s = this.borrow_mut();
        // 応答待ちの間に別の選択が来ていたら、この結果は捨てる
        if s.selection_seq != seq {
            return;
        }
        s.dict = match result {
            Ok(r) => DictStatus::Loaded(r),
            Err(e) => DictStatus::Error(e),
        };
        ctx.request_repaint();
    });
}

/// 現在の選択の解説を始める（進行中のものがあれば中止してから）。
pub fn start_explain(this: &SharedRef, ctx: &egui::Context) {
    let (req, seq, signal) = {
        let mut s = this.borrow_mut();
        let Some(sel) = s.selection.clone() else {
            return;
        };
        s.explain.abort();
        s.explain.markdown.clear();
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
            regenerate: false,
        };
        (req, s.explain.seq, signal)
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
                if s.explain.seq != seq {
                    return;
                }
                let e = &mut s.explain;
                match ev.event.as_str() {
                    sse_event::QUEUED => e.phase = ExplainPhase::Queued,
                    sse_event::STARTED => e.phase = ExplainPhase::Connecting,
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
                s.selection_seq += 1;
                s.explain.abort();
                s.explain.phase = ExplainPhase::Idle;
                s.explain.markdown.clear();
                s.explain.target = None;
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
