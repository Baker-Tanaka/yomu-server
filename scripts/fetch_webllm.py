#!/usr/bin/env python3
"""実験モード（iPhone などの端末で LLM を動かす）用に、WebLLM とモデルを Mac に取ってくる。

iPhone は外部（Hugging Face や CDN）に直接アクセスせず、すべてこの Mac のサーバーの /webllm/ から読む。
このスクリプトは次のものを <dest> に置き、manifest.json を書く（サーバーは <dest> をそのまま配信する）。

  lib/web-llm.js                      WebLLM の ES モジュール（npm の整合性ハッシュで検証）
  libs/<model_lib>.wasm               モデル用の WebGPU カーネル（mlc-ai/binary-mlc-llm-libs）
  models/<id>/resolve/main/<file>     モデルの重みと設定（Hugging Face の mlc-ai/<id>、LFS は sha256 で検証）
  manifest.json                       画面が読むモデルの一覧

使い方:
  python3 -I scripts/fetch_webllm.py                # 配置先（~/Library/Application Support/paper-reader/data/webllm）、無ければ data/webllm
  python3 -I scripts/fetch_webllm.py --dest data/webllm --models qwen2.5-0.5b
取得済みで大きさ（とハッシュ）が合うファイルは飛ばすので、途中で止まっても続きから取れる。
"""

import argparse
import base64
import hashlib
import io
import json
import os
import sys
import tarfile
import urllib.request
from pathlib import Path

WEBLLM_VERSION = "0.2.89"
# web-llm 0.2.89 の prebuiltAppConfig が使うモデル用 wasm の版
MODEL_LIB_VERSION = "v0_2_87/base"
MODEL_LIB_BASE = "https://raw.githubusercontent.com/mlc-ai/binary-mlc-llm-libs/main/web-llm-models/"
HF_BASE = "https://huggingface.co/"

# 画面に出す順。vram_mb は web-llm の prebuiltAppConfig の値
MODELS = [
    {
        "key": "qwen2.5-0.5b",
        "id": "Qwen2.5-0.5B-Instruct-q4f16_1-MLC",
        "label": "Qwen2.5 0.5B（最軽量）",
        "note": "いちばん軽く速いが、訳の質は低め",
        "model_lib": "Qwen2-0.5B-Instruct-q4f16_1_cs1k-webgpu.wasm",
        "vram_mb": 945,
    },
    {
        "key": "qwen2.5-1.5b",
        "id": "Qwen2.5-1.5B-Instruct-q4f16_1-MLC",
        "label": "Qwen2.5 1.5B（標準）",
        "note": "速さと訳の質のバランス",
        "model_lib": "Qwen2-1.5B-Instruct-q4f16_1_cs1k-webgpu.wasm",
        "vram_mb": 1630,
    },
    {
        "key": "gemma-2-2b-jpn",
        "id": "gemma-2-2b-jpn-it-q4f16_1-MLC",
        "label": "Gemma 2 2B 日本語版（重い）",
        "note": "日本語向けに調整されたモデル。メモリを最も使う",
        "model_lib": "gemma-2-2b-jpn-it-q4f16_1_cs1k-webgpu.wasm",
        "vram_mb": 1895,
    },
]

UA = {"User-Agent": "paper-reader-fetch-webllm"}


def default_dest() -> Path:
    app = Path.home() / "Library/Application Support/paper-reader"
    if app.is_dir():
        return app / "data/webllm"
    return Path("data/webllm")


def get(url: str) -> bytes:
    with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=120) as r:
        return r.read()


def download(url: str, path: Path, size: int | None = None, sha256: str | None = None) -> int:
    """url を path に保存する（大きさとハッシュが合えば飛ばす）。保存したファイルの大きさを返す。"""
    if path.exists() and (size is None or path.stat().st_size == size):
        if sha256 is None or file_sha256(path) == sha256:
            return path.stat().st_size
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".part")
    h = hashlib.sha256()
    done = 0
    with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=120) as r, open(tmp, "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)
            h.update(chunk)
            done += len(chunk)
            if size:
                print(f"\r  {path.name}: {done * 100 // size}%", end="", flush=True)
    print(f"\r  {path.name}: {done:,} bytes")
    if size is not None and done != size:
        tmp.unlink()
        sys.exit(f"大きさが合いません: {url}（{done} != {size}）")
    if sha256 is not None and h.hexdigest() != sha256:
        tmp.unlink()
        sys.exit(f"sha256 が合いません: {url}")
    os.replace(tmp, path)
    return done


def file_sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def fetch_library(dest: Path) -> None:
    meta = json.loads(get(f"https://registry.npmjs.org/@mlc-ai/web-llm/{WEBLLM_VERSION}"))
    tarball = get(meta["dist"]["tarball"])
    algo, b64 = meta["dist"]["integrity"].split("-", 1)
    if algo != "sha512" or hashlib.sha512(tarball).digest() != base64.b64decode(b64):
        sys.exit("web-llm の整合性ハッシュが合いません")
    with tarfile.open(fileobj=io.BytesIO(tarball)) as tar:
        js = tar.extractfile("package/lib/index.js")
        lic = tar.extractfile("package/LICENSE")
        if js is None or lic is None:
            sys.exit("web-llm のパッケージに lib/index.js がありません")
        (dest / "lib").mkdir(parents=True, exist_ok=True)
        (dest / "lib/web-llm.js").write_bytes(js.read())
        (dest / "lib/LICENSE").write_bytes(lic.read())
    print(f"web-llm {WEBLLM_VERSION}: lib/web-llm.js")


def fetch_model(dest: Path, m: dict) -> int:
    print(f"{m['id']}:")
    tree = json.loads(get(f"{HF_BASE}api/models/mlc-ai/{m['id']}/tree/main"))
    total = 0
    for f in tree:
        if f.get("type") != "file":
            continue
        name = f["path"]
        # 自分で確かめられる名前だけ（サブディレクトリや変な名前は取らない）
        if "/" in name or name.startswith(".") or name in ("README.md",):
            continue
        lfs = f.get("lfs") or {}
        total += download(
            f"{HF_BASE}mlc-ai/{m['id']}/resolve/main/{name}",
            dest / "models" / m["id"] / "resolve/main" / name,
            size=f.get("size"),
            sha256=lfs.get("oid"),
        )
    total += download(
        f"{MODEL_LIB_BASE}{MODEL_LIB_VERSION}/{m['model_lib']}",
        dest / "libs" / m["model_lib"],
    )
    return total


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dest", type=Path, default=None)
    ap.add_argument("--models", nargs="*", default=[m["key"] for m in MODELS],
                    help="取るモデル（" + ", ".join(m["key"] for m in MODELS) + "）")
    args = ap.parse_args()
    dest = (args.dest or default_dest()).resolve()
    unknown = set(args.models) - {m["key"] for m in MODELS}
    if unknown:
        sys.exit(f"不明なモデル: {', '.join(sorted(unknown))}")
    print(f"配置先: {dest}")
    fetch_library(dest)
    listed = []
    for m in MODELS:
        if m["key"] not in args.models:
            continue
        size = fetch_model(dest, m)
        listed.append({
            "id": m["id"],
            "label": m["label"],
            "note": m["note"],
            "vram_mb": m["vram_mb"],
            "bytes": size,
            "model_url": f"/webllm/models/{m['id']}/",
            "model_lib": f"/webllm/libs/{m['model_lib']}",
        })
    manifest = {"webllm_version": WEBLLM_VERSION, "library": "/webllm/lib/web-llm.js", "models": listed}
    (dest / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"manifest.json: {len(listed)} モデル")


if __name__ == "__main__":
    main()
