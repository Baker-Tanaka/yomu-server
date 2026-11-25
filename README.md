# paper-reader

英語論文（arXiv の HTML 版）を読むための Web アプリです。左ペインに論文本文を表示し、選択した単語・英文を右ペイン（egui / wasm）に表示します。

- 現在の実装範囲: **フェーズ4**（論文表示と選択の取得、ローカル辞書による単語訳、LLM による解説のストリーミング、
  解説キャッシュ、履歴、本番ビルドと常時稼働）
- 設計とルール: [CLAUDE.md](CLAUDE.md)

## 構成

| パス | 内容 |
| --- | --- |
| `crates/shared` | API の型（server と web で共有） |
| `crates/server` | axum サーバー（静的配信、`/api/paper`、`/api/search`、`/api/dict`、`/api/words`、`/api/explain`、`/api/history`、`/api/health`） |
| `crates/web` | eframe アプリ（wasm32-unknown-unknown）、`index.html`、`Trunk.toml`、`assets/` |
| `scripts/import_dict` | 辞書データを SQLite に投入するツール |
| `scripts/issues.py` | GitHub の Issue を選んで Claude Code に実装させ、PR にするスクリプト（「Issue から開発する」を参照） |
| `scripts/subset_font.sh` | 日本語フォントを使う文字だけに絞るスクリプト |
| `deploy/` | 本番ビルド、launchd の plist、インストール用スクリプト |
| `data/` | 辞書の元データ、SQLite ファイル（git 管理外） |

## 必要なもの

- Rust（`rust-toolchain.toml` で 1.100.0 を指定しています。初回の `cargo` 実行時に rustup が自動でインストールします。eframe 0.37 は rustc 1.95 以上が必要です）
- wasm ターゲット（`rust-toolchain.toml` に記載済みのため通常は自動で入ります）

  ```sh
  rustup target add wasm32-unknown-unknown
  ```

- trunk

  ```sh
  cargo install --locked trunk
  # または: brew install trunk
  ```

  trunk は対応する `wasm-bindgen` CLI を自動でダウンロードします。

## 辞書データ（EJDict-hand）

単語訳には [EJDict-hand](https://github.com/kujirahand/EJDict)（くじらはんど）を使います。
**パブリックドメイン（CC0 1.0）** の英和辞書で、約 4.6 万語を収録しています。データはリポジトリに含めていないので、各自で入手してください。

```sh
# どちらか一方
git clone --depth 1 https://github.com/kujirahand/EJDict data/EJDict        # → data/EJDict/src/*.txt
# または https://kujirahand.com/web-tools/EJDictFreeDL.php から
#   テキスト版をダウンロードし、data/ejdict-hand-utf8.txt として置く

# SQLite（DATABASE_URL、既定は data/app.db）の dict テーブルへ投入
cargo run -p import-dict --release
# パスを明示する場合: cargo run -p import-dict --release -- path/to/file_or_dir ...
```

- 形式は「見出し語\t訳」のタブ区切りです。`A,a,an` のようにカンマで並んだ見出し語は、それぞれの語として登録します
- 見出し語は小文字化します。`ACT`（略語）と `act` のように大文字小文字だけが違う項目は 1 つにまとめ、元から小文字の項目の訳を先に並べます
- 再実行すると dict テーブルを入れ替えます（サーバーを動かしたままでも実行できます）

## LLM（Ollama）

解説の生成には [Ollama](https://ollama.com) の OpenAI 互換 API を使います（llama.cpp の llama-server でも可）。

```sh
brew install ollama
ollama pull gemma4:e4b-it-qat
ollama serve          # 127.0.0.1:11434 で待ち受け（外部には公開しない）
```

### モデルの選定（MacBook Air M2 / 16GB）

既定のモデルは **`gemma4:e4b-it-qat`**（6.1GB）です。2026-11 時点で 16GB の Mac に載る候補を、このアプリの実際のプロンプト
（`/api/explain` 経由、単語 1 件・英文 2 件）で比較して選びました。

| モデル | 最初の文字まで | 生成速度 | 品質 | メモリ |
| --- | --- | --- | --- | --- |
| **gemma4:e4b-it-qat** | **0.1〜1.2 秒** | 約 45〜50 字/秒 | 良い（細かな文法用語の揺れは時々ある） | 6.1GB |
| gemma4:e4b（MLX 版） | 0.2〜1.0 秒 | 約 50〜60 字/秒 | 上と同程度 | 9.4GB |
| qwen3.5:9b | 3〜5 秒 | 約 30 字/秒 | 和訳に誤りがあった | 7.6GB |
| gemma4:12b-it-qat | 約 6 秒 | 約 15 字/秒 | 最も良い（関係詞の先行詞なども正確） | 7.2GB |

- 「1〜2 秒以内に解説が流れ始める」を満たすのは e4b 系だけでした。品質を優先して待ち時間を許容するなら `gemma4:12b-it-qat` も選べます（`LLM_MODEL` を変えるだけ）
- MLX 版はわずかに速いものの 3GB 多くメモリを使うため、ブラウザやサーバーと同居する 16GB 機では QAT 版を既定にしました
- thinking に対応したモデルでは最初の文字が遅れないよう、`LLM_DISABLE_THINKING=true`（既定）で `reasoning_effort: "none"` を送ります
- 計測時、Mac のスリープで生成が 15 分止まりました。常時稼働させる場合はスリープ防止が必要です（「運用」を参照）

## 開発時の起動

ターミナルを 2 つ使います。

```sh
# 1. サーバー（127.0.0.1:8080）
scripts/setup_env.sh   # .env を作る（GITHUB_REPO は git remote から。トークンは省略可）
cargo run -p paper-reader-server
```

```sh
# 2. フロントエンド（127.0.0.1:8000。/api/ は 8080 へ proxy されます）
cd crates/web
trunk serve
```

ブラウザで http://127.0.0.1:8000 を開き、右上の欄に arXiv の URL（例: `https://arxiv.org/abs/1706.03762`）を入れて「開く」を押します。
本文の単語をダブルクリックするか英文をドラッグで選択すると、右ペインに選択文字列・mode（word / sentence）・段落が表示されます。

### 使い方のヒント

- **文法項目と講座の案内**: 英文を解説すると、解説の前に「どの文法項目が使われているか」がチップで並びます
  - 「📺 関係詞 › （レッスン名）」… 記入したレッスンに対応する項目
  - 「📺 関係詞（パーフェクト英文法）」… レッスン未記入のカテゴリ（カテゴリ単位の案内）
  - 「論文構文：同格の that（講座範囲外）」… 講座の範囲外（論文に多い構文や、記入したレッスンが扱っていない項目）
  - チップを押すと本文の該当箇所がハイライトされ、項目の説明と対応レッスンが出ます。「文法項目」タブでは、
    カテゴリ → 項目 → レッスンの一覧と、解説した英文での出現回数（よく出る順）を見られます
  - 右上の ⚙ で「講座の案内を表示する」をオフにすると、チップは文法項目の名前だけになります

- **付箋**: ヘッダーの 📌 で「ここまで読んだ」を残せます。本文で選択している段落、無ければ画面のいちばん上の段落に貼り、
  本文には琥珀色の印が付きます。次にその論文を開くと付箋の位置から表示します（Mac に保存するので、iPhone と Mac で共通）。
  📌 から「付箋へ移動」「ここに貼り直す」「付箋をはがす」ができ、「見た論文」には「📌 付箋あり」と出ます
- **見た論文**: 開いた論文は「履歴」タブの「見た論文」に残り（最後に開いた順・開いた回数・調べた単語と英文の数）、そこから開けます。
  論文を開く前の「解説」タブにも最近の 5 本が出ます。「…」から一覧から外す（保存した本文と図も消すこともできる）
- **論文の保存**: 開いた論文の本文と図は Mac（SQLite）に保存され、2 回目からは arXiv にアクセスせずに表示します
- **論文を開く・探す**: 解説ペイン上部の欄に、arXiv の URL・ID（`1706.03762`、`arXiv:2301.12345v2` など）を入れると「開く」、
  それ以外の語を入れると「検索」になります。検索結果は「検索」タブに並び、「読む」かタイトルで開きます
  - 題名をそのまま入れると題名で探します（`attention is all you need`）。`"..."` で語句、`ti:` `au:` `abs:` `cat:` で欄を指定できます
  - 並び順は「関連度」と「新着」。「さらに表示」で続きを読み込みます

- **単語の辞書と AI 訳**: 単語を選ぶとローカル辞書（EJDict）で引きます。辞書に無い語（modalities など）は、
  ローカル LLM が文脈に合わせて訳し、「AI 訳」と明記して出します（誤りを含むことがあります）
  - **カタカナの読み**: 見出し語の横に、英語の発音をカタカナで出します（ローカル LLM の推定。「読みは AI 推定」と表示）。
    ⚙ の「単語の読みをカタカナで表示する」でオフにできます
  - **発音**: 見出し語の横の 🔊 で、端末の音声合成（Web Speech API, en-US）で読み上げます。
    ネットワークもサーバーも使わないのですぐ鳴ります（iPhone ではマナーモードだと鳴らないことがあります）
  - **品詞**: 訳を品詞（名詞・動詞・形容詞・副詞・前置詞・接続詞）ごとにまとめて表示します。EJDict には品詞の記号が無いため、
    訳語の形（〈C〉〈U〉、「…を〜する」、「〜な」など）と英語の語尾（-tion, -ous, -ly など）から推定しています（誤ることがあります）
  - 一度推定した読み・AI 訳は単語帳（SQLite）に保存し、次からは LLM を呼ばずにすぐ出します
- **直近の単語のメモ**: 解説ペインのいちばん上の 1 行に、最近引いた 10 語を「英語＋短い訳」で新しい順に並べます。
  横にスワイプ（スクロール）すると前の語が見え、タップするとその語をもう一度引きます。
  思い出すための簡単なメモなので、サーバーやデータベースには送らず、ブラウザの localStorage にだけ保存します（⚙ で非表示・消去）
- **単語帳**: 「単語帳」タブに、引いた単語が検索回数・読み・訳と一緒に並びます（回数順・最近・ABC、前方一致で絞り込み）
  - 「＋ 単語を登録」で、辞書に無い語の訳や読みを自分で登録・編集できます（自分で登録した訳は AI 訳より優先）
  - チェックを付けて「選んだ N 語を削除」、または「検索回数の少ない語をまとめて削除」で整理します
  - 上限（`WORD_STORE_MAX`、既定 20,000 語）の 9 割を超えると、整理するか確かめる画面が出ます。
    上限を超えたら検索回数の少ない語（同じなら古い語）から自動で消します。自分で登録した語は自動では消しません
- **解説**: 「解説する」または **E** キー。**Esc** で選択を解除します（解説ペインにフォーカスがあっても使えます）
- **キャッシュ**: 一度解説した文は保存され、次からは LLM を呼ばずにすぐ表示されます（「キャッシュから表示」と出ます）。
  作り直すときは「再生成」を押します
- **履歴**: 「履歴」タブに、論文ごとに調べた単語と解説した文が新しい順に並びます。クリックで再表示（解説がキャッシュにあればそれも表示）、
  × で削除。「すべての論文」に切り替えると、ほかの論文の履歴と「論文を開く」ボタンが出ます
- **配色**: 右上のボタンで 自動（OS の設定に従う）→ ライト → ダーク と切り替わります。論文ペインと解説ペインの両方に効き、ブラウザに保存されます。
  ダークでは図（黒い線の透過画像が多い）に白い下地を敷いて読めるようにしています
- **PC 用とスマホ用**: 画面幅 800px を境に、レイアウトが自動で切り替わります（設定は別々に保存）
  - **PC 用**（左右に並ぶ）: 境界線をドラッグで幅を変更（ダブルクリックで元に戻す、矢印キーでも調整可）
  - **スマホ用**（iPhone 15 などの縦画面）: 本文を画面いっぱいに出し、解説パネルは下から出るシートになります。
    つまみを上下にドラッグすると「少し・半分・全体」の 3 段階で止まり、タップで「少し ⇄ 半分」を切り替えます
    - 論文を開くと「少し」（本文を広く読む）、英文を長押しで選ぶと「半分」、解説・検索・文字入力・フィードバックでは「全体」に自動で動きます
    - 単語を選んだときは「少し」のまま、シートの上に単語・読み・訳を小さく出します（本文を隠さない）。「詳しく」かタップで「半分」に上げて辞書の全体と解説を出します
    - 論文を開いている間は、論文の URL・検索の入力欄を隠します。論文を探すときは「検索」タブを開くと入力欄が出ます
    - 選択中のテキストの欄に「解説する」ボタンが出ます。ボタンや文字はスマホでは大きめ、見出しは記号だけの省スペース表示です
    - ノッチ・ホームインジケーターを避けて表示し（safe area）、ダブルタップでの拡大は止めています。横に長い表は横スクロールします
    - Safari の共有メニューから「ホーム画面に追加」すると、ブラウザの枠なしで開けます

### サーバーだけで配信する

```sh
(cd crates/web && trunk build)
cargo run -p paper-reader-server   # http://127.0.0.1:8080 で UI も配信
```

## 文法タグと講座のレッスン対応

英文の解説では、まず LLM が英文の文法項目を判定し（タグ付け）、受講中の講座

- **講座の内容はリポジトリにも LLM のプロンプトにも入れません。** 扱うのはカテゴリ名と、自分で書き写したレッスンのタイトルだけです。
  解説の中身は LLM が一般的な英文法の知識で書きます
- [data/grammar_tags.yaml](data/grammar_tags.yaml) … 文法タグのマスター（id・日本語名・カテゴリ・別名・LLM 向けの 1 行説明・implies）。
  講座の 17 分類と「論文構文」に分けた 62 項目。`implies` は「完了形の分詞構文 → 分詞構文」のように一緒に付ける一般的な項目
- [data/grammar_map.template.yaml](data/grammar_map.template.yaml) … レッスン対応のテンプレート。17 カテゴリごとに `lessons: []` と記入例
- `data/grammar_map.local.yaml` … 自分で記入する実ファイル（git 管理外）。無ければテンプレートを読み、カテゴリ単位の案内だけを出します

```sh
cp data/grammar_map.template.yaml data/grammar_map.local.yaml
# lessons に { id, category, title（書き写したタイトル）, tags: [grammar_tags の id] } を追記する
curl -X POST http://127.0.0.1:8080/api/admin/reload-grammar   # 開発時: 再起動せずに読み直す
deploy/install.sh --no-build                                  # 常時稼働の配置先にもコピーして再起動する
```

- 起動時（と読み直し時）に検証し、存在しないタグ id があれば `data/grammar_map.local.yaml:12: レッスン「…」のタグ「…」は grammar_tags.yaml にありません`
  のようにファイル名と行を示して起動を止めます（読み直しのときは今のデータを使い続けます）
- 範囲内・外の判定: 「論文構文」のタグは常に範囲外。レッスンを書いたカテゴリでは、どのレッスンからも参照されていないタグは範囲外。
  レッスンをまだ書いていないカテゴリのタグはカテゴリ単位の案内を出します
- `/api/admin/reload-grammar` は Mac 上から直接（127.0.0.1）の要求だけを受け付けます。tailscale serve 経由の要求も接続元は
  127.0.0.1 になるため、転送ヘッダー（X-Forwarded-For / Forwarded / Tailscale-*）が付いた要求は拒否します

タグ付けの仕組み:

- 解説の前に、ストリーミングなしの問い合わせでタグを判定します（構造化出力 `response_format: json_schema`、`tag_id` はタグ id の enum）。
  ランタイムが json_schema を受け付けなければ「JSON だけを出力」させてサーバーで検証します。不明な id・壊れた JSON は捨て、
  タグ無しで解説を続けます。確信度 0.5 未満と重複を除いて最大 5 件
- タグ付けと解説は一連の処理として LLM の同時実行 1 件に数えます。解説のプロンプトには検出した項目名と該当箇所を渡し、
  「文法ポイント」ではその名前を小見出しに使わせます
- 解説キャッシュにはタグも保存し、ヒット時は `tags` も再送します（レッスン対応はその時点の grammar_map で解決し直す）。
  キャッシュのキーには grammar_tags.yaml の内容のハッシュを含めるので、タグを編集すると英文の解説は作り直されます
- 英文は ① 和訳（短いプロンプト）→ ② 文法タグ付け → ③ 文構造・文法ポイント・語句（和訳とタグを渡す）の順に、同じ順番待ちの中で問い合わせます。
  和訳はタグに依らないので先に流し、最初の文字が出るまでを約 1 秒にしています（以前はタグ付けの JSON の生成を待つため 7〜13 秒）。
  チップはタグ付けが終わった時点（7〜15 秒ほど）で出ます（gemma4:e4b-it-qat、M2 16GB で計測）
- 論文を開いたとき、LLM が空いていれば、タグ付け（約 3,000 トークン）と解説のプロンプトを LLM に読み込ませておきます（5 分に 1 回まで）。
  プロンプトのキャッシュが冷えていると、最初のタグ付けだけで 15 秒ほどかかるためです。Ollama は
  `OLLAMA_NUM_PARALLEL=2` にして、タグ付けと解説のプロンプトがそれぞれキャッシュされるようにしています

## 不具合・要望を送る（GitHub Issue）

右上の 💬（または ⚙ の「不具合・要望を送る…」）から、気づいた不具合や欲しい機能を書いて送れます。

1. 内容を書く（タイトルは空欄でよい）。「状況を添付する」をオンにすると、開いている論文・選択中のテキスト・ブラウザの種類も載ります
2. 「AI でラベルを付けて確認」… ローカル LLM が種類（`bug` / `enhancement` / `question`）と機能の場所
   （`area:paper` `area:search` `area:dict` `area:explain` `area:grammar` `area:history` `area:ui` `area:mobile` `area:ops`）
   のラベルを選び、タイトルが空なら要約を付けます。ラベルは押して付け外しでき、送られる本文もそのまま表示されます
3. 「GitHub に Issue を作成」… Issue になり、リンクが出ます。`from-app` ラベルが必ず付きます
   （LLM がラベルを付けられなかったときは `needs-triage`）

GitHub が未設定のとき・送信に失敗したときは、この Mac の SQLite に「未送信」として残ります。設定してサーバーを再起動すると自動で送られ、
💬 の画面の「送ったフィードバック」から手動で再送もできます。ラベルは最初の送信時にリポジトリへ作られます（色と説明つき）。

### GitHub の設定

1. GitHub の Settings → Developer settings → Personal access tokens → **Fine-grained tokens** でトークンを作る
   - Repository access: **Only select repositories** で対象のリポジトリだけ
   - Permissions: Repository permissions の **Issues: Read and write** だけ（Metadata: Read-only は自動で付く）
2. `.env` に書く（`.env` は git 管理外）

   ```sh
   scripts/setup_env.sh --token --apply
   ```

   - `GITHUB_REPO` が空なら git の remote（`origin`）から `owner/name` を入れます（`--remote <name>` で変更可）
   - トークンは画面に出さずに貼り付けます（`pbpaste | scripts/setup_env.sh --token` でも可）。
     コマンドライン引数には載せず、GitHub API でリポジトリと Issues にアクセスできるか確かめてから保存します（`--no-verify` で省略）
   - `.env` は 600（本人だけ読める）にします。`.env.example` に増えた設定は既定値で足し、既にある値は変えません
   - 手で書く場合は `GITHUB_REPO=owner/name` と `GITHUB_TOKEN=github_pat_...`

3. 配置先に反映する … 上の `--apply` が配置先の `.env` を置き換えてサーバーを再起動します
   （`deploy/install.sh --no-build` でも同じく `.env` をコピーします）

- トークンはサーバーにだけ置き、ブラウザや LLM には渡しません。ログの設定表示も `Secret(***)` になります
- 宛先は `https://api.github.com` 固定です。連続送信は 5 秒あけ、本文は 4,000 文字まで
- **公開リポジトリの Issue は誰でも読めます。** 個人的な情報は書かず、添付する内容は確認画面で確かめてください

## Issue から開発する（scripts/issues.py）

開いている Issue を一覧にし、選んだものを Claude Code（`claude -p`）に実装させて PR にします。マージは人が行います。マージすると、自動更新（「自動更新」を参照）が 10 分以内にサーバーへ反映します。

```sh
scripts/issues.py            # 一覧を出し、実施する番号を入力（例: 5 7）
scripts/issues.py 5          # #5 を実施
scripts/issues.py --list     # 一覧だけ
scripts/issues.py 5 --dry-run  # Claude Code に渡すプロンプトを確かめる
```

1 件ずつ次の順に進みます。

1. `main` から `issue-<番号>` ブランチを作り、Issue に `claude:working` を付ける
2. Claude Code が CLAUDE.md のルールに沿って実装する。許すのは編集・`cargo`・`trunk build`・読み取りだけで、
   コミット・push・`curl`・Web へのアクセスはさせない。費用は 1 件 10 ドルまで（`--max-budget-usd`）
3. スクリプトが `cargo fmt --check` / clippy（wasm も）/ `cargo test` を実行する
4. 差分とまとめを見せて確認したら、`Closes #<番号>` を付けてコミットし、push して PR を作る。Issue に PR を知らせ `claude:pr-open` を付ける
5. 変更が無い・チェックに失敗・中止したときは、ブランチに WIP としてコミットして残し（push しない）、`claude:blocked` を付けて理由をコメントする

- 自分以外が書いた Issue は既定では選べません（本文に悪意ある指示が入りうるため。`--allow-others` で許可）。
  Issue の本文は「要望のデータ」としてプロンプトに入れ、本文中の指示より CLAUDE.md のルールを優先させます
- トークン: Issue の読み書きは `.env` の `GITHUB_TOKEN`。PR を作るには、そのリポジトリの **Pull requests: Read and write** だけを持つ
  Fine-grained token を `.env` の `ISSUES_PR_TOKEN` に入れます（無ければ PR を作る URL を表示）。push はいつもの git の認証で行います。
  トークンは Claude Code には渡しません
- 実行の記録（プロンプト・Claude Code の出力・チェックの出力）は `.git/issue-runs/` に残ります
- 始める前に作業ツリーをきれいにしておきます。`main` に push していないコミットがあると PR に含まれるので、先に push してください
- `claude` は PATH・`~/.claude/local`・VS Code 拡張の中から探します。別の場所なら `CLAUDE_BIN` で指定します

## 運用（常時稼働）

MacBook Air 1 台でサーバーと LLM を常時動かし、tailnet 内の別の端末（iPhone など）から HTTPS で使うための手順です。

### 1. 本番ビルド

```sh
deploy/build.sh
```

- サーバーを release ビルドし、フロントエンドを `trunk build --release` でビルドします。
  `index.html` の `data-wasm-opt="z"` により、trunk が wasm-opt（サイズ優先）を自動でダウンロードして適用します
- wasm は約 5.8MB（うちフォント約 1.9MB）で、配信時は brotli / gzip で約 3.2MB に圧縮されます
- `brew install brotli` しておくと、ビルド時に `.br` / `.gz` を最大圧縮で作り（wasm は約 2.6MB）、サーバーはそれをそのまま返します。
  無ければサーバーがその場で圧縮します（`tower-http` の CompressionLayer。SSE は圧縮しません）
- ファイル名にハッシュが付いた wasm / js / css は `Cache-Control: immutable` で長期間キャッシュされ、`index.html` は毎回更新を確認します

### 2. サーバーと Ollama の自動起動（launchd）

```sh
deploy/install.sh                # ビルド → 配置 → LaunchAgent を登録して起動
curl -s http://127.0.0.1:8080/api/health
```

- `~/Library/LaunchAgents` に 2 つの LaunchAgent を入れます。ログイン時に起動し、落ちても 10 秒後に再起動します（`KeepAlive`）
  - `local.paper-reader.server` … サーバー（[deploy/local.paper-reader.server.plist](deploy/local.paper-reader.server.plist)）
  - `local.paper-reader.ollama` … `ollama serve`（`OLLAMA_HOST=127.0.0.1:11434` で外部に公開しない。
    `OLLAMA_KEEP_ALIVE=30m` で最後に使ってから 30 分はモデルをメモリに残す）
- **配置先は `~/Library/Application Support/paper-reader`** です。launchd のジョブは macOS のプライバシー保護により
  `~/Desktop` や `~/Documents` のファイルを読めないため、サーバーのバイナリ・`dist/`・`.env` をそこへコピーします。
  データベースは初回だけ `data/app.db`（辞書・キャッシュ・履歴）をコピーし、以後はそちらを使います
- ログ: `~/Library/Logs/paper-reader/server.log`, `ollama.log`
- 一括操作: 登録したあとは [deploy/ctl.sh](deploy/ctl.sh) でまとめて操作できます（`--caffeinate` で入れた caffeinate も対象）

  ```sh
  deploy/ctl.sh start     # まとめて起動（止まっているものだけ）。最後に /api/health を表示
  deploy/ctl.sh stop      # まとめて停止（次のログインでまた自動起動する）
  deploy/ctl.sh restart   # まとめて再起動
  deploy/ctl.sh status    # 状態と /api/health
  deploy/ctl.sh logs      # ログを追いかけて表示（Ctrl-C で終了）
  ```

- 更新するとき: コードを変えたら `deploy/install.sh` をもう一度実行します（データベースはそのまま）
- 自動起動ごと外す: `deploy/uninstall.sh`（配置先のデータとログは残ります）。一時的に止めるだけなら `deploy/ctl.sh stop`
- Ollama を `brew services start ollama` で動かしたい場合は、`deploy/install.sh --no-ollama` を使います（二重起動しないように）
- 辞書を入れ直すとき: `DATABASE_URL="sqlite://$HOME/Library/Application Support/paper-reader/data/app.db" cargo run -p import-dict --release`

### 自動更新（GitHub の main に入ったら反映する）

```sh
deploy/update.sh --setup    # 一度だけ。配置先に専用の clone を作り、.env などをコピーして LaunchAgent を入れる
deploy/update.sh --status   # 反映している版と GitHub の main
deploy/update.sh --force    # 今の版をビルドし直して反映する
```

- LaunchAgent（`local.paper-reader.update`）が 10 分ごとに GitHub の `main` を確認し、新しいコミットがあれば
  `~/Library/Application Support/paper-reader/src` に取り込んでビルド（`deploy/build.sh`）し、`deploy/install.sh --no-build --no-ollama` で入れ替えて再起動します。
  launchd のジョブは `~/Desktop` を読めないため、開発用の repo とは別の clone を使います
- `origin/main` が今の版を含むとき（早送りできるとき）だけ取り込みます。手元で反映した未 push のコミットを巻き戻さないためです（ログに「main を push してください」と出ます）
- ビルドに失敗したら入れ替えず、今の版のまま動き続けます（同じコミットは `--force` まで試し直さない）。反映後に `/api/health` が返らなければ前の版に戻します
- 電源につながっていないときは自動では更新しません。反映・失敗は macOS の通知で知らせ、反映したコミットの `Closes #N` の Issue に「反映しました」とコメントします
- `.env` と `data/grammar_map.local.yaml` は git 管理外なので、変えたら `deploy/update.sh --setup` をもう一度実行するか、配置先に直接コピーします
- ログ: `~/Library/Logs/paper-reader/update.log`（ビルドは `update-build.log`、配置は `update-install.log`）

### 3. スリープ防止

Mac がスリープすると生成もアクセスも止まります。次のどちらかを使います。

- **caffeinate（おすすめ）**: `deploy/install.sh --caffeinate` で `caffeinate -i -s` の LaunchAgent も入れます。
  電源接続中のシステムスリープとアイドルスリープを防ぎ、ディスプレイは通常どおり消えます。やめるときは `deploy/uninstall.sh` か
  `launchctl bootout gui/$(id -u)/local.paper-reader.caffeinate`
- **pmset**: 電源接続中は眠らない設定にします（管理者権限が必要）

  ```sh
  sudo pmset -c sleep 0 disksleep 0 displaysleep 10   # -c は電源接続時の設定
  pmset -g                                            # 確認
  ```

- どちらの方法でも、**MacBook の蓋を閉じるとスリープします**（外部ディスプレイ・電源・キーボードをつないだクラムシェルモードを除く）。
  蓋を閉じて使うときは次の「蓋を閉じても動かし続ける」を設定します

#### 蓋を閉じても動かし続ける（deploy/lid.sh）

外部ディスプレイなしで蓋を閉じても眠らないように、`pmset disablesleep` を使います（管理者権限が必要）。

```sh
deploy/lid.sh auto     # 電源接続中だけ、蓋を閉じても眠らない（おすすめ）
deploy/lid.sh status   # 今の状態（モード・電源・蓋を閉じたときに眠るか）
deploy/lid.sh normal   # 元に戻す
deploy/lid.sh awake    # 電源に関係なく常に眠らない（非推奨）
```

- `auto` は root の LaunchDaemon `local.paper-reader.lidwake`（[deploy/local.paper-reader.lidwake.plist](deploy/local.paper-reader.lidwake.plist)）を
  `/Library/LaunchDaemons` に入れます。15 秒ごとに電源を確かめ、AC 電源なら `disablesleep 1`、バッテリーなら `0` にします。
  再起動後も有効です。`deploy/uninstall.sh` では外れないので、やめるときは `deploy/lid.sh normal`
- 蓋を閉じるとディスプレイは消えますが、サーバー・Ollama・Tailscale は動き続け、iPhone から使えます
- **電源を抜いてから 15 秒ほどは眠らない設定のまま**です。持ち運ぶときは、電源を抜いて少し待ってから蓋を閉じるか、
  先に `deploy/lid.sh normal` を実行してください。`awake` はバッテリーでも眠らないので、バッグの中で発熱・電池切れになります
- MacBook Air はファンが無いため、蓋を閉じたまま長い生成を続けると熱で速度が落ちることがあります。風通しのよい場所に置いてください
- アイドルスリープも防がれるので、`caffeinate` を併用する必要はありません（入れたままでも害はありません）
- `deploy/ctl.sh status` の最後にも「蓋を閉じると: スリープしない / する」を表示します

### 4. tailnet 内に HTTPS で公開（tailscale serve）

サーバーは 127.0.0.1 だけで待ち受けたまま、[Tailscale](https://tailscale.com) の `serve` で tailnet 内の端末にだけ HTTPS で公開します
（インターネットには公開しない `funnel` は使いません）。

1. Mac に Tailscale を入れてログインする（App Store 版か `brew install --cask tailscale`）。iPhone にも Tailscale アプリを入れて同じアカウントでログインする
2. 管理画面（https://login.tailscale.com/admin/dns）で **MagicDNS** と **HTTPS Certificates** を有効にする
3. Mac で次を実行する

   ```sh
   tailscale serve --bg 8080          # https://<マシン名>.<tailnet名>.ts.net/ → http://127.0.0.1:8080
   tailscale serve status             # 公開中の URL を確認
   ```

   `--bg` を付けると設定が保存され、再起動後も Tailscale が起動すれば自動で有効になります。やめるときは `tailscale serve reset`
4. iPhone の Safari で `https://<マシン名>.<tailnet名>.ts.net/` を開く

- 論文ペインと解説ペインは上下に並びます。単語や英文は長押しで選択します（選択範囲を調整すると解説ペインに反映されます）
- Ollama（11434）は 127.0.0.1 のままで、tailnet にも公開しません

### 再起動後の確認

ログイン後に次がすべて通れば、自動起動が働いています。

```sh
launchctl print gui/$(id -u)/local.paper-reader.server | grep state   # state = running
launchctl print gui/$(id -u)/local.paper-reader.ollama | grep state
curl -s http://127.0.0.1:8080/api/health    # {"server":true,"llm":true,...,"model_available":true}
tailscale serve status
```

## 設定（環境変数 / `.env`）

| 変数 | 既定値 | 説明 |
| --- | --- | --- |
| `BIND_ADDR` | `127.0.0.1:8080` | サーバーの待ち受けアドレス |
| `LLM_BASE_URL` | `http://127.0.0.1:11434/v1` | OpenAI 互換 API（`/api/health` で疎通確認） |
| `LLM_MODEL` | `gemma4:e4b-it-qat` | 解説に使うモデル |
| `LLM_DISABLE_THINKING` | `true` | thinking 対応モデルで推論を無効にする |
| `DATABASE_URL` | `sqlite://data/app.db` | SQLite（無ければ作成） |
| `STATIC_DIR` | `crates/web/dist` | `trunk build` の出力先 |
| `ALLOWED_HOSTS` | `arxiv.org,www.arxiv.org` | 論文取得を許可するホスト（https のみ） |
| `GRAMMAR_TAGS_PATH` | `data/grammar_tags.yaml` | 文法タグのマスター |
| `GRAMMAR_MAP_PATH` | `data/grammar_map.local.yaml` | 講座のレッスン対応（git 管理外） |
| `GRAMMAR_MAP_TEMPLATE_PATH` | `data/grammar_map.template.yaml` | `GRAMMAR_MAP_PATH` が無いときに読むテンプレート |
| `GITHUB_REPO` | （空） | フィードバックを Issue にするリポジトリ `owner/name`。`scripts/setup_env.sh` が git remote から入れる |
| `GITHUB_TOKEN` | （空） | Fine-grained token（そのリポジトリの Issues: Read and write だけ）。ブラウザ・LLM・ログには出さない |

| `WORD_STORE_MAX` | `20000` | 単語帳に残す語数の上限（100〜1,000,000）。超えたら検索回数の少ない語から消す |

`.env` の作成・更新は `scripts/setup_env.sh` で行えます（「GitHub の設定」を参照）。

## API

- `GET /api/paper?url=<arXiv URL>` → `{ id, title, html, source_url, bookmark? }`（bookmark は付箋 `{ block, snippet, updated_at }`）
  - abs / pdf / html の URL から論文 ID を取り出し、`https://arxiv.org/html/<id>` を取得します
  - HTML 版が無い場合は 404 と `{ "error": "..." }`
  - 結果は SQLite の `paper_cache` に論文 ID をキーとして保存します
- `GET /api/dict?word=<単語>` → `{ query, headword, meanings: [string], related: [{ headword, meanings }] }`
  - 前後の記号と所有格の 's を除いて小文字化し、完全一致で検索します
  - 見つからなければ語形変化を還元して順に試します（-ies→-y, -s, -es, -ves→-f/-fe, -ied→-y, -ed/-ing→e/なし,
    子音重ね（running→run）, -ier/-iest→-y, -er/-est）
  - 入力した語そのものが見出し語にある場合（running, used など）も、動詞の原形（run, use）を `related` に入れます。
    辞書の「goの過去」「childの複数形」のような記述からも原形（go, child）を取り出します
  - 見つからない場合は `headword: null`。辞書が未投入なら 503
  - 引くたびに単語帳（word_notes）の検索回数を 1 増やします（見出し語があればその語、無ければ検索語）。
    応答の `note` に単語帳の情報（`kana`, `meanings`, `kana_source` / `meanings_source`: `llm` | `user`, `lookup_count`, `ai_checked`）、
    `store` に使用量 `{ count, max }` を付けます
- `POST /api/dict/ai` body: `{ word, context }` → `/api/dict` と同じ形
  - 辞書に無い語の訳（`meanings`）と、カタカナの読み（`kana`）をローカル LLM に推定させ、単語帳に保存して返します（保存済みなら LLM を呼ばない）
  - 構造化出力（JSON）で受け取り、読みはカタカナだけのとき、訳は 4 個・1 つ 80 文字までを使います。自分で登録した訳・読みは上書きしません
  - LLM への同時リクエストは解説と同じ 1 件の枠を使い、90 秒待っても空かなければ 503。context は 600 文字に切り詰めます
- `GET /api/words?q=<前方一致>&sort=count|recent|alpha&offset=<0〜>` → `{ items: [単語帳の 1 語（dict_summary に辞書の最初の訳）], total, store }`（100 語ずつ）
- `POST /api/words` body: `{ word, kana, meanings }` → 保存した 1 語（自分で登録。空の項目は変えない。meanings は改行・「、」区切り）
- `POST /api/words/delete` body: `{ words: [string] }` → `{ deleted, store }`（1,000 語まで）
- `POST /api/words/prune` body: `{ count }` → `{ deleted, store }`（検索回数の少ない順・同じなら古い順に消す。自分で登録した語は残す）
- `POST /api/explain` body: `{ mode: "word" | "sentence", text, context, regenerate? }` → `text/event-stream`
  - `event: queued`（ほかの解説の生成待ち）→ `event: started` → `event: token`（data は追記する文字列）… → `event: done`
  - 英文は `token`（「## 和訳」と和訳）… → `event: tags` → `token`（文構造・文法ポイント・語句）… の順。キャッシュのヒット時は `cached` → `tags` → `token`（全文）
  - `tags` の data: `[{ tag_id, name, category, span, in_scope, lessons: [{ id, category, title }], hint }]`
  - 失敗時は `event: error`（data はエラーメッセージ）。入力の検証エラーは 400 と `{ "error": "..." }`
  - text は 2,000 文字、context は 4,000 文字まで。LLM への同時リクエストは 1 件で、クライアントが切断すると生成も止めます
  - 最後まで生成できた解説は `explain_cache` に保存します。キーは `sha256(LLM_MODEL, mode, text, context, プロンプトのバージョン)`。
    次に同じ解説を頼まれたら LLM を呼ばずに `event: cached` → `event: token`（全文）→ `event: done` を返します。
    `regenerate: true` ならキャッシュを使わずに生成し、結果で上書きします
- `GET /api/papers` → `{ papers: [{ id, title, source_url, last_opened_at, open_count, word_count, sentence_count, cached, bookmarked }], cache: { papers, html_bytes, assets, asset_bytes } }`
  - 開いた論文の記録（paper_visits）。同じ論文の版違い（`2509.19480` と `2509.19480v1`）は 1 件にまとめる
- `DELETE /api/papers?id=<論文 ID>&purge=<true|false>` → 204（版違いも含めて外す。付箋も消す。purge なら本文と図のキャッシュも消す。単語・英文の履歴は残す）
- `PUT /api/bookmark` body: `{ paper_id, block, snippet }` → `{ block, snippet, updated_at }`（付箋を貼る。1 論文に 1 枚で、貼り直すと置き換える）
- `DELETE /api/bookmark?paper_id=<論文 ID>` → 204（付箋をはがす）
  - 付箋は版なしの論文 ID で `paper_bookmarks` に保存する（版違いでも同じ付箋）。block は本文のブロック要素（段落・見出し・リスト項目など）の通し番号、
    snippet はその書き出し（80 文字まで）。番号がずれたら書き出しで探し直す
- `GET /api/paper-asset?url=<図の URL>` → 画像
  - `/api/paper` が返す HTML の `<img src>` はこの経路に書き換えてある。初回だけ arXiv から取得して SQLite（paper_assets）に保存する
  - 取得先は ALLOWED_HOSTS の https で `/html/` 以下、画像（image/*）だけ、1 枚 15MB まで。`Content-Security-Policy: sandbox` と `nosniff` を付ける
- `GET /api/search?q=<検索語>&start=<0〜>&sort=relevance|newest` → `{ total, start, items: [{ id, title, authors, summary, published, primary_category }] }`
  - arXiv API（`https://export.arxiv.org/api/query`）で 20 件ずつ検索します。宛先は固定で、クエリはサーバーで組み立てます
  - 語はすべて含む（AND）。is / of などのストップワードは除きます（arXiv では AND に入れると 0 件になるため）。
    3 語以上でストップワードを含む入力は題名とみなし、まず語句として探して、0 件ならキーワードで探し直します。題名が完全に一致する論文は先頭に出します
  - arXiv API の利用規約に従って問い合わせは 3 秒に 1 回までにし、同じ検索は 10 分間メモリにキャッシュします
- `GET /api/feedback` → `{ configured, repo, labels: [{ name, color, description, kind }], items: [{ id, created_at, title, labels, status, issue_url, issue_number, error }] }`
- `POST /api/feedback/preview` body: `{ title, body, include_context, context: { paper_id, paper_title, selection, tab, user_agent } }`
  → `{ title, labels, issue_body, llm_labeled }`（LLM のラベル付けは同時実行 1 件の枠を使い、20 秒待っても空かなければラベル無し）
- `POST /api/feedback` body: `{ title, body, labels, include_context, context }` → 保存した 1 件（`status` は `created` か `pending`）
- `POST /api/feedback/<id>/retry` → 送り直した結果（GitHub 未設定なら 400）
- `GET /api/grammar-map` → `{ course, course_short, source: "local" | "template", categories: [{ name, in_course, lesson_count }],
  tags: [{ id, name, category, aliases, hint, in_scope, lessons: [{ id, category, title }], count }] }`（count は履歴の英文での出現回数）
- `POST /api/admin/reload-grammar` → `{ tags, lessons, source }`（127.0.0.1 から直接の要求のみ。それ以外は 403）
- `GET /api/history?paper=<論文 ID>` → `{ entries: [{ id, paper_id, paper_title, mode, text, context, updated_at, has_explanation }] }`
  - 新しい順に最大 200 件。`paper` を省くと全論文。`has_explanation` は今のモデルの解説がキャッシュにあるか
- `POST /api/history` body: `{ paper_id, paper_title, mode, text, context }` → 204（同じ論文・mode・text なら時刻と文脈を更新）
- `DELETE /api/history/<id>` → 204
- `GET /api/health` → `{ server, llm, model, model_available }`（LLM ランタイムの `/models` で疎通とモデルの有無を確認）

## テストと静的解析

```sh
cargo test
# 実際の LLM で例文に期待するタグが付くか（Ollama などが必要）
cargo test -p paper-reader-server -- --ignored --nocapture tagging_examples
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p paper-reader-web --target wasm32-unknown-unknown -- -D warnings
```

## フォント

`crates/web/assets/fonts/NotoSansJP-Regular.otf` は [Noto CJK](https://github.com/notofonts/noto-cjk)（SIL Open Font License 1.1）の
Noto Sans JP を、使う文字だけに絞ったものです（4.5MB → 1.9MB）。wasm に埋め込むので、小さいほど読み込みが速くなります。

含める文字は [scripts/font_chars.py](scripts/font_chars.py) で決めています: JIS X 0208（第1・第2水準漢字、かな、記号）、CP932 の拡張（①、㈱、NEC/IBM 拡張漢字など）、
ASCII・Latin-1・ギリシャ文字・一般句読点・矢印・数学記号・全角形など約 1.1 万字。これ以外の文字（JIS 第3・第4水準の漢字など）は豆腐（□）になります。

作り直す手順:

```sh
pip install fonttools                      # または brew install fonttools
mkdir -p data/fonts
curl -L -o data/fonts/NotoSansJP-Regular.otf \
  https://github.com/notofonts/noto-cjk/raw/main/Sans/SubsetOTF/JP/NotoSansJP-Regular.otf
scripts/subset_font.sh                     # → crates/web/assets/fonts/NotoSansJP-Regular.otf
# 文字を増やすときは scripts/font_chars.py の RANGES を編集して再実行する
```

## ライセンス

このリポジトリのソースコードは [MIT License](LICENSE) の下で公開されています。
