# API

型は [crates/shared](../crates/shared) に定義しています。

- `GET /api/paper?url=<arXiv URL>` → `{ id, title, html, source_url, bookmark? }`（bookmark は付箋 `{ block, offset, snippet, updated_at }`）
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
- `POST /api/explain` body: `{ mode: "word" | "sentence" | "formula", text, context, regenerate?, slash? }`（`formula` の `text` は数式の LaTeX。単語と同じく 1 回の生成で、`tags` は送らない） → `text/event-stream`
  - `slash: true` なら英文の解説の先頭（和訳の次）に「## スラッシュリーディング」を入れる。キャッシュのキーには有無を含めるが、
    片方しか無ければそれを返す（web は設定に合わせて節を隠すか、「付けて解説し直す」を出す）
  - `event: queued`（ほかの解説の生成待ち）→ `event: started` → `event: token`（data は追記する文字列）… → `event: done`
  - 登録した知識（RAG）が対象に近ければ、最初に `event: knowledge`（data は `[{ id, term }]`）を送る（キャッシュのヒット時も同じ）。
    知識は解説のユーザーメッセージに添え（英文の和訳には添えない）、キャッシュのキーに内容ごと含める
  - 英文は `token`（「## 和訳」と和訳）… → `event: tags` → `token`（文構造・文法ポイント・語句）… の順。キャッシュのヒット時は `cached` → `tags` → `token`（全文）
  - `tags` の data: `[{ tag_id, name, category, span, hint, guide }]`（`guide` は `{ summary, form, examples: [{ en, ja }], toeic, caution }`）
  - 失敗時は `event: error`（data はエラーメッセージ）。入力の検証エラーは 400 と `{ "error": "..." }`
  - text は 2,000 文字、context は 4,000 文字まで。LLM への同時リクエストは 1 件で、クライアントが切断すると生成も止めます
  - 最後まで生成できた解説は `explain_cache` に保存します。キーは `sha256(LLM_MODEL, mode, text, context, プロンプトのバージョン)`。
    次に同じ解説を頼まれたら LLM を呼ばずに `event: cached` → `event: token`（全文）→ `event: done` を返します。
    `regenerate: true` ならキャッシュを使わずに生成し、結果で上書きします
- `POST /api/chat` body: `{ mode, text, context, explanation, messages: [{ role: "user" | "assistant", content }] }` → `text/event-stream`
  - 解説のあとの質問。`messages` は質問と回答が交互で、最後が今回の質問（21 件まで）。会話はサーバーに保存せず、画面が毎回送ります
  - イベントは `queued` → `started` → `token`… → `done`（失敗は `error`）。LLM の同時実行 1 件の枠は解説と共通で、クライアントが切断すると生成も止めます
  - text は 2,000 文字、context は 4,000 文字、質問は 1,000 文字、解説・回答は 12,000 文字まで
  - LLM に渡す分は、コンテキスト長（Ollama の既定は 4096 トークン）に収まるようにサーバーが切り詰めます（解説と文脈を短くし、古いやり取りから除く）
- `GET /api/knowledge` → `{ items: [{ id, term, summary, source_text, paper_id, created_at }], max, embed_model }`（新しい順）
- `POST /api/knowledge` body: `{ mode, text, context, explanation, paper_id? }` → 登録した 1 件。
  表示中の解説から LLM（同時 1 件の枠を使う）が用語と要点（300 字ほど）を JSON で整理し、`EMBED_MODEL` で埋め込みを作って保存する。
  `EMBED_MODEL` が空なら 400、LLM・埋め込みモデルが使えなければ 503。2,000 件まで
- `DELETE /api/knowledge?id=` → `{ deleted }`
- `GET /api/papers` → `{ papers: [{ id, title, source_url, last_opened_at, open_count, word_count, sentence_count, cached, bookmarked }], cache: { papers, html_bytes, assets, asset_bytes } }`
  - 開いた論文の記録（paper_visits）。同じ論文の版違い（`2509.19480` と `2509.19480v1`）は 1 件にまとめる
- `DELETE /api/papers?id=<論文 ID>&purge=<true|false>` → 204（版違いも含めて外す。付箋も消す。purge なら本文と図のキャッシュも消す。単語・英文の履歴は残す）
- `GET /api/read-later` → `{ items: [{ id, title, authors, added_at }] }`（後で読む論文。後から積んだ順）
- `PUT /api/read-later` body: `{ paper_id, title, authors }` → 204（一番上に積む。すでにあれば積み直す。版なしの arXiv ID で `read_later` に保存し、上限 200 件を超える追加は 400）
- `DELETE /api/read-later?paper_id=<論文 ID>` → 204（後で読むから外す）
- `PUT /api/bookmark` body: `{ paper_id, block, offset, snippet }` → `{ block, offset, snippet, updated_at }`（付箋を貼る。1 論文に 1 枚で、貼り直すと置き換える）
- `DELETE /api/bookmark?paper_id=<論文 ID>` → 204（付箋をはがす）
  - 付箋は版なしの論文 ID で `paper_bookmarks` に保存する（版違いでも同じ付箋）。block は本文のブロック要素（段落・見出し・リスト項目など）の通し番号、
    offset はそのブロックの本文の先頭から数えた文字の位置（UTF-16）で、画面はその文字がある行に印を付ける（以前の付箋は 0）。
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
- `GET /api/grammar-tags` → `{ categories: [string], tags: [{ id, name, category, aliases, hint, guide, count }] }`（count は履歴の英文での出現回数）
- `POST /api/admin/reload-grammar` → `{ tags }`（127.0.0.1 から直接の要求のみ。それ以外は 403）
- `GET /api/history?paper=<論文 ID>` → `{ entries: [{ id, paper_id, paper_title, mode, text, context, updated_at, has_explanation }] }`
  - 新しい順に最大 200 件。`paper` を省くと全論文。`has_explanation` は今のモデルの解説がキャッシュにあるか
- `POST /api/history` body: `{ paper_id, paper_title, mode, text, context }` → 204（同じ論文・mode・text なら時刻と文脈を更新）
- `DELETE /api/history/<id>` → 204
- `GET /api/health` → `{ server, llm, model, model_available, version, git_hash, git_date }`（LLM ランタイムの `/models` で疎通とモデルの有無を確認）
  - `version` は Cargo.toml のバージョン、`git_hash` / `git_date` はサーバーをビルドしたコミットの短いハッシュと日付（`crates/shared/build.rs` がビルド時に埋め込みます。
    git が無い環境では空文字。環境変数 `PAPER_READER_GIT_HASH` / `PAPER_READER_GIT_DATE` をビルド時に与えると上書きできます）
  - 画面の ⚙（設定）の下に、画面（wasm）とサーバーのそれぞれのバージョンとハッシュを表示します。食い違っていたら再読み込みを促します。
    フィードバックの Issue に添付する「アプリ」の行にもハッシュが入ります
  - コミットしていない変更はハッシュに表れません（ハッシュはビルドした時点の HEAD）
