# プロジェクト概要
英語論文を読むための Web アプリ「paper-reader」を作る。
- 論文本文はブラウザの DOM で表示し、ユーザーが選択した単語・英文を取得する
- 単語はローカル辞書で即座に和訳し、英文はローカル LLM で「和訳・文構造・文法解説」を日本語でストリーミング表示する
- 解説 UI は egui（eframe）を wasm にビルドして表示する
- サーバー・LLM・DB はすべて 1 台の MacBook Air（M2, 16GB）で動かす

# アーキテクチャ
ブラウザ
  ├─ 左ペイン: <div id="paper">  … サーバーが無害化した論文 HTML を表示（DOM）
  └─ 右ペイン: <canvas id="egui_canvas"> … egui の解説 UI
        │  HTTPS（tailscale serve が 127.0.0.1:8080 へ転送）
MacBook Air
  ├─ Rust サーバー（axum, 127.0.0.1:8080）… 静的配信 / 論文取得 / 辞書 / LLM 中継
  ├─ LLM ランタイム（Ollama または llama.cpp の llama-server, 127.0.0.1 のみ）
  └─ SQLite（辞書・解説キャッシュ・論文キャッシュ）
外部: arxiv.org（サーバーからのみアクセス）

すべて同一オリジンで配信するため CORS 設定は不要。LLM ランタイムを外部に公開してはいけない。

# リポジトリ構成（Cargo workspace）
paper-reader/（このリポジトリのルート）
├─ Cargo.toml            # workspace
├─ crates/
│  ├─ shared/            # API の型（serde）。server と web の両方から使う
│  ├─ server/            # axum サーバー
│  └─ web/               # eframe アプリ（wasm32-unknown-unknown）
│     ├─ index.html      # 2 ペインのレイアウト、#paper と #egui_canvas
│     ├─ Trunk.toml      # 開発時 /api を 127.0.0.1:8080 へ proxy
│     └─ assets/         # 日本語フォント（Noto Sans JP サブセット）, paper.css
├─ data/                 # 辞書の元データ, SQLite ファイル（git 管理外）
├─ scripts/              # 辞書インポートなど
└─ deploy/               # launchd plist, 起動スクリプト

# 主要ライブラリ
server: axum, tokio, tower-http (ServeDir, compression, trace), reqwest (rustls), ammonia, scraper,
        sqlx (sqlite, runtime-tokio), serde, serde_json, tracing, tracing-subscriber, anyhow, thiserror, sha2
web:    eframe（egui）, egui_commonmark, wasm-bindgen, wasm-bindgen-futures, web-sys, js-sys, serde, serde_json
ツール: trunk, wasm-opt, sqlx-cli（必要なら）
クレートのバージョンは実装時点の最新安定版を使い、eframe の Web 起動 API などはそのバージョンのドキュメントに合わせること。

# API（型は crates/shared に定義）
GET  /                          … trunk build の成果物（dist/）を配信
GET  /api/paper?url=<arxiv URL> … 論文を取得し、無害化した本文 HTML とメタ情報を JSON で返す
GET  /api/papers                … 見た論文の一覧（最後に開いた順、版違いはまとめる）とキャッシュの使用量
DELETE /api/papers?id=&purge=   … 見た論文から外す（purge なら保存した本文と図も消す）
PUT  /api/bookmark              … 論文に付箋（どこまで読んだか）を貼る。body: { paper_id, block, snippet }。版なしの ID で 1 論文 1 枚
DELETE /api/bookmark?paper_id=  … 付箋をはがす（付箋は /api/paper の応答にも含める）
GET  /api/paper-asset?url=      … 論文の図をサーバー経由で配信し SQLite に保存する（ALLOWED_HOSTS の https で /html/ 以下の画像のみ）
GET  /api/dict?word=<word>      … ローカル辞書で引いた結果を返す（見出し語、訳のリスト、単語帳の情報）。引くたびに単語帳の検索回数を数える
POST /api/dict/ai               … 辞書に無い語の訳とカタカナの読みを LLM に推定させ、単語帳に保存して返す（UI では「AI 訳」「AI 推定」と明記する）
GET  /api/words                 … 単語帳の一覧（検索回数・読み・訳）と使用量
POST /api/words                 … 単語帳に自分で登録・編集する（自分で登録した語は自動では消さない）
POST /api/words/delete          … 選んだ語を単語帳から消す
POST /api/words/prune           … 検索回数の少ない語から消す（上限 WORD_STORE_MAX を超えたときも自動で同じ順に消す）
POST /api/explain               … body: { mode: "word" | "sentence", text, context }
                                   レスポンスは text/event-stream（SSE 形式）で解説を逐次送る
                                   イベント: queued / started / token / tags / done / error
                                   （mode=sentence は token で和訳を先に流し、tags（文法タグ）を送ってから残りの解説の token を流す）
                                   （キャッシュのヒット時は cached → tags → token → done）
GET  /api/grammar-map           … 文法タグ一覧・カテゴリ・レッスン対応・履歴での出現回数
POST /api/admin/reload-grammar  … 文法タグとレッスン対応を読み直す（127.0.0.1 から直接の要求のみ。転送ヘッダー付きは拒否）
GET  /api/feedback              … 送信先の設定（GitHub）・ラベル定義・送ったフィードバックの一覧
POST /api/feedback/preview      … 不具合・要望の下書きに LLM がラベル（種類・機能の場所）とタイトルを付け、Issue の本文を組み立てて返す（まだ送らない）
POST /api/feedback              … 確認した内容を保存し、GitHub が設定されていれば Issue を作る（未設定・失敗なら未送信として残す）
POST /api/feedback/{id}/retry   … 未送信のフィードバックを送り直す
GET  /api/health                … サーバーと LLM ランタイムの疎通確認

# 設定（環境変数, .env 可）
BIND_ADDR=127.0.0.1:8080
LLM_BASE_URL=http://127.0.0.1:11434/v1   # OpenAI 互換 API（Ollama / llama-server 共通）
LLM_MODEL=<モデル名>
DATABASE_URL=sqlite://data/app.db
STATIC_DIR=crates/web/dist
ALLOWED_HOSTS=arxiv.org,www.arxiv.org
GRAMMAR_TAGS_PATH=data/grammar_tags.yaml                # 文法タグのマスター（リポジトリで管理）
GRAMMAR_MAP_PATH=data/grammar_map.local.yaml            # 講座のレッスン対応（ユーザーが記入。git 管理外）
GRAMMAR_MAP_TEMPLATE_PATH=data/grammar_map.template.yaml # local が無いときに読むテンプレート
WORD_STORE_MAX=20000                                    # 単語帳に残す語数の上限
GITHUB_REPO=<owner/name>                                # フィードバックを Issue にするリポジトリ（未設定なら保存だけ）
GITHUB_TOKEN=<fine-grained token>                       # そのリポジトリの Issues: Read and write だけを持つトークン

# 開発方針: 速さを最優先し、品質は落とさない
UI の反応と結果の表示をできるだけ速くする。ただし速さのために正確さ・安全性・解説の質を削らない。
「守るべきルール」と衝突したらルールを優先し、その範囲で一番速い実装を選ぶ。
- 体感速度
  - 操作には 1 フレーム以内に何かを返す（選択直後に見出し・「解析中」などを即表示し、結果は後から埋める）
  - 結果は出来た分から順に出す。LLM は SSE で逐次表示し、最初のトークンまでの時間（TTFT）を縮めることを重視する
  - 辞書引きなどローカルで済む処理は LLM を待たずに先に表示する
  - ネットワーク・LLM・DB の待ちで UI スレッド（egui の描画、wasm のメインスレッド）を止めない
- サーバー
  - キャッシュ（解説・論文・図・辞書）を先に見て、ヒットしたら LLM や外部取得を呼ばない
  - 重い処理（HTML 無害化、DB 書き込み、外部取得）は必要なら spawn_blocking / 非同期に逃がし、他の要求を詰まらせない
  - レスポンスは圧縮し、不要なデータを返さない。LLM のプロンプトは品質を保てる範囲で短くする（入力トークンは TTFT に直結する）
  - Semaphore で 1 件に絞る制約は守り、順番待ちは早く伝える。モデルのウォームアップや keep_alive で初回の読み込み待ちを減らす
- web（wasm）
  - 毎フレームの無駄な再計算・再割り当てを避け、変化があるときだけ request_repaint する
  - wasm とフォントのサイズを抑える（release + wasm-opt、フォントはサブセット）。初回表示を遅らせる処理は後回しにする
- 品質の担保
  - 速くした変更でも、テスト・clippy・fmt を通し、キャッシュのキーや無効化の誤りで古い／別の結果を出さないことを確かめる
  - 速度に効く変更は、可能なら前後で時間を測って（tracing のスパン、ブラウザの計測など）効果を確認する。推測だけで複雑にしない
  - 解説の正確さ（和訳・文構造・文法用語）を落とす短縮・モデル変更・プロンプト削減はしない

# 守るべきルール
- セキュリティ
  - /api/paper は SSRF 対策として ALLOWED_HOSTS のホストへの https のみ許可する。リダイレクト先も検証する
  - 取得した HTML は必ず ammonia で無害化してから返す。script, style 属性, on* 属性, iframe, form は除去する。
    MathML（math, mi, mo, mn, msup, msub, mfrac, mrow, msqrt, mtext, semantics, annotation など）と、
    表・図・見出し・段落・リストは残す。相対 URL の img src は絶対 URL に書き換える
  - LLM への入力文字数に上限を設ける（例: text 2,000 文字、context 4,000 文字）
  - GITHUB_TOKEN はサーバーの環境変数にだけ置く。ブラウザ・LLM・ログに出さない（設定の Debug 表示は伏せ字にする）
  - GitHub API の宛先は https://api.github.com に固定し、リダイレクトは追わない
  - フィードバックは公開リポジトリの Issue になりうるので、送る前に本文をプレビューで見せる。論文・選択テキスト・ブラウザ情報の添付は利用者が選ぶ
  - フィードバックのラベルは固定の一覧から選ぶ（LLM の出力は enum で縛り、サーバーでも検証する）
- 信頼性
  - LLM への同時リクエストは tokio::sync::Semaphore で 1 件に制限し、待機中は SSE で「順番待ち」を送る
  - クライアントが切断したら LLM へのストリームも中断する
  - reqwest にタイムアウトを設定する
  - サーバーのハンドラ内で unwrap / expect を使わず、エラーは JSON（{ error: string }）と適切なステータスで返す
- 品質
  - cargo fmt, cargo clippy -- -D warnings を通す
  - server の純粋なロジック（URL 検証、HTML 無害化、辞書の語形処理、キャッシュキー生成）には単体テストを書く
  - web クレートは wasm 専用。clippy は `cargo clippy -p paper-reader-web --target wasm32-unknown-unknown -- -D warnings` でも確認する
- UI の文言は日本語
  - 講座の解説文・動画の内容・テキストは、リポジトリにも LLM のプロンプトにも一切入れない
  - 扱ってよいのは「カテゴリ名」と「ユーザーが自分で書き写したレッスンのタイトル」だけ。レッスンのタイトルも LLM には渡さない
  - 解説の中身は LLM が一般的な英文法の知識で書く。講座は「復習先の案内」としてだけ表示する
  - 文法用語は日本の学校文法の呼び方に揃える（第5文型、分詞構文、関係副詞、仮定法過去完了 など）
