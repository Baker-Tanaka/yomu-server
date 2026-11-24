//! 環境変数（.env 可）から読む設定。

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

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
        })
    }
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
    fn bools() {
        assert!(super::parse_bool("true"));
        assert!(super::parse_bool("1"));
        assert!(!super::parse_bool(" False "));
        assert!(!super::parse_bool("off"));
    }
}
