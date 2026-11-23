# paper-reader

英語論文（arXiv の HTML 版）を読むための Web アプリです。左ペインに論文本文を表示し、選択した単語・英文を右ペイン（egui / wasm）に表示します。

- 現在の実装範囲: **フェーズ2**（論文表示と選択の取得、ローカル辞書による単語訳）
- 設計とルール: [CLAUDE.md](CLAUDE.md)

## 構成

| パス | 内容 |
| --- | --- |
| `crates/shared` | API の型（server と web で共有） |
| `crates/server` | axum サーバー（静的配信、`/api/paper`、`/api/dict`、`/api/health`） |
| `crates/web` | eframe アプリ（wasm32-unknown-unknown）、`index.html`、`Trunk.toml`、`assets/` |
| `scripts/import_dict` | 辞書データを SQLite に投入するツール |
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

## 開発時の起動

ターミナルを 2 つ使います。

```sh
# 1. サーバー（127.0.0.1:8080）
cp .env.example .env   # 必要に応じて編集
cargo run -p paper-reader-server
```

```sh
# 2. フロントエンド（127.0.0.1:8000。/api/ は 8080 へ proxy されます）
cd crates/web
trunk serve
```

ブラウザで http://127.0.0.1:8000 を開き、右上の欄に arXiv の URL（例: `https://arxiv.org/abs/1706.03762`）を入れて「開く」を押します。
本文の単語をダブルクリックするか英文をドラッグで選択すると、右ペインに選択文字列・mode（word / sentence）・段落が表示されます。

### サーバーだけで配信する

```sh
(cd crates/web && trunk build)
cargo run -p paper-reader-server   # http://127.0.0.1:8080 で UI も配信
```

## 設定（環境変数 / `.env`）

| 変数 | 既定値 | 説明 |
| --- | --- | --- |
| `BIND_ADDR` | `127.0.0.1:8080` | サーバーの待ち受けアドレス |
| `LLM_BASE_URL` | `http://127.0.0.1:11434/v1` | OpenAI 互換 API（`/api/health` で疎通確認） |
| `LLM_MODEL` | （空） | フェーズ3で使用 |
| `DATABASE_URL` | `sqlite://data/app.db` | SQLite（無ければ作成） |
| `STATIC_DIR` | `crates/web/dist` | `trunk build` の出力先 |
| `ALLOWED_HOSTS` | `arxiv.org,www.arxiv.org` | 論文取得を許可するホスト（https のみ） |

## API

- `GET /api/health` → `{ "server": true, "llm": false }`
- `GET /api/paper?url=<arXiv URL>` → `{ id, title, html, source_url }`
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

## テストと静的解析

```sh
cargo test
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p paper-reader-web --target wasm32-unknown-unknown -- -D warnings
```

## フォント

`crates/web/assets/fonts/NotoSansJP-Regular.otf` は [Noto CJK](https://github.com/notofonts/noto-cjk)（SIL Open Font License 1.1）の日本語サブセット版です。フェーズ4で、使う文字だけに絞ってさらに小さくします。
