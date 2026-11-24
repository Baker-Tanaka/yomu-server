//! 環境変数（.env 可）から読む設定。

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::grammar::GrammarPaths;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub llm_base_url: String,
    pub llm_model: String,
    /// thinking を行うモデルで推論を無効にする（`reasoning_effort: "none"` を付ける）
    pub llm_disable_thinking: bool,
    pub database_url: String,
    pub static_dir: PathBuf,
    pub allowed_hosts: Vec<String>,
    /// 文法タグのマスター
    pub grammar_tags_path: PathBuf,
    /// 講座のレッスン対応（ユーザーが記入する。無ければテンプレートを読む）
    pub grammar_map_path: PathBuf,
    pub grammar_map_template_path: PathBuf,
    /// フィードバックを Issue にするリポジトリ（owner/name）。未設定なら送信せずに保存だけする
    pub github_repo: Option<String>,
    /// Issue を作る GitHub のトークン（Fine-grained token、そのリポジトリの Issues: Read and write だけ）
    pub github_token: Option<Secret>,
    /// 単語帳（検索回数・カタカナの読み・辞書に無い語の訳）に残す語数の上限
    pub word_store_max: i64,
}

/// ログなどに出してはいけない値。Debug では伏せ字になる。
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let get = |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.into());
        Ok(Self {
            bind_addr: get("BIND_ADDR", "127.0.0.1:8080")
                .parse()
                .context("BIND_ADDR を解釈できません")?,
            llm_base_url: get("LLM_BASE_URL", "http://127.0.0.1:11434/v1"),
            llm_model: get("LLM_MODEL", "gemma4:e4b-it-qat"),
            llm_disable_thinking: parse_bool(&get("LLM_DISABLE_THINKING", "true")),
            database_url: get("DATABASE_URL", "sqlite://data/app.db"),
            static_dir: get("STATIC_DIR", "crates/web/dist").into(),
            allowed_hosts: parse_hosts(&get("ALLOWED_HOSTS", "arxiv.org,www.arxiv.org")),
            grammar_tags_path: get("GRAMMAR_TAGS_PATH", "data/grammar_tags.yaml").into(),
            grammar_map_path: get("GRAMMAR_MAP_PATH", "data/grammar_map.local.yaml").into(),
            grammar_map_template_path: get(
                "GRAMMAR_MAP_TEMPLATE_PATH",
                "data/grammar_map.template.yaml",
            )
            .into(),
            github_repo: parse_repo(&get("GITHUB_REPO", ""))?,
            github_token: Some(get("GITHUB_TOKEN", ""))
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .map(Secret),
            word_store_max: parse_word_store_max(&get("WORD_STORE_MAX", "20000"))?,
        })
    }
}

/// `owner/name` の形か確かめる（空なら未設定）。
fn parse_repo(s: &str) -> Result<Option<String>> {
    let s = s.trim().trim_end_matches(".git");
    if s.is_empty() {
        return Ok(None);
    }
    let ok_part = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    match s.split_once('/') {
        Some((owner, name)) if ok_part(owner) && ok_part(name) => Ok(Some(s.to_string())),
        _ => anyhow::bail!(
            "GITHUB_REPO は owner/name の形で指定してください（例: octocat/hello）: {s}"
        ),
    }
}

impl Config {
    pub fn grammar_paths(&self) -> GrammarPaths {
        GrammarPaths {
            tags: self.grammar_tags_path.clone(),
            map: self.grammar_map_path.clone(),
            template: self.grammar_map_template_path.clone(),
        }
    }
}

/// 単語帳の上限（100 〜 1,000,000 語）。
fn parse_word_store_max(s: &str) -> Result<i64> {
    let n: i64 = s
        .trim()
        .parse()
        .with_context(|| format!("WORD_STORE_MAX は語数（整数）で指定してください: {s}"))?;
    anyhow::ensure!(
        (100..=1_000_000).contains(&n),
        "WORD_STORE_MAX は 100 〜 1000000 の範囲で指定してください: {n}"
    );
    Ok(n)
}

fn parse_bool(s: &str) -> bool {
    !matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

fn parse_hosts(s: &str) -> Vec<String> {
    s.split(',')
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn hosts() {
        assert_eq!(
            super::parse_hosts(" arxiv.org, WWW.arxiv.org ,,"),
            vec!["arxiv.org", "www.arxiv.org"]
        );
    }

    #[test]
    fn repo() {
        assert_eq!(super::parse_repo("").unwrap(), None);
        assert_eq!(
            super::parse_repo(" Baker-Tanaka/yomu-server.git ")
                .unwrap()
                .as_deref(),
            Some("Baker-Tanaka/yomu-server")
        );
        for bad in [
            "noslash",
            "a/b/c",
            "a/..",
            "a /b",
            "https://github.com/a/b",
            "a/b?x",
        ] {
            assert!(super::parse_repo(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn secret_is_hidden() {
        let s = super::Secret("ghp_x".into());
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(s.expose(), "ghp_x");
    }

    #[test]
    fn word_store_max() {
        assert_eq!(super::parse_word_store_max(" 20000 ").unwrap(), 20_000);
        assert!(super::parse_word_store_max("10").is_err());
        assert!(super::parse_word_store_max("abc").is_err());
    }

    #[test]
    fn bools() {
        assert!(super::parse_bool("true"));
        assert!(super::parse_bool("1"));
        assert!(!super::parse_bool(" False "));
        assert!(!super::parse_bool("off"));
    }
}
