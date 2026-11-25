# 運用（常時稼働）

MacBook Air 1 台でサーバーと LLM を常時動かし、tailnet 内の別の端末（iPhone など）から HTTPS で使うための手順です。

## 1. 本番ビルド

```sh
deploy/build.sh
```

- サーバーを release ビルドし、フロントエンドを `trunk build --release` でビルドします。
  `index.html` の `data-wasm-opt="z"` により、trunk が wasm-opt（サイズ優先）を自動でダウンロードして適用します
- wasm は約 5.8MB（うちフォント約 1.9MB）で、配信時は brotli / gzip で約 3.2MB に圧縮されます
- `brew install brotli` しておくと、ビルド時に `.br` / `.gz` を最大圧縮で作り（wasm は約 2.6MB）、サーバーはそれをそのまま返します。
  無ければサーバーがその場で圧縮します（`tower-http` の CompressionLayer。SSE は圧縮しません）
- ファイル名にハッシュが付いた wasm / js / css は `Cache-Control: immutable` で長期間キャッシュされ、`index.html` は毎回更新を確認します

## 2. サーバーと Ollama の自動起動（launchd）

```sh
deploy/install.sh                # ビルド → 配置 → LaunchAgent を登録して起動
curl -s http://127.0.0.1:8080/api/health
```

- `~/Library/LaunchAgents` に 2 つの LaunchAgent を入れます。ログイン時に起動し、落ちても 10 秒後に再起動します（`KeepAlive`）
  - `local.paper-reader.server` … サーバー（[deploy/local.paper-reader.server.plist](../deploy/local.paper-reader.server.plist)）
  - `local.paper-reader.ollama` … `ollama serve`（`OLLAMA_HOST=127.0.0.1:11434` で外部に公開しない。
    `OLLAMA_KEEP_ALIVE=30m` で最後に使ってから 30 分はモデルをメモリに残す）
- **配置先は `~/Library/Application Support/paper-reader`** です。launchd のジョブは macOS のプライバシー保護により
  `~/Desktop` や `~/Documents` のファイルを読めないため、サーバーのバイナリ・`dist/`・`.env` をそこへコピーします。
  データベースは初回だけ `data/app.db`（辞書・キャッシュ・履歴）をコピーし、以後はそちらを使います
- ログ: `~/Library/Logs/paper-reader/server.log`, `ollama.log`
- 一括操作: 登録したあとは [deploy/ctl.sh](../deploy/ctl.sh) でまとめて操作できます（`--caffeinate` で入れた caffeinate も対象）

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

## 自動更新（GitHub の main に入ったら反映する）

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

## 3. スリープ防止

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

### 蓋を閉じても動かし続ける（deploy/lid.sh）

外部ディスプレイなしで蓋を閉じても眠らないように、`pmset disablesleep` を使います（管理者権限が必要）。

```sh
deploy/lid.sh auto     # 電源接続中だけ、蓋を閉じても眠らない（おすすめ）
deploy/lid.sh status   # 今の状態（モード・電源・蓋を閉じたときに眠るか）
deploy/lid.sh normal   # 元に戻す
deploy/lid.sh awake    # 電源に関係なく常に眠らない（非推奨）
```

- `auto` は root の LaunchDaemon `local.paper-reader.lidwake`（[deploy/local.paper-reader.lidwake.plist](../deploy/local.paper-reader.lidwake.plist)）を
  `/Library/LaunchDaemons` に入れます。15 秒ごとに電源を確かめ、AC 電源なら `disablesleep 1`、バッテリーなら `0` にします。
  再起動後も有効です。`deploy/uninstall.sh` では外れないので、やめるときは `deploy/lid.sh normal`
- 蓋を閉じるとディスプレイは消えますが、サーバー・Ollama・Tailscale は動き続け、iPhone から使えます
- **電源を抜いてから 15 秒ほどは眠らない設定のまま**です。持ち運ぶときは、電源を抜いて少し待ってから蓋を閉じるか、
  先に `deploy/lid.sh normal` を実行してください。`awake` はバッテリーでも眠らないので、バッグの中で発熱・電池切れになります
- MacBook Air はファンが無いため、蓋を閉じたまま長い生成を続けると熱で速度が落ちることがあります。風通しのよい場所に置いてください
- アイドルスリープも防がれるので、`caffeinate` を併用する必要はありません（入れたままでも害はありません）
- `deploy/ctl.sh status` の最後にも「蓋を閉じると: スリープしない / する」を表示します

## 4. tailnet 内に HTTPS で公開（tailscale serve）

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

## 再起動後の確認

ログイン後に次がすべて通れば、自動起動が働いています。

```sh
launchctl print gui/$(id -u)/local.paper-reader.server | grep state   # state = running
launchctl print gui/$(id -u)/local.paper-reader.ollama | grep state
curl -s http://127.0.0.1:8080/api/health    # {"server":true,"llm":true,...,"model_available":true}
tailscale serve status
```
