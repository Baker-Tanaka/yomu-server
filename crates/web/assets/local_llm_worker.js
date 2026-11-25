// 実験モード: 端末の LLM の推論を行う Web Worker（local_llm.js が作る）。
// WebLLM はこの Mac のサーバーの /webllm/ から読む。
import { WebWorkerMLCEngineHandler } from "/webllm/lib/web-llm.js";

const handler = new WebWorkerMLCEngineHandler();
self.onmessage = (msg) => handler.onmessage(msg);
