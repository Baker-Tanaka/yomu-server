#!/bin/bash
# GitHub の main に新しいコミットがあれば、取り込んでビルドし、配置して再起動する（自動更新）。
#
# launchd のジョブは ~/Desktop を読めないため、配置先の中に専用の clone（__APP_HOME__/src）を持ち、
# そこで origin/main を取り込んでビルドする。LaunchAgent（local.paper-reader.update）が 10 分ごとに実行する。
#
# 使い方:
#   deploy/update.sh --setup   配置先に clone を作り、.env などをコピーして、自動更新の LaunchAgent を入れる
#                              （開発用の repo から一度だけ実行する。そのあとすぐ 1 回更新する）
#   deploy/update.sh           更新があれば反映する（LaunchAgent が実行する）
#   deploy/update.sh --force   更新が無くてもビルドし直して反映する（バッテリー駆動中でも実行する）
#   deploy/update.sh --status  反映している版と、GitHub の main を表示する
#
# 安全のため:
#   - origin/main が今の版を含むとき（早送りできるとき）だけ取り込む（push していないコミットを巻き戻さない）
#   - ビルドに失敗したら何も入れ替えない（今の版が動き続ける）。同じコミットは --force まで試し直さない
#   - 反映後に /api/health が返らなければ、前の版に戻す
#   - 電源につながっていないときは自動では更新しない（ビルドが重いため）
#   - 反映したコミットの "Closes #N" の Issue に「反映しました」とコメントする（配置先の .env の GITHUB_TOKEN）
set -euo pipefail

app_home="$HOME/Library/Application Support/paper-reader"
src="$app_home/src"
agents="$HOME/Library/LaunchAgents"
log_dir="$HOME/Library/Logs/paper-reader"
domain="gui/$(id -u)"
label=local.paper-reader.update
repo_url="https://github.com/Baker-Tanaka/yomu-server.git"

mode=update force=0
for arg in "$@"; do
  case "$arg" in
    --setup) mode=setup ;;
    --status) mode=status ;;
    --force) force=1 ;;
    -h|--help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "不明な引数: ${arg}" >&2; exit 2 ;;
  esac
done

log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $*"; }
notify() {
  osascript -e "display notification \"$1\" with title \"論文リーダー\"" >/dev/null 2>&1 || true
}
# 配置先の .env の値（無ければ空）
env_value() {
  [ -f "$app_home/.env" ] || return 0
  awk -v k="$1" 'index($0, k "=") == 1 { v = substr($0, length(k) + 2); gsub(/^[ \t"\x27]+|[ \t"\x27]+$/, "", v); val = v } END { printf "%s", val }' "$app_home/.env"
}

# ───────────── --setup ─────────────
if [ "$mode" = setup ]; then
  dev="$(cd "$(dirname "$0")/.." && pwd)"
  mkdir -p "$app_home/data" "$log_dir" "$agents"
  if [ ! -d "$src/.git" ]; then
    git clone -q "$repo_url" "$src"
    echo "clone しました: ${src}"
  fi
  # git 管理外の設定は開発用の repo からコピーする（.env はトークンを含むので 600）
  if [ "$dev" != "$src" ]; then
    [ -f "$dev/.env" ] && install -m 600 "$dev/.env" "$app_home/.env" && echo ".env をコピーしました"
  fi
  path="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
  esc() { printf '%s' "$1" | sed 's/[&|\\]/\\&/g'; }
  sed -e "s|__APP_HOME__|$(esc "$app_home")|g" -e "s|__LOG_DIR__|$(esc "$log_dir")|g" \
      -e "s|__PATH__|$(esc "$path")|g" "$src/deploy/$label.plist" > "$agents/$label.plist"
  plutil -lint "$agents/$label.plist" >/dev/null
  launchctl bootout "$domain/$label" 2>/dev/null || true
  sleep 1
  launchctl bootstrap "$domain" "$agents/$label.plist"
  echo "自動更新を入れました（10 分ごとに GitHub の main を確認）: ${agents}/${label}.plist"
  echo "ログ: ${log_dir}/update.log"
  exec "$src/deploy/update.sh" --force
fi

cd "$src"

# ───────────── --status ─────────────
if [ "$mode" = status ]; then
  git fetch -q origin main 2>/dev/null || echo "（GitHub に接続できません）"
  echo "反映している版: $(cat "$app_home/deployed" 2>/dev/null || echo '不明')"
  echo "GitHub の main:  $(git log -1 --format='%h %s' origin/main 2>/dev/null || echo '不明')"
  [ -f "$app_home/update.failed" ] && echo "失敗した版: $(cat "$app_home/update.failed")（--force で試し直す）"
  launchctl print "$domain/$label" >/dev/null 2>&1 && echo "自動更新: 有効" || echo "自動更新: 無効（deploy/update.sh --setup）"
  exit 0
fi

# ───────────── 更新 ─────────────
# 取り込みでこのファイル自体が変わっても動き続けるよう、コピーから実行し直す
if [ -z "${PAPER_READER_UPDATE_COPY:-}" ]; then
  copy="$(mktemp -t paper-reader-update)"
  cp "$0" "$copy"
  PAPER_READER_UPDATE_COPY="$copy" exec /bin/bash "$copy" "$@"
fi

# 二重に動かない（2 時間より古いロックは前回の異常終了とみなす）
lock="$app_home/update.lock"
if ! mkdir "$lock" 2>/dev/null; then
  if [ -n "$(find "$lock" -maxdepth 0 -mmin +120 2>/dev/null)" ]; then
    rm -rf "$lock" && mkdir "$lock"
  else
    exit 0
  fi
fi
trap 'rm -rf "$lock"; rm -f "${PAPER_READER_UPDATE_COPY:-}"' EXIT

if [ "$force" = 0 ] && ! pmset -g batt | head -1 | grep -q 'AC Power'; then
  exit 0
fi

if ! git fetch -q origin main; then
  log "GitHub に接続できません。次回また確認します"
  exit 0
fi
cur="$(git rev-parse HEAD)"
remote="$(git rev-parse origin/main)"
target="$cur"
if [ "$remote" != "$cur" ]; then
  if git merge-base --is-ancestor "$cur" "$remote"; then
    target="$remote"
  elif [ "$force" = 0 ]; then
    # 前回だけ知らせる（10 分ごとに同じログを出さない）
    if [ "$(cat "$app_home/update.diverged" 2>/dev/null)" != "$remote" ]; then
      log "origin/main（${remote:0:7}）が反映中の版（${cur:0:7}）を含んでいないので取り込みません（main を push してください）"
      echo "$remote" > "$app_home/update.diverged"
    fi
    exit 0
  fi
fi
if [ "$target" = "$cur" ] && [ "$force" = 0 ]; then
  exit 0
fi
if [ "$force" = 0 ] && [ "$(cat "$app_home/update.failed" 2>/dev/null)" = "$target" ]; then
  exit 0
fi

log "反映を始めます: ${cur:0:7} → ${target:0:7} $(git log -1 --format=%s "$target")"
git checkout -q --detach "$target"

fail() {
  log "失敗: $1"
  echo "$target" > "$app_home/update.failed"
  git checkout -q --detach "$cur" || true
  notify "更新に失敗しました（${target:0:7}）: $1"
  exit 1
}

if ! deploy/build.sh > "$log_dir/update-build.log" 2>&1; then
  fail "ビルドに失敗しました（${log_dir}/update-build.log）。今の版のまま動いています"
fi

# 戻せるように今の成果物を取っておく
rm -rf "$app_home/rollback"
mkdir -p "$app_home/rollback"
cp "$app_home/bin/paper-reader-server" "$app_home/rollback/" 2>/dev/null || true
cp -R "$app_home/dist" "$app_home/rollback/dist" 2>/dev/null || true

if ! deploy/install.sh --no-build --no-ollama > "$log_dir/update-install.log" 2>&1; then
  fail "配置に失敗しました（${log_dir}/update-install.log）"
fi

healthy=0
for _ in $(seq 1 30); do
  if curl -s --max-time 2 http://127.0.0.1:8080/api/health | grep -q '"server":true'; then
    healthy=1
    break
  fi
  sleep 1
done
if [ "$healthy" = 0 ]; then
  if [ -f "$app_home/rollback/paper-reader-server" ]; then
    cp "$app_home/rollback/paper-reader-server" "$app_home/bin/paper-reader-server"
    rm -rf "$app_home/dist" && cp -R "$app_home/rollback/dist" "$app_home/dist"
    launchctl kickstart -k "$domain/local.paper-reader.server" || true
  fi
  fail "反映後にサーバーが応答しないため、前の版に戻しました（${log_dir}/server.log）"
fi

git log -1 --format='%h %s (%ci)' HEAD > "$app_home/deployed"
rm -f "$app_home/update.failed" "$app_home/update.diverged"
log "反映しました: $(cat "$app_home/deployed")"
notify "新しい版を反映しました: $(git log -1 --format=%s HEAD)"

# 反映したコミットが閉じる Issue に知らせる
token="$(env_value GITHUB_TOKEN)"
repo="$(env_value GITHUB_REPO)"
if [ "$cur" != "$target" ] && [ -n "$token" ] && [ -n "$repo" ]; then
  short="$(git rev-parse --short HEAD)"
  for n in $(git log --format=%B "$cur..$target" | grep -oiE '(closes|fixes|resolves) #[0-9]+' | grep -oE '[0-9]+' | sort -un); do
    body="$(printf '{"body":"🤖 この変更をサーバーに反映しました（%s）。アプリを再読み込みすると使えます。"}' "$short")"
    printf 'Authorization: Bearer %s\n' "$token" |
      curl -sS --max-time 15 --proto '=https' -o /dev/null -X POST -H @- \
        -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' \
        -d "$body" "https://api.github.com/repos/$repo/issues/$n/comments" &&
      log "Issue #${n} に反映を知らせました" || true
  done
fi
