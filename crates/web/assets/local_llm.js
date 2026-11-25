// 実験モード: 端末（iPhone など）の GPU（WebGPU）で小さな LLM を動かす。
//
// - 実験モードがオフ（既定）のあいだは、このファイルも WebLLM も読み込まない（wasm から import() で読む）
// - WebLLM とモデルは、この Mac のサーバーの /webllm/ から読む（外部には直接アクセスしない）
// - 推論は Web Worker で行い、画面（egui）の描画を止めない
// - ダウンロードしたモデルはブラウザの Cache Storage に残り、次からはそこから読む

const MANIFEST_URL = "/webllm/manifest.json";

let manifestCache = null;
let worker = null;
let engine = null;
let loadedId = null;

/** WebGPU が使えるか。 */
export function supported() {
  return typeof navigator !== "undefined" && !!navigator.gpu;
}

/** サーバーにあるモデルの一覧（scripts/fetch_webllm.py が書く manifest.json）。 */
export async function manifest() {
  const r = await fetch(MANIFEST_URL, { cache: "no-cache" });
  if (r.status === 404) {
    throw new Error("サーバーに端末用のモデルがありません（Mac で scripts/fetch_webllm.py を実行してください）");
  }
  if (!r.ok) {
    throw new Error(`モデルの一覧を読めません（HTTP ${r.status}）`);
  }
  manifestCache = await r.json();
  return manifestCache;
}

function appConfig(m) {
  const origin = location.origin;
  return {
    model_list: [
      {
        model: origin + m.model_url,
        model_id: m.id,
        model_lib: origin + m.model_lib,
        vram_required_MB: m.vram_mb,
        low_resource_required: true,
        overrides: { context_window_size: 4096 },
      },
    ],
  };
}

function entry(id) {
  const m = manifestCache && manifestCache.models.find((x) => x.id === id);
  if (!m) throw new Error(`モデル ${id} はサーバーの一覧にありません`);
  return m;
}

/**
 * モデルを読み込む（初回はダウンロード）。onProgress(割合 0〜1, 説明) を呼ぶ。
 * 戻り値は読み込みにかかったミリ秒。
 */
export async function load(id, onProgress) {
  if (!supported()) throw new Error("このブラウザは WebGPU に対応していません");
  if (!manifestCache) await manifest();
  const m = entry(id);
  if (engine && loadedId === id) return 0;
  await unload();
  // 端末の空きが少ないときにブラウザがモデルを消さないよう、永続化を頼む（断られても続ける）
  try {
    if (navigator.storage && navigator.storage.persist) await navigator.storage.persist();
  } catch (_) {}
  const started = performance.now();
  const webllm = await import(manifestCache.library);
  worker = new Worker(new URL("./local_llm_worker.js", import.meta.url), { type: "module" });
  engine = await webllm.CreateWebWorkerMLCEngine(worker, m.id, {
    appConfig: appConfig(m),
    initProgressCallback: (report) => {
      try {
        onProgress(report.progress ?? 0, report.text ?? "");
      } catch (_) {}
    },
  });
  loadedId = id;
  return performance.now() - started;
}

/**
 * 生成する。onToken(断片) を呼び、終わったら計測値を返す。
 * { ttft_ms, total_ms, tokens, decode_tps, prefill_tps }
 */
export async function generate(system, user, maxTokens, onToken) {
  if (!engine) throw new Error("モデルが読み込まれていません");
  const started = performance.now();
  let ttft = null;
  let usage = null;
  const chunks = await engine.chat.completions.create({
    stream: true,
    stream_options: { include_usage: true },
    temperature: 0.3,
    max_tokens: maxTokens,
    messages: [
      { role: "system", content: system },
      { role: "user", content: user },
    ],
  });
  for await (const chunk of chunks) {
    const delta = chunk.choices && chunk.choices[0] && chunk.choices[0].delta && chunk.choices[0].delta.content;
    if (delta) {
      if (ttft === null) ttft = performance.now() - started;
      onToken(delta);
    }
    if (chunk.usage) usage = chunk.usage;
  }
  const extra = (usage && usage.extra) || {};
  return {
    ttft_ms: ttft ?? 0,
    total_ms: performance.now() - started,
    tokens: (usage && usage.completion_tokens) || 0,
    decode_tps: extra.decode_tokens_per_s || 0,
    prefill_tps: extra.prefill_tokens_per_s || 0,
  };
}

/** 生成を止める。 */
export function interrupt() {
  if (engine) engine.interruptGenerate();
}

/** モデルを外してメモリを空ける（ダウンロードしたファイルは残る）。 */
export async function unload() {
  const e = engine;
  engine = null;
  loadedId = null;
  if (e) {
    try {
      await e.unload();
    } catch (_) {}
  }
  if (worker) {
    worker.terminate();
    worker = null;
  }
}

/** ダウンロードしたモデルを端末から消す。 */
export async function deleteModel(id) {
  if (!manifestCache) await manifest();
  const m = entry(id);
  if (loadedId === id) await unload();
  const webllm = await import(manifestCache.library);
  await webllm.deleteModelAllInfoInCache(m.id, appConfig(m));
}
