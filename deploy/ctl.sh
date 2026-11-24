#!/bin/bash
# サーバー・Ollama（・caffeinate）を一括で操作する。deploy/install.sh で登録した LaunchAgent が対象。
#
# 使い方: deploy/ctl.sh <start|stop|restart|status|logs>
#   start    まとめて起動する（止まっているものだけ起動）
#   stop     まとめて止める（次のログイン時にはまた自動起動する。自動起動ごとやめるなら deploy/uninstall.sh）
#   restart  まとめて再起動する
#   status   状態と /api/health を表示する
#   logs     サーバーと Ollama のログを追いかけて表示する（Ctrl-C で終了）
set -euo pipefail

domain="gui/$(id -u)"
agents="$HOME/Library/LaunchAgents"
log_dir="$HOME/Library/Logs/paper-reader"
# 起動する順（止めるときは逆順）
names=(local.paper-reader.ollama local.paper-reader.server local.paper-reader.caffeinate)

installed() { [ -f "$agents/$1.plist" ]; }
loaded() { launchctl print "$domain/$1" >/dev/null 2>&1; }

# deploy/install.sh で登録したものだけを対象にする（caffeinate は --caffeinate を付けたときだけ）
targets() {
  local found=0
  for n in "${names[@]}"; do
    if installed "$n"; then
      echo "$n"
      found=1
    fi
  done
  if [ "$found" = 0 ]; then
    echo "LaunchAgent が登録されていません。先に deploy/install.sh を実行してください。" >&2
    exit 1
  fi
}

wait_unloaded() {
  for _ in $(seq 1 50); do
    loaded "$1" || return 0
    sleep 0.2
  done
  echo "${1} を停止できません" >&2
  return 1
}

health() {
  local out
  for _ in $(seq 1 25); do
    if out="$(curl -s --max-time 2 http://127.0.0.1:8080/api/health)"; then
      echo "health: ${out}"
      return 0
    fi
    sleep 0.4
  done
  echo "health: サーバーに接続できません（ログ: ${log_dir}/server.log）"
  return 1
}

cmd_start() {
  local list
  list="$(targets)"
  for n in $list; do
    if loaded "$n"; then
      echo "起動済み: ${n}"
    else
      launchctl bootstrap "$domain" "$agents/$n.plist"
      echo "起動しました: ${n}"
    fi
  done
  health
}

cmd_stop() {
  local list
  list="$(targets)"
  # 逆順に止める（サーバー → Ollama）
  for n in $(echo "$list" | tail -r); do
    if loaded "$n"; then
      launchctl bootout "$domain/$n" 2>/dev/null || true
      wait_unloaded "$n"
      echo "停止しました: ${n}"
    else
      echo "停止済み: ${n}"
    fi
  done
}

cmd_restart() {
  local list
  list="$(targets)"
  for n in $list; do
    if loaded "$n"; then
      launchctl kickstart -k "$domain/$n"
      echo "再起動しました: ${n}"
    else
      launchctl bootstrap "$domain" "$agents/$n.plist"
      echo "起動しました: ${n}"
    fi
  done
  health
}

cmd_status() {
  local list state pid
  list="$(targets)"
  for n in $list; do
    if loaded "$n"; then
      state="$(launchctl print "$domain/$n" | awk -F' = ' '/^\tstate = /{print $2; exit}')"
      pid="$(launchctl print "$domain/$n" | awk -F' = ' '/^\tpid = /{print $2; exit}')"
      echo "${n}: ${state:-unknown}${pid:+ (pid ${pid})}"
    else
      echo "${n}: 停止中"
    fi
  done
  if loaded local.paper-reader.server; then
    health || true
  fi
}

cmd_logs() {
  tail -n 20 -F "$log_dir/server.log" "$log_dir/ollama.log"
}

case "${1:-}" in
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_restart ;;
  status) cmd_status ;;
  logs) cmd_logs ;;
  *)
    echo "使い方: $0 <start|stop|restart|status|logs>" >&2
    exit 2
    ;;
esac
