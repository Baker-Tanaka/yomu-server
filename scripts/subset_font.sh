#!/usr/bin/env bash
# 日本語フォントを使う文字だけに絞る（wasm に埋め込むフォントを小さくする）。
#
# 使い方: scripts/subset_font.sh [元のフォント] [出力先]
#   元のフォント 既定: data/fonts/NotoSansJP-Regular.otf
#   出力先       既定: crates/web/assets/fonts/NotoSansJP-Regular.otf
# 必要なもの: fonttools（pip install fonttools または brew install fonttools）
set -euo pipefail
cd "$(dirname "$0")/.."

src="${1:-data/fonts/NotoSansJP-Regular.otf}"
out="${2:-crates/web/assets/fonts/NotoSansJP-Regular.otf}"

if python3 -c "import fontTools" >/dev/null 2>&1; then
  subset=(python3 -m fontTools.subset)
elif pyftsubset --help >/dev/null 2>&1; then
  subset=(pyftsubset)
else
  echo "fonttools が見つかりません。pip install fonttools でインストールしてください。" >&2
  exit 1
fi
if [ ! -f "$src" ]; then
  echo "元のフォント ${src} がありません（README の「フォント」を参照）。" >&2
  exit 1
fi

chars="$(mktemp)"
trap 'rm -f "$chars"' EXIT
python3 scripts/font_chars.py > "$chars"

"${subset[@]}" "$src" \
  --text-file="$chars" \
  --output-file="$out" \
  --layout-features='*' \
  --name-IDs='*' \
  --name-languages='*' \
  --no-hinting \
  --notdef-outline

echo "${out}: $(wc -c < "$src" | tr -d ' ') → $(wc -c < "$out" | tr -d ' ') バイト（$(wc -m < "$chars" | tr -d ' ') 文字）"
