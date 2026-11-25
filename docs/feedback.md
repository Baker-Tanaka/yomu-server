# 不具合・要望を送る（GitHub Issue）

右上の 💬（または ⚙ の「不具合・要望を送る…」）から、気づいた不具合や欲しい機能を書いて送れます。

1. 内容を書く（タイトルは空欄でよい）。「状況を添付する」をオンにすると、開いている論文・選択中のテキスト・ブラウザの種類も載ります
2. 「AI でラベルを付けて確認」… ローカル LLM が種類（`bug` / `enhancement` / `question`）と機能の場所
   （`area:paper` `area:search` `area:dict` `area:explain` `area:grammar` `area:history` `area:ui` `area:mobile` `area:ops`）
   のラベルを選び、タイトルが空なら要約を付けます。ラベルは押して付け外しでき、送られる本文もそのまま表示されます
3. 「GitHub に Issue を作成」… Issue になり、リンクが出ます。`from-app` ラベルが必ず付きます
   （LLM がラベルを付けられなかったときは `needs-triage`）

GitHub が未設定のとき・送信に失敗したときは、この Mac の SQLite に「未送信」として残ります。設定してサーバーを再起動すると自動で送られ、
💬 の画面の「送ったフィードバック」から手動で再送もできます。ラベルは最初の送信時にリポジトリへ作られます（色と説明つき）。

## GitHub の設定

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
