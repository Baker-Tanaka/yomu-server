#!/usr/bin/env python3
"""日本語フォントのサブセットに含める文字を標準出力に書き出す。

JIS X 0208（第1・第2水準漢字、かな、記号）と CP932 の拡張（①、㈱、NEC/IBM 拡張漢字など）、
ASCII・Latin-1・一般句読点・矢印・数学記号・全角形を含める。
LLM の解説や辞書の訳に出てくる文字はほぼこの範囲に収まる。
"""

import sys


def jis_x0208_and_cp932() -> set[str]:
    chars: set[str] = set()
    for lead in range(0x81, 0xFD):
        for trail in range(0x40, 0xFD):
            try:
                chars.update(bytes([lead, trail]).decode("cp932"))
            except UnicodeDecodeError:
                pass
    return chars


RANGES = [
    (0x0020, 0x007E),  # ASCII
    (0x00A0, 0x00FF),  # Latin-1
    (0x0370, 0x03FF),  # ギリシャ文字
    (0x2000, 0x206F),  # 一般句読点（— – … ‘ ’ “ ” など）
    (0x2070, 0x209F),  # 上付き・下付き
    (0x2100, 0x214F),  # 文字様記号（℃ № ™）
    (0x2150, 0x218F),  # 数字の形（Ⅰ Ⅱ ½）
    (0x2190, 0x21FF),  # 矢印
    (0x2200, 0x22FF),  # 数学記号
    (0x2460, 0x24FF),  # 囲み英数字
    (0x2500, 0x257F),  # 罫線
    (0x25A0, 0x25FF),  # 幾何学模様（■ □ ▲ ●）
    (0x2600, 0x26FF),  # その他の記号（★ ☆）
    (0x3000, 0x303F),  # CJK の記号と句読点
    (0x3040, 0x309F),  # ひらがな
    (0x30A0, 0x30FF),  # カタカナ
    (0x31F0, 0x31FF),  # カタカナ拡張
    (0x3200, 0x33FF),  # 囲み CJK 文字、CJK 互換用文字
    (0xFF00, 0xFFEF),  # 全角形・半角形
]


def main() -> None:
    chars = jis_x0208_and_cp932()
    for start, end in RANGES:
        chars.update(chr(c) for c in range(start, end + 1))
    sys.stdout.write("".join(sorted(chars)))


if __name__ == "__main__":
    main()
