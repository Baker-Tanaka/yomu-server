# paper-reader

英語論文（arXiv の HTML 版）を読むための Web アプリです。左ペインに論文本文を表示し、選択した単語・英文を右ペイン（egui / wasm）に表示します。

- 現在の実装範囲: **フェーズ1**（論文表示と選択の取得）
- 設計とルール: [CLAUDE.md](CLAUDE.md)

## 構成

| パス | 内容 |
| --- | --- |
| `crates/shared` | API の型（server と web で共有） |
| `crates/server` | axum サーバー（静的配信、`/api/paper`、`/api/health`） |
| `crates/web` | eframe アプリ（wasm32-unknown-unknown）、`index.html`、`Trunk.toml`、`assets/` |
| `data/` | SQLite ファイルなど（git 管理外） |

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

## API（フェーズ1）

- `GET /api/health` → `{ "server": true, "llm": false }`
- `GET /api/paper?url=<arXiv URL>` → `{ id, title, html, source_url }`
  - abs / pdf / html の URL から論文 ID を取り出し、`https://arxiv.org/html/<id>` を取得します
  - HTML 版が無い場合は 404 と `{ "error": "..." }`
  - 結果は SQLite の `paper_cache` に論文 ID をキーとして保存します

## テストと静的解析

```sh
cargo test
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p paper-reader-web --target wasm32-unknown-unknown -- -D warnings
```

## フォント

`crates/web/assets/fonts/NotoSansJP-Regular.otf` は [Noto CJK](https://github.com/notofonts/noto-cjk)（SIL Open Font License 1.1）の日本語サブセット版です。フェーズ4で、使う文字だけに絞ってさらに小さくします。
