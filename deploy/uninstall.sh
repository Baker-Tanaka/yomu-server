#!/bin/bash
# deploy/install.sh で入れた LaunchAgent を停止して取り除く。
# ~/Library/Application Support/paper-reader（データベースを含む）とログは残す。
set -euo pipefail
domain="gui/$(id -u)"
for name in local.paper-reader.server local.paper-reader.ollama local.paper-reader.caffeinate; do
  plist="$HOME/Library/LaunchAgents/$name.plist"
  launchctl bootout "$domain/$name" 2>/dev/null || true
  if [ -f "$plist" ]; then
    rm "$plist"
    echo "削除しました: ${name}"
  fi
done
