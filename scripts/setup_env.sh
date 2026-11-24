#!/bin/bash
# .env を用意する。
#   - 無ければ .env.example から作る（既にある値はそのまま残す）
#   - GITHUB_REPO が空なら git の remote（origin）から owner/name を入れる
#   - GITHUB_TOKEN を画面に出さずに入力し、GitHub API でリポジトリと Issues にアクセスできるか確かめる
#   - .env のパーミッションを 600 にする
#   - --apply で、deploy/install.sh の配置先へコピーしてサーバーを再起動する
#
# 使い方: scripts/setup_env.sh [--token] [--no-verify] [--apply] [--remote <name>]
#   --token       GITHUB_TOKEN を入れ直す（既定では未設定のときだけ聞く）
#   --no-verify   GitHub API での確認を省く
#   --apply       配置先（~/Library/Application Support/paper-reader）の .env を置き換えてサーバーを再起動する
#   --remote      GITHUB_REPO を取り出す remote（既定: origin）
#
# トークンは標準入力からも渡せる: pbpaste | scripts/setup_env.sh --token
set -euo pipefail
cd "$(dirname "$0")/.."

env_file=.env
remote=origin
ask_token=0 verify=1 apply=0
while [ $# -gt 0 ]; do
  case "$1" in
    --token) ask_token=1 ;;
    --no-verify) verify=0 ;;
    --apply) apply=1 ;;
    --remote) remote="${2:?--remote には remote 名が必要です}"; shift ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "不明な引数: $1" >&2; exit 2 ;;
  esac
  shift
done

umask 077

# KEY の値を取り出す（行頭の KEY= だけを見る。前後の空白と引用符は外す）
get_kv() {
  awk -v k="$1" '
    index($0, k "=") == 1 {
      v = substr($0, length(k) + 2)
      gsub(/^[ \t]+|[ \t]+$/, "", v)
      if (v ~ /^".*"$/ || v ~ /^\x27.*\x27$/) v = substr(v, 2, length(v) - 2)
      val = v
    }
    END { printf "%s", val }
  ' "$env_file"
}

# KEY=value を書き換える（無ければ末尾に足す）。値は環境変数で渡し、sed の特殊文字やコマンドラインへの露出を避ける
set_kv() {
  local tmp
  tmp="$(mktemp "${env_file}.XXXXXX")"
  K="$1" V="$2" awk '
    BEGIN { k = ENVIRON["K"]; v = ENVIRON["V"] }
    index($0, k "=") == 1 { if (!done) print k "=" v; done = 1; next }
    { print }
    END { if (!done) print k "=" v }
  ' "$env_file" > "$tmp"
  chmod 600 "$tmp"
  mv -f "$tmp" "$env_file"
}

# git の remote URL から owner/name を取り出す
#   https://github.com/owner/name(.git)  git@github.com:owner/name(.git)  ssh://git@github.com/owner/name(.git)
repo_from_remote() {
  local url
  url="$(git remote get-url "$remote" 2>/dev/null)" || return 1
  url="${url%/}"
  url="${url%.git}"
  case "$url" in
    https://github.com/*) url="${url#https://github.com/}" ;;
    https://*@github.com/*) url="${url#https://*@github.com/}" ;;
    git@github.com:*) url="${url#git@github.com:}" ;;
    ssh://git@github.com/*) url="${url#ssh://git@github.com/}" ;;
    *) return 1 ;;
  esac
  [[ "$url" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] || return 1
  printf '%s' "$url"
}

# --- .env を用意する ---
if [ ! -f "$env_file" ]; then
  cp .env.example "$env_file"
  echo "作成しました: ${env_file}（.env.example から）"
else
  # .env.example に増えた設定を足す（既にある値は変えない）
  added=()
  while IFS= read -r line; do
    case "$line" in ''|'#'*) continue ;; esac
    key="${line%%=*}"
    if ! grep -q "^${key}=" "$env_file"; then
      printf '%s\n' "$line" >> "$env_file"
      added+=("$key")
    fi
  done < .env.example
  if [ ${#added[@]} -gt 0 ]; then
    echo "追加しました: ${added[*]}（.env.example の既定値）"
  fi
fi
chmod 600 "$env_file"

# --- GITHUB_REPO ---
repo="$(get_kv GITHUB_REPO)"
if [ -z "$repo" ]; then
  if repo="$(repo_from_remote)"; then
    set_kv GITHUB_REPO "$repo"
    echo "GITHUB_REPO=${repo}（git remote ${remote} から）"
  else
    repo=""
    echo "git remote ${remote} から GitHub のリポジトリを読み取れません。.env の GITHUB_REPO=owner/name を手で書いてください。" >&2
  fi
else
  echo "GITHUB_REPO=${repo}（設定済み）"
  if r="$(repo_from_remote)" && [ "$r" != "$repo" ]; then
    echo "  ※ git remote ${remote} は ${r} です。変えるなら .env を直接編集してください。"
  fi
fi

# --- GITHUB_TOKEN ---
token="$(get_kv GITHUB_TOKEN)"
if [ -z "$token" ] || [ "$ask_token" = 1 ]; then
  new=""
  if [ -t 0 ]; then
    echo
    echo "GitHub の Fine-grained token を貼り付けて Enter（画面には出ません。空のまま Enter で省略）"
    echo "  作り方: https://github.com/settings/personal-access-tokens/new"
    echo "  Repository access: Only select repositories → ${repo:-対象のリポジトリ}"
    echo "  Permissions: Repository permissions → Issues: Read and write（ほかは付けない）"
    printf 'GITHUB_TOKEN: '
    IFS= read -rs new || true
    echo
  elif [ "$ask_token" = 1 ]; then
    IFS= read -r new || true
  fi
  new="$(printf '%s' "$new" | tr -d '[:space:]')"
  if [ -n "$new" ]; then
    case "$new" in
      github_pat_*) ;;
      ghp_*|gho_*|ghu_*)
        echo "  ※ classic / OAuth のトークンです。権限を絞れる Fine-grained token（github_pat_…）を推奨します。" ;;
      *)
        echo "GitHub のトークンの形ではありません（github_pat_ で始まるはずです）。保存しませんでした。" >&2
        exit 1 ;;
    esac
    token="$new"
    set_kv GITHUB_TOKEN "$token"
    echo "GITHUB_TOKEN を保存しました（${env_file}, パーミッション 600）"
  elif [ -z "$token" ]; then
    echo "GITHUB_TOKEN は未設定のままです（フィードバックはこの Mac に保存だけします）。あとで scripts/setup_env.sh --token で設定できます。"
  fi
else
  echo "GITHUB_TOKEN=***（設定済み。入れ直すなら --token）"
fi

# --- GitHub API で確かめる ---
# トークンはコマンドライン引数に載せない（curl の -H @- で標準入力からヘッダーを渡す）
gh_get() {
  printf 'Authorization: Bearer %s\n' "$token" |
    curl -sS --max-time 15 --proto '=https' -o "$2" -w '%{http_code}' \
      -H @- -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' \
      "https://api.github.com$1"
}

if [ "$verify" = 1 ] && [ -n "$repo" ] && [ -n "$token" ]; then
  echo
  echo "GitHub API で確認しています…"
  body="$(mktemp)"
  trap 'rm -f "$body"' EXIT
  code="$(gh_get "/repos/${repo}" "$body" || echo 000)"
  case "$code" in
    200) ;;
    401) echo "NG: トークンが無効か期限切れです（HTTP 401）" >&2; exit 1 ;;
    403|404) echo "NG: ${repo} にアクセスできません（HTTP ${code}）。トークンの Repository access に ${repo} を入れてください" >&2; exit 1 ;;
    000) echo "NG: api.github.com に接続できません" >&2; exit 1 ;;
    *) echo "NG: 想定外の応答です（HTTP ${code}）" >&2; exit 1 ;;
  esac
  if grep -q '"has_issues": *false' "$body"; then
    echo "NG: ${repo} は Issues が無効です（リポジトリの Settings → Features → Issues を有効に）" >&2
    exit 1
  fi
  if grep -q '"private": *false' "$body"; then
    echo "  ※ ${repo} は公開リポジトリです。フィードバックの Issue は誰でも読めます。"
  fi
  code="$(gh_get "/repos/${repo}/issues?per_page=1" "$body" || echo 000)"
  if [ "$code" != 200 ]; then
    echo "NG: Issues を読めません（HTTP ${code}）。トークンに Issues: Read and write を付けてください" >&2
    exit 1
  fi
  # 書き込み権限: 名前が空のラベルを作ろうとする。GitHub は権限を先に確かめるので、
  # 権限が無ければ 403、あれば入力エラーの 422 になる（どちらでも何も作られない）
  code="$(printf 'Authorization: Bearer %s\n' "$token" |
    curl -sS --max-time 15 --proto '=https' -o "$body" -w '%{http_code}' -X POST \
      -H @- -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' \
      -d '{"name":""}' "https://api.github.com/repos/${repo}/labels" || echo 000)"
  case "$code" in
    422) ;;
    403)
      echo "NG: Issues を書き込めません（HTTP 403）。トークンの Permissions で Issues を「Read and write」にしてください" >&2
      echo "  https://github.com/settings/personal-access-tokens で該当トークン → Edit（トークンの値は変わらないので .env はそのまま）" >&2
      exit 1 ;;
    *) echo "NG: 書き込み権限を確かめられません（HTTP ${code}）" >&2; exit 1 ;;
  esac
  echo "OK: ${repo} の Issues を読み書きできます"
fi

# --- 配置先へ反映する ---
app_home="$HOME/Library/Application Support/paper-reader"
if [ "$apply" = 1 ]; then
  if [ ! -d "$app_home" ]; then
    echo "配置先がありません（${app_home}）。先に deploy/install.sh を実行してください。" >&2
    exit 1
  fi
  install -m 600 "$env_file" "$app_home/.env"
  echo "コピーしました: ${app_home}/.env"
  if launchctl print "gui/$(id -u)/local.paper-reader.server" >/dev/null 2>&1; then
    launchctl kickstart -k "gui/$(id -u)/local.paper-reader.server"
    echo "サーバーを再起動しました"
  else
    echo "サーバーは動いていません（deploy/ctl.sh start で起動）"
  fi
elif [ -f "$app_home/.env" ] && ! cmp -s "$env_file" "$app_home/.env"; then
  echo
  echo "配置先の .env と内容が違います。反映するなら scripts/setup_env.sh --apply（または deploy/install.sh --no-build）"
fi
