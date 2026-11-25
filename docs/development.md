# 開発

設計と守るべきルールは [CLAUDE.md](../CLAUDE.md) にまとめています。起動の手順は [セットアップ](setup.md#開発時の起動) を参照してください。

## 構成

| パス | 内容 |
| --- | --- |
| `crates/shared` | API の型（server と web で共有） |
| `crates/server` | axum サーバー（静的配信、`/api/paper`、`/api/search`、`/api/dict`、`/api/words`、`/api/explain`、`/api/history`、`/api/health`） |
| `crates/web` | eframe アプリ（wasm32-unknown-unknown）、`index.html`、`Trunk.toml`、`assets/` |
| `scripts/import_dict` | 辞書データを SQLite に投入するツール |
| `scripts/issues.py` | GitHub の Issue を選んで Claude Code に実装させ、PR にするスクリプト（下の「Issue から開発する」を参照） |
| `scripts/subset_font.sh` | 日本語フォントを使う文字だけに絞るスクリプト |
| `deploy/` | 本番ビルド、launchd の plist、インストール用スクリプト |
| `data/` | 辞書の元データ、SQLite ファイル（git 管理外） |

## テストと静的解析

```sh
cargo test
# 実際の LLM で例文に期待するタグが付くか（Ollama などが必要）
cargo test -p paper-reader-server -- --ignored --nocapture tagging_examples
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p paper-reader-web --target wasm32-unknown-unknown -- -D warnings
```

## Issue から開発する（scripts/issues.py）

開いている Issue を一覧にし、選んだものを Claude Code（`claude -p`）に実装させて PR にします。マージは人が行います。マージすると、自動更新（[自動更新](operations.md#自動更新github-の-main-に入ったら反映する) を参照）が 10 分以内にサーバーへ反映します。

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

## フォント

`crates/web/assets/fonts/NotoSansJP-Regular.otf` は [Noto CJK](https://github.com/notofonts/noto-cjk)（SIL Open Font License 1.1）の
Noto Sans JP を、使う文字だけに絞ったものです（4.5MB → 1.9MB）。wasm に埋め込むので、小さいほど読み込みが速くなります。

含める文字は [scripts/font_chars.py](../scripts/font_chars.py) で決めています: JIS X 0208（第1・第2水準漢字、かな、記号）、CP932 の拡張（①、㈱、NEC/IBM 拡張漢字など）、
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
