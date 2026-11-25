# 実験モード: 端末で LLM を動かす（WebGPU）

iPhone などの端末の GPU で小さな LLM を動かし、Mac の LLM と速さ・訳の質を比べるための実験機能です。**既定はオフ**で、
オフのあいだは関係するファイルを一切読み込まないので、最初の表示は遅くなりません。

1. Mac で WebLLM とモデルを取ってくる（初回だけ。合計約 2.6 GB）

   ```bash
   python3 -I scripts/fetch_webllm.py                     # 配置先（~/Library/Application Support/paper-reader/data/webllm）へ
   python3 -I scripts/fetch_webllm.py --dest data/webllm  # 開発用（cargo run で起動したサーバー）
   python3 -I scripts/fetch_webllm.py --models qwen2.5-0.5b   # 一部だけ
   ```

   WebLLM（npm の `@mlc-ai/web-llm`、整合性ハッシュで検証）、モデルの重み（Hugging Face の `mlc-ai/*`、sha256 で検証）、
   モデル用の WebGPU カーネル（`mlc-ai/binary-mlc-llm-libs`）を取得し、`manifest.json` を書きます。
   端末はこれらを **Mac のサーバーの `/webllm/` から**読みます（端末から外部には直接アクセスしません）

2. 画面の ⚙ で「🧪 実験: 端末で LLM を動かす」をオンにすると、選んだモデルを Mac からダウンロードして読み込みます
   （進み具合は ⚙ と解説タブに出ます）。ダウンロードしたモデルはブラウザの Cache Storage に残り、次からはそこから読みます
3. 単語・英文を選ぶと、解説タブの下に「🧪 端末の LLM（実験）」が出ます。「端末で訳す」「端末で和訳」で、
   この端末の LLM だけで訳し、最初の文字までの時間・合計時間・トークン/秒を表示します

| モデル | ダウンロード | GPU メモリ | 特徴 |
| --- | --- | --- | --- |
| Qwen2.5 0.5B | 約 0.3 GB | 約 0.9 GB | 最軽量。訳の質は低め |
| Qwen2.5 1.5B（既定） | 約 0.9 GB | 約 1.6 GB | 速さと訳の質のバランス |
| Gemma 2 2B 日本語版 | 約 1.4 GB | 約 1.9 GB | 日本語向けに調整。いちばん重い |

- WebGPU が必要です（iPhone は Safari 26 以降）。推論は Web Worker で行い、画面の描画を止めません
- 実験モードをオフにするとモデルをメモリから外します（ダウンロードしたファイルは残る）。「削除」で端末から消します
- 端末の空きが少ないと、ブラウザが保存したモデルを消すことがあります（ホーム画面に追加して使うと消されにくい）
- Mac の LLM（解説）とは独立しています。端末の LLM の結果は保存しません
