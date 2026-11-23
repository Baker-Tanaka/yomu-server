//! EJDict-hand（パブリックドメインの英和辞書）を SQLite の dict テーブルに投入する。
//!
//! 使い方:
//!   cargo run -p import-dict --release                  # data/EJDict/src または data/ejdict-hand-utf8.txt
//!   cargo run -p import-dict --release -- <ファイル|ディレクトリ>...
//!
//! 入力は「見出し語\t訳」のタブ区切り。見出し語はカンマ区切りで複数並ぶことがある（"A,a,an"）。
//! 見出し語は小文字化し、大文字小文字違いで重複したものは訳を " / " でつなげて 1 行にまとめる。
//! その際、元から小文字だった見出し語（act）の訳を、略語など大文字の見出し語（ACT）の訳より前に置く。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{ConnectOptions, Connection};

const DEFAULT_INPUTS: &[&str] = &["data/EJDict/src", "data/ejdict-hand-utf8.txt"];

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/app.db".into());

    let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let inputs = if args.is_empty() {
        let found = DEFAULT_INPUTS
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .with_context(|| {
                format!(
                    "辞書データが見つかりません。{} のどちらかに置くか、パスを引数で指定してください（README 参照）",
                    DEFAULT_INPUTS.join(" / ")
                )
            })?;
        vec![found]
    } else {
        args
    };

    let files = collect_files(&inputs)?;
    let mut dict = Dict::default();
    let mut skipped = 0usize;
    for file in &files {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("{} を読み込めません", file.display()))?;
        for line in text.lines() {
            match parse_line(line) {
                Some((words, meaning)) => dict.add(&words, meaning),
                None if line.trim().is_empty() => {}
                None => skipped += 1,
            }
        }
    }
    if dict.entries.is_empty() {
        bail!("辞書データが空です: {:?}", files);
    }

    let options = SqliteConnectOptions::from_str(&database_url)
        .with_context(|| format!("DATABASE_URL を解釈できません: {database_url}"))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    if let Some(parent) = options.get_filename().parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut conn = options.connect().await.context("SQLite に接続できません")?;
    // スキーマはサーバーのマイグレーションと共通
    sqlx::migrate!("../../crates/server/migrations")
        .run(&mut conn)
        .await
        .context("マイグレーションに失敗しました")?;

    let mut tx = conn.begin().await?;
    sqlx::query("DELETE FROM dict").execute(&mut *tx).await?;
    for (word, meaning) in dict.rows() {
        sqlx::query("INSERT INTO dict (word, meaning) VALUES (?, ?)")
            .bind(word)
            .bind(meaning)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    println!(
        "{} ファイルから {} 語を投入しました（読み飛ばした行: {}）→ {}",
        files.len(),
        dict.entries.len(),
        skipped,
        database_url
    );
    Ok(())
}

/// ディレクトリなら中の *.txt を名前順に、ファイルならそのまま。
fn collect_files(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for input in inputs {
        if input.is_dir() {
            let mut txts: Vec<PathBuf> = std::fs::read_dir(input)
                .with_context(|| format!("{} を読めません", input.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| is_txt(p))
                .collect();
            txts.sort();
            files.extend(txts);
        } else if input.is_file() {
            files.push(input.clone());
        } else {
            bail!("{} が見つかりません", input.display());
        }
    }
    Ok(files)
}

fn is_txt(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "txt")
}

/// 見出し語（小文字化済み）と、元の表記がすでに小文字だったか。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Headword {
    word: String,
    was_lowercase: bool,
}

/// 1 行を（見出し語のリスト, 訳）に分解する。形式が不正なら `None`。
fn parse_line(line: &str) -> Option<(Vec<Headword>, String)> {
    let line = line.trim_start_matches('\u{feff}').trim_end_matches('\r');
    let (head, meaning) = line.split_once('\t')?;
    let meaning = meaning.trim();
    if meaning.is_empty() || meaning.contains('\t') {
        return None;
    }
    let mut words: Vec<Headword> = Vec::new();
    for w in head.split(',') {
        let original = w.split_whitespace().collect::<Vec<_>>().join(" ");
        let word = original.to_lowercase();
        let was_lowercase = word == original;
        if word.is_empty() {
            continue;
        }
        match words.iter_mut().find(|h| h.word == word) {
            // "A,a" のように両方あれば小文字扱い
            Some(h) => h.was_lowercase |= was_lowercase,
            None => words.push(Headword {
                word,
                was_lowercase,
            }),
        }
    }
    (!words.is_empty()).then(|| (words, meaning.to_string()))
}

#[derive(Default)]
struct Dict {
    /// 小文字化した見出し語 → (元が小文字か, 訳) のリスト（出現順）
    entries: BTreeMap<String, Vec<(bool, String)>>,
}

impl Dict {
    fn add(&mut self, words: &[Headword], meaning: String) {
        for h in words {
            let list = self.entries.entry(h.word.clone()).or_default();
            match list.iter_mut().find(|(_, m)| *m == meaning) {
                Some(existing) => existing.0 |= h.was_lowercase,
                None => list.push((h.was_lowercase, meaning.clone())),
            }
        }
    }

    /// (見出し語, 訳) の行。小文字の見出し語の訳を先に並べる（同順位は出現順）。
    fn rows(&self) -> impl Iterator<Item = (&str, String)> {
        self.entries.iter().map(|(word, list)| {
            let mut list: Vec<&(bool, String)> = list.iter().collect();
            list.sort_by_key(|(lower, _)| !*lower);
            let meaning = list
                .iter()
                .map(|(_, m)| m.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            (word.as_str(), meaning)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(word: &str, was_lowercase: bool) -> Headword {
        Headword {
            word: word.into(),
            was_lowercase,
        }
    }

    #[test]
    fn parse_simple() {
        assert_eq!(
            parse_line("study\t『勉強』 / 研究"),
            Some((vec![hw("study", true)], "『勉強』 / 研究".into()))
        );
    }

    #[test]
    fn parse_aliases_and_case() {
        assert_eq!(
            parse_line("A,a,an\t一つの"),
            Some((vec![hw("a", true), hw("an", true)], "一つの".into()))
        );
        assert_eq!(
            parse_line("\u{feff}A  level\t上級\r"),
            Some((vec![hw("a level", false)], "上級".into()))
        );
    }

    #[test]
    fn parse_invalid() {
        assert_eq!(parse_line("no tab here"), None);
        assert_eq!(parse_line("word\t"), None);
        assert_eq!(parse_line("\tmeaning"), None);
        assert_eq!(parse_line("a\tb\tc"), None);
    }

    #[test]
    fn merge_case_duplicates_lowercase_first() {
        let mut d = Dict::default();
        d.add(&[hw("act", false)], "American College Testing".into());
        d.add(&[hw("act", true)], "『行い』".into());
        d.add(&[hw("act", true)], "『行い』".into());
        let rows: Vec<_> = d.rows().collect();
        assert_eq!(
            rows,
            vec![("act", "『行い』 / American College Testing".to_string())]
        );
    }
}
