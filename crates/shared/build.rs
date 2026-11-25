//! ビルドしたコミットの Git ハッシュと日付を埋め込む（`PAPER_READER_GIT_HASH` / `PAPER_READER_GIT_DATE`）。
//!
//! server と web（wasm）の両方がこのクレートを使うので、画面とサーバーで同じ方法でバージョンが分かる。
//! git が無い・リポジトリの外でビルドした場合は空文字（表示側で「不明」にする）。
//! 環境変数 `PAPER_READER_GIT_HASH` / `PAPER_READER_GIT_DATE` を与えればそれを使う。

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(env_dir())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn env_dir() -> String {
    std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into())
}

/// 埋め込む値に使ってよい文字だけにする（念のため）。
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(40)
        .collect()
}

fn main() {
    println!("cargo:rerun-if-env-changed=PAPER_READER_GIT_HASH");
    println!("cargo:rerun-if-env-changed=PAPER_READER_GIT_DATE");

    // コミット・ブランチの切り替えで作り直す
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        let dir = Path::new(&git_dir);
        let mut watch = vec![dir.join("HEAD"), dir.join("packed-refs")];
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            watch.push(dir.join(r));
        }
        // 無いファイルを指定すると毎回作り直しになるので、あるものだけ
        for p in watch.iter().filter(|p| p.exists()) {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }

    let hash = std::env::var("PAPER_READER_GIT_HASH")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| git(&["rev-parse", "--short=7", "HEAD"]))
        .map(|s| clean(&s))
        .unwrap_or_default();
    let date = std::env::var("PAPER_READER_GIT_DATE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| git(&["log", "-1", "--format=%cs", "HEAD"]))
        .map(|s| clean(&s))
        .unwrap_or_default();
    println!("cargo:rustc-env=PAPER_READER_GIT_HASH={hash}");
    println!("cargo:rustc-env=PAPER_READER_GIT_DATE={date}");
}
