#!/bin/bash
# 本番用の成果物を ~/Library/Application Support/paper-reader にコピーし、
# LaunchAgent を ~/Library/LaunchAgents に入れて起動する（ログイン時に自動起動、落ちたら再起動）。
# 2 回目以降はサーバーとフロントエンドを更新して再起動する（データベースはそのまま）。
#
# 使い方: deploy/install.sh [--no-build] [--no-ollama] [--caffeinate]
#   --no-build    ビルドを省く（target/release と crates/web/dist が既にあるとき）
#   --no-ollama   Ollama の LaunchAgent を入れない（brew services などで別に動かす場合）
#   --caffeinate  スリープ防止（caffeinate -i -s）の LaunchAgent も入れる
set -euo pipefail
cd "$(dirname "$0")/.."
repo="$(pwd)"
app_home="$HOME/Library/Application Support/paper-reader"
agents="$HOME/Library/LaunchAgents"
log_dir="$HOME/Library/Logs/paper-reader"
domain="gui/$(id -u)"

build=1 ollama=1 caffeinate=0
for arg in "$@"; do
  case "$arg" in
    --no-build) build=0 ;;
    --no-ollama) ollama=0 ;;
    --caffeinate) caffeinate=1 ;;
    *) echo "不明な引数: ${arg}" >&2; exit 2 ;;
  esac
done

if [ "$build" = 1 ]; then
  deploy/build.sh
fi
for f in target/release/paper-reader-server crates/web/dist/index.html; do
  if [ ! -e "$f" ]; then
    echo "${f} がありません。deploy/build.sh を実行してください。" >&2
    exit 1
  fi
done
mkdir -p "$agents" "$log_dir" "$app_home/bin" "$app_home/data"

# 読み込まれていれば止め、取り除かれるまで待つ（すぐ bootstrap すると失敗する）
stop_agent() {
  local name="$1"
  launchctl bootout "$domain/$name" 2>/dev/null || true
  for _ in $(seq 1 50); do
    launchctl print "$domain/$name" >/dev/null 2>&1 || return 0
    sleep 0.2
  done
  echo "${name} を停止できません" >&2
  return 1
}

# plist を置き換えて入れ、すでに読み込まれていれば入れ直す
install_agent() {
  local name="$1"; shift
  local dest="$agents/$name.plist"
  sed "$@" "deploy/$name.plist" > "$dest"
  plutil -lint "$dest" >/dev/null
  stop_agent "$name"
  launchctl bootstrap "$domain" "$dest"
  echo "起動しました: ${name}（${dest}）"
}

esc() { printf '%s' "$1" | sed 's/[&|\\]/\\&/g'; }

if [ "$ollama" = 1 ]; then
  ollama_bin="$(command -v ollama || true)"
  if [ -z "$ollama_bin" ]; then
    echo "ollama が見つかりません（brew install ollama）。--no-ollama で省けます。" >&2
    exit 1
  fi
  if brew services list 2>/dev/null | grep -q '^ollama .*started'; then
    echo "brew services の ollama が動いています。二重起動を避けるため brew services stop ollama を実行してください。" >&2
    exit 1
  fi
  install_agent local.paper-reader.ollama \
    -e "s|__OLLAMA__|$(esc "$ollama_bin")|g" -e "s|__LOG_DIR__|$(esc "$log_dir")|g"
fi

# サーバーを止めてから成果物を入れ替える
stop_agent local.paper-reader.server
cp target/release/paper-reader-server "$app_home/bin/paper-reader-server.new"
mv -f "$app_home/bin/paper-reader-server.new" "$app_home/bin/paper-reader-server"
rm -rf "$app_home/dist.new"
cp -R crates/web/dist "$app_home/dist.new"
rm -rf "$app_home/dist"
mv "$app_home/dist.new" "$app_home/dist"
# .env はトークンを含むので本人だけが読めるようにする
if [ -f .env ]; then
  install -m 600 .env "$app_home/.env"
fi
# 文法タグのマスター（サーバーは配置先の data/ を読む）
cp data/grammar_tags.yaml "$app_home/data/"
# 初回だけ、開発用のデータベース（辞書・キャッシュ・履歴）を引き継ぐ
if [ ! -f "$app_home/data/app.db" ] && [ -f data/app.db ]; then
  sqlite3 data/app.db ".backup '$app_home/data/app.db'"
  echo "data/app.db をコピーしました"
fi

install_agent local.paper-reader.server \
  -e "s|__APP_HOME__|$(esc "$app_home")|g" -e "s|__LOG_DIR__|$(esc "$log_dir")|g"

if [ "$caffeinate" = 1 ]; then
  install_agent local.paper-reader.caffeinate
fi

echo "配置先: ${app_home}"
echo "ログ: ${log_dir}"
echo "確認: curl -s http://127.0.0.1:8080/api/health"
