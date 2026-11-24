#!/bin/bash
# MacBook の蓋を閉じてもスリープさせず、サーバーと LLM を動かし続けるための設定（pmset disablesleep）。
# 外部ディスプレイなしのクラムシェルでも動く。設定の変更には管理者権限（sudo）が要る。
#
# 使い方: deploy/lid.sh <auto|awake|normal|status>
#   auto     電源接続中だけ、蓋を閉じても眠らない（おすすめ）。バッテリーになると元の動作に戻る。
#            root の LaunchDaemon（local.paper-reader.lidwake）を入れ、15 秒ごとに電源を確かめる
#   awake    電源に関係なく、常に眠らない（バッテリーでも眠らないので持ち運び時は要注意）
#   normal   元に戻す（LaunchDaemon を外し、disablesleep 0 にする）
#   status   今の状態を表示する
set -euo pipefail
cd "$(dirname "$0")/.."

label=local.paper-reader.lidwake
daemon="/Library/LaunchDaemons/$label.plist"

sleep_disabled() {
  local v
  v="$(pmset -g | awk '/SleepDisabled/{print $2}')"
  echo "${v:-0}"
}

daemon_loaded() { launchctl print "system/$label" >/dev/null 2>&1; }

remove_daemon() {
  if daemon_loaded; then
    sudo launchctl bootout "system/$label" 2>/dev/null || true
  fi
  if [ -f "$daemon" ]; then
    sudo rm -f "$daemon"
  fi
}

cmd_auto() {
  plutil -lint "deploy/$label.plist" >/dev/null
  remove_daemon
  # root が実行する定義なので、root 所有・他者書き込み不可で置く
  sudo install -o root -g wheel -m 644 "deploy/$label.plist" "$daemon"
  sudo launchctl bootstrap system "$daemon"
  sleep 1
  echo "設定しました: 電源接続中は蓋を閉じてもスリープしません（バッテリーでは通常どおり）"
  cmd_status
}

cmd_awake() {
  remove_daemon
  sudo pmset -a disablesleep 1
  echo "設定しました: 電源に関係なく、蓋を閉じてもスリープしません"
  echo "注意: バッグに入れても眠りません。持ち運ぶ前に deploy/lid.sh normal で戻してください"
  cmd_status
}

cmd_normal() {
  remove_daemon
  sudo pmset -a disablesleep 0
  echo "元に戻しました: 蓋を閉じるとスリープします"
  cmd_status
}

cmd_status() {
  local mode power
  if daemon_loaded || [ -f "$daemon" ]; then
    mode="auto（電源接続中だけ眠らない）"
  elif [ "$(sleep_disabled)" = 1 ]; then
    mode="awake（常に眠らない）"
  else
    mode="normal（蓋を閉じると眠る）"
  fi
  power="$(pmset -g ps | head -n 1 | sed -E "s/.*'(.*)'.*/\1/")"
  echo "モード: ${mode}"
  echo "電源: ${power}"
  if [ "$(sleep_disabled)" = 1 ]; then
    echo "いま蓋を閉じると: スリープしない（SleepDisabled = 1）"
  else
    echo "いま蓋を閉じると: スリープする（SleepDisabled = 0）"
  fi
}

case "${1:-}" in
  auto) cmd_auto ;;
  awake) cmd_awake ;;
  normal) cmd_normal ;;
  status) cmd_status ;;
  *)
    echo "使い方: $0 <auto|awake|normal|status>" >&2
    exit 2
    ;;
esac
