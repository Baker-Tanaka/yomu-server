#!/bin/bash
# 本番用にビルドする: サーバー（release）と、フロントエンド（trunk build --release + wasm-opt）。
# brotli があれば dist/ のファイルを事前に最大圧縮しておく（サーバーはその場で圧縮するより軽く、小さく返せる）。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -p paper-reader-server
(cd crates/web && trunk build --release)

if command -v brotli >/dev/null 2>&1; then
  shopt -s nullglob
  for f in crates/web/dist/*.{wasm,js,css,html}; do
    brotli -q 11 -k -f "$f"
    gzip -9 -k -f "$f"
  done
else
  echo "brotli が無いので事前圧縮は省きます（サーバーがその場で圧縮します。brew install brotli で入れると転送量が約 2 割減ります）"
fi
ls -lh crates/web/dist/
