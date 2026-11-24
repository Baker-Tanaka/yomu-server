//! `GET /api/search?q=&start=&sort=` — arXiv API（export.arxiv.org）で論文を検索する。
//!
//! - クエリはサーバーで組み立てる（ユーザーの入力から URL を作らない）。宛先は固定の HTTPS ホスト
//! - arXiv API の利用規約に従い、API への問い合わせは 3 秒に 1 回までにする
//! - 同じ検索は 10 分間メモリにキャッシュする
//! - 題名らしい入力（3 語以上で is / of などを含む）はまず語句として探し、0 件ならキーワードの AND で探し直す
//!   （arXiv API は is / all などのストップワードを AND に入れると 0 件になるため）

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use paper_reader_shared::{SEARCH_PAGE_SIZE, SearchItem, SearchQuery, SearchResponse, SearchSort};
use tokio::sync::Mutex;
use url::Url;

use crate::AppState;
use crate::error::{AppError, AppResult};

pub const API_HOST: &str = "export.arxiv.org";
const API_URL: &str = "https://export.arxiv.org/api/query";
/// API への問い合わせの最小間隔（arXiv API の利用規約）
const MIN_INTERVAL: Duration = Duration::from_secs(3);
const CACHE_TTL: Duration = Duration::from_secs(600);
/// arXiv にアクセスを制限されたとき（HTTP 429 / 503）に問い合わせを止める時間の最初の値と上限
const BACKOFF_INITIAL: Duration = Duration::from_secs(60);
const BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);
const CACHE_MAX: usize = 200;
const MAX_QUERY_CHARS: usize = 200;
const MAX_TERMS: usize = 12;
/// これより先のページは取らない（API の負荷と応答時間のため）
const MAX_START: u32 = 1_000;

/// キャッシュのキー（入力, start, 並び順）と値（保存時刻, 結果）
type CacheKey = (String, u32, SearchSort);
type CacheEntry = (Instant, SearchResponse);
/// 入力と並び順ごとに、1 ページ目で使った search_query（2 ページ目以降も同じものを使う）
type PlanMemo = HashMap<(String, SearchSort), (Instant, String)>;

/// 検索の状態（AppState で共有する）。
#[derive(Default)]
pub struct Searcher {
    /// 最後に API へ問い合わせた時刻。ロックを持ったまま待つことで間隔を守る
    last_call: Mutex<Option<Instant>>,
    cache: Mutex<HashMap<CacheKey, CacheEntry>>,
    chosen: Mutex<PlanMemo>,
    backoff: Mutex<Backoff>,
}

/// arXiv にアクセスを制限されている間は問い合わせない（制限中に送り続けると制限が延びるため）。
#[derive(Debug, Default)]
struct Backoff {
    /// この時刻までは問い合わせない
    until: Option<Instant>,
    /// 次に制限されたときに止める時間（成功したら初期値に戻す）
    next: Option<Duration>,
}

impl Backoff {
    /// 制限中なら残り時間。
    fn remaining(&self, now: Instant) -> Option<Duration> {
        self.until
            .filter(|until| *until > now)
            .map(|until| until - now)
    }

    /// 制限された。止める時間を返す（60 秒から倍々、最大 15 分）。
    fn limited(&mut self, now: Instant) -> Duration {
        let wait = self.next.unwrap_or(BACKOFF_INITIAL);
        self.until = Some(now + wait);
        self.next = Some((wait * 2).min(BACKOFF_MAX));
        wait
    }

    fn succeeded(&mut self) {
        *self = Self::default();
    }
}

/// 制限中に返すメッセージ。
fn rate_limited_message(wait: Duration) -> String {
    format!(
        "arXiv の検索 API にアクセスが一時的に制限されています（混雑や短時間の連続アクセスのため）。\
         約 {} 秒後にもう一度お試しください。論文の URL や arXiv ID（例: 1706.03762）が分かっていれば、\
         入力欄にそのまま入れて開けます。",
        wait.as_secs().max(1)
    )
}

impl Searcher {
    /// 間隔を守って API に問い合わせる。
    async fn call(&self, client: &reqwest::Client, url: Url) -> AppResult<String> {
        if let Some(wait) = self.backoff.lock().await.remaining(Instant::now()) {
            return Err(AppError::ServiceUnavailable(rate_limited_message(wait)));
        }
        let mut last = self.last_call.lock().await;
        if let Some(prev) = *last {
            let wait = MIN_INTERVAL.saturating_sub(prev.elapsed());
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }
        let result = fetch(client, url).await;
        *last = Some(Instant::now());
        match result {
            Ok(body) => {
                self.backoff.lock().await.succeeded();
                Ok(body)
            }
            Err(FetchError::RateLimited(status)) => {
                let wait = self.backoff.lock().await.limited(Instant::now());
                tracing::warn!(
                    status,
                    wait_secs = wait.as_secs(),
                    "arXiv API rate limited; backing off"
                );
                Err(AppError::ServiceUnavailable(rate_limited_message(wait)))
            }
            Err(FetchError::Other(e)) => Err(e),
        }
    }

    async fn search(
        &self,
        client: &reqwest::Client,
        query: &str,
        start: u32,
        sort: SearchSort,
    ) -> AppResult<SearchResponse> {
        let url = api_url(query, start, sort).map_err(|e| AppError::Internal(e.into()))?;
        let body = self.call(client, url).await?;
        let feed = tokio::task::spawn_blocking(move || parse_feed(&body))
            .await
            .map_err(|e| AppError::Internal(e.into()))?
            .map_err(|e| AppError::Upstream(format!("arXiv の検索結果を解釈できません: {e}")))?;
        match feed {
            Feed::Results(r) => Ok(r),
            Feed::ApiError(msg) => Err(AppError::BadRequest(format!(
                "arXiv が検索語を受け付けませんでした: {msg}"
            ))),
        }
    }
}

pub type SharedSearcher = Arc<Searcher>;

pub async fn get_search(
    State(state): State<AppState>,
    query: Result<Query<SearchQuery>, QueryRejection>,
) -> AppResult<Json<SearchResponse>> {
    let Query(q) = query.map_err(|_| AppError::BadRequest("検索語を指定してください".into()))?;
    let plan = plan_search(&q.q).map_err(AppError::BadRequest)?;
    let input = q.q.split_whitespace().collect::<Vec<_>>().join(" ");
    let start = q.start.min(MAX_START);
    let key = (input.clone(), start, q.sort);

    let searcher = &state.searcher;
    {
        let cache = searcher.cache.lock().await;
        if let Some((at, resp)) = cache.get(&key)
            && at.elapsed() < CACHE_TTL
        {
            return Ok(Json(resp.clone()));
        }
    }

    let client = &state.search_http;
    let resp = if start == 0 {
        let mut resp = searcher.search(client, &plan.primary, 0, q.sort).await?;
        let mut used = plan.primary.clone();
        if resp.total == 0
            && let Some(fallback) = &plan.fallback
        {
            resp = searcher.search(client, fallback, 0, q.sort).await?;
            used = fallback.clone();
        }
        promote_exact_title(&mut resp, &input);
        let mut chosen = searcher.chosen.lock().await;
        if chosen.len() >= CACHE_MAX {
            chosen.clear();
        }
        chosen.insert((input.clone(), q.sort), (Instant::now(), used));
        resp
    } else {
        let used = {
            let chosen = searcher.chosen.lock().await;
            chosen
                .get(&(input.clone(), q.sort))
                .filter(|(at, _)| at.elapsed() < CACHE_TTL)
                .map(|(_, used)| used.clone())
        };
        let query = used.unwrap_or(plan.primary);
        searcher.search(client, &query, start, q.sort).await?
    };

    let mut cache = searcher.cache.lock().await;
    if cache.len() >= CACHE_MAX {
        cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
    }
    cache.insert(key, (Instant::now(), resp.clone()));
    Ok(Json(resp))
}

enum FetchError {
    /// HTTP 429 / 503（アクセス制限・混雑）
    RateLimited(u16),
    Other(AppError),
}

async fn fetch(client: &reqwest::Client, url: Url) -> Result<String, FetchError> {
    let resp = client.get(url).send().await.map_err(|e| {
        FetchError::Other(if e.is_timeout() {
            AppError::UpstreamTimeout(
                "arXiv の検索がタイムアウトしました。arXiv が混み合っている可能性があります。少し待ってからお試しください。"
                    .into(),
            )
        } else {
            AppError::Upstream(format!("arXiv に接続できません: {e}"))
        })
    })?;
    let status = resp.status().as_u16();
    if is_rate_limited(status) {
        return Err(FetchError::RateLimited(status));
    }
    if !resp.status().is_success() {
        return Err(FetchError::Other(AppError::Upstream(format!(
            "arXiv の検索に失敗しました（HTTP {status}）"
        ))));
    }
    resp.text().await.map_err(|e| {
        FetchError::Other(AppError::Upstream(format!(
            "arXiv の応答を受信できません: {e}"
        )))
    })
}

fn is_rate_limited(status: u16) -> bool {
    matches!(status, 429 | 503)
}

fn api_url(search_query: &str, start: u32, sort: SearchSort) -> Result<Url, url::ParseError> {
    let mut url = Url::parse(API_URL)?;
    let sort_by = match sort {
        SearchSort::Relevance => "relevance",
        SearchSort::Newest => "submittedDate",
    };
    url.query_pairs_mut()
        .append_pair("search_query", search_query)
        .append_pair("start", &start.to_string())
        .append_pair("max_results", &SEARCH_PAGE_SIZE.to_string())
        .append_pair("sortBy", sort_by)
        .append_pair("sortOrder", "descending");
    Ok(url)
}

/// arXiv API に渡す search_query。`primary` が 0 件なら `fallback` で探し直す。
#[derive(Debug, PartialEq, Eq)]
pub struct SearchPlan {
    pub primary: String,
    pub fallback: Option<String>,
}

/// 題名が入力と（大文字小文字と記号を除いて）一致する論文を先頭に移す。
fn promote_exact_title(resp: &mut SearchResponse, input: &str) {
    let norm = |s: &str| words_of(s).join(" ").to_lowercase();
    let target = norm(input);
    if let Some(pos) = resp.items.iter().position(|it| norm(&it.title) == target)
        && pos > 0
    {
        let item = resp.items.remove(pos);
        resp.items.insert(0, item);
    }
}

/// AND に入れると 0 件になる語（arXiv の検索で索引されない英語のストップワード）
const STOPWORDS: &[&str] = &[
    "a", "about", "all", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for",
    "from", "how", "in", "into", "is", "it", "its", "not", "of", "on", "or", "our", "that", "the",
    "their", "this", "to", "via", "we", "what", "when", "which", "why", "with", "you", "your",
];

fn is_stopword(w: &str) -> bool {
    STOPWORDS.contains(&w.to_ascii_lowercase().as_str())
}

/// 入力から検索の方針を決める。
///
/// - 引用符も欄の指定も無く、3 語以上でストップワードを含む入力（題名らしいもの）は、まず全体を語句として探す
/// - それ以外は [`build_search_query`]
pub fn plan_search(input: &str) -> Result<SearchPlan, String> {
    let keywords = build_search_query(input)?;
    let plain = !input.contains(['"', '“', '”', ':']);
    let words = words_of(input);
    if plain && words.len() >= 3 && words.iter().any(|w| is_stopword(w)) {
        return Ok(SearchPlan {
            primary: format!("all:\"{}\"", words.join(" ")),
            fallback: Some(keywords),
        });
    }
    Ok(SearchPlan {
        primary: keywords,
        fallback: None,
    })
}

/// 英数字と一部の記号（- . ' +）以外を区切りとして、語に分ける。
fn words_of(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '.' | '\'' | '+')))
        .map(|w| w.trim_matches(|c: char| matches!(c, '-' | '.' | '\'' | '+')))
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 入力をキーワード検索の search_query にする。
///
/// - 語はすべて含む（AND）。ただしストップワードは除く（すべてストップワードなら残す）。`"..."` は語句として扱う
/// - `ti:` `au:` `abs:` `cat:` で始まる語はその欄だけを検索する（それ以外は `all:`）
/// - 英数字と一部の記号（- . ' +）以外は区切りとして扱う
pub fn build_search_query(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("検索語を入力してください".into());
    }
    if input.chars().count() > MAX_QUERY_CHARS {
        return Err(format!("検索語が長すぎます（上限 {MAX_QUERY_CHARS} 文字）"));
    }

    // (語句, ストップワードか)
    let mut terms: Vec<(String, bool)> = Vec::new();
    for (raw, quoted) in split_terms(input) {
        let (field, text) = match raw.split_once(':') {
            Some((f, t)) if !quoted && matches!(f, "ti" | "au" | "abs" | "cat") => (f, t),
            _ => ("all", raw.as_str()),
        };
        let words = words_of(text);
        match words.len() {
            0 => {}
            _ if quoted => terms.push((format!("{field}:\"{}\"", words.join(" ")), false)),
            _ => terms.extend(
                words
                    .iter()
                    .map(|w| (format!("{field}:{w}"), field == "all" && is_stopword(w))),
            ),
        }
    }
    if terms.is_empty() {
        return Err("検索できる語がありません（英数字で入力してください）".into());
    }
    if terms.iter().any(|(_, stop)| !stop) {
        terms.retain(|(_, stop)| !stop);
    }
    let mut terms: Vec<String> = terms.into_iter().map(|(t, _)| t).collect();
    terms.truncate(MAX_TERMS);
    Ok(terms.join(" AND "))
}

/// 空白で区切る。`"..."` の中は 1 つの語句にする。戻り値は (語, 引用符で囲まれていたか)。
fn split_terms(input: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for c in input.chars() {
        match c {
            '"' | '“' | '”' => {
                if !current.trim().is_empty() {
                    out.push((current.trim().to_string(), in_quote));
                }
                current.clear();
                in_quote = !in_quote;
            }
            c if c.is_whitespace() && !in_quote => {
                if !current.is_empty() {
                    out.push((std::mem::take(&mut current), false));
                }
            }
            c => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        out.push((current.trim().to_string(), in_quote));
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum Feed {
    Results(SearchResponse),
    /// API がエラーを 1 件のエントリとして返した
    ApiError(String),
}

const ATOM: &str = "http://www.w3.org/2005/Atom";
const OPENSEARCH: &str = "http://a9.com/-/spec/opensearch/1.1/";
const ARXIV: &str = "http://arxiv.org/schemas/atom";

/// Atom フィードを検索結果にする。
pub fn parse_feed(xml: &str) -> Result<Feed, roxmltree::Error> {
    let doc = roxmltree::Document::parse(xml)?;
    let feed = doc.root_element();
    let child_text = |node: roxmltree::Node, ns: &str, name: &str| -> String {
        node.children()
            .find(|n| n.has_tag_name((ns, name)))
            .and_then(|n| n.text())
            .map(collapse)
            .unwrap_or_default()
    };

    let total = child_text(feed, OPENSEARCH, "totalResults")
        .parse()
        .unwrap_or(0);
    let start = child_text(feed, OPENSEARCH, "startIndex")
        .parse()
        .unwrap_or(0);

    let mut items = Vec::new();
    for entry in feed.children().filter(|n| n.has_tag_name((ATOM, "entry"))) {
        let id_url = child_text(entry, ATOM, "id");
        if id_url.contains("/api/errors") {
            return Ok(Feed::ApiError(child_text(entry, ATOM, "summary")));
        }
        let Some(id) = id_from_abs_url(&id_url) else {
            continue;
        };
        let authors = entry
            .children()
            .filter(|n| n.has_tag_name((ATOM, "author")))
            .map(|a| child_text(a, ATOM, "name"))
            .filter(|n| !n.is_empty())
            .collect();
        let primary_category = entry
            .children()
            .find(|n| n.has_tag_name((ARXIV, "primary_category")))
            .and_then(|n| n.attribute("term"))
            .unwrap_or_default()
            .to_string();
        let published: String = child_text(entry, ATOM, "published")
            .chars()
            .take(10)
            .collect();
        items.push(SearchItem {
            id,
            title: child_text(entry, ATOM, "title"),
            authors,
            summary: child_text(entry, ATOM, "summary"),
            published,
            primary_category,
        });
    }
    Ok(Feed::Results(SearchResponse {
        total,
        start,
        items,
    }))
}

/// `http://arxiv.org/abs/2209.15001v3` → `2209.15001`（バージョンを除く）
fn id_from_abs_url(url: &str) -> Option<String> {
    let id = url.split("/abs/").nth(1)?.trim_end_matches('/');
    let base = match id.rsplit_once('v') {
        Some((b, v)) if !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) => b,
        _ => id,
    };
    (!base.is_empty()).then(|| base.to_string())
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_building() {
        // ストップワードは除く
        assert_eq!(
            build_search_query("attention is all you need").unwrap(),
            "all:attention AND all:need"
        );
        assert_eq!(build_search_query("the").unwrap(), "all:the");
        assert_eq!(
            build_search_query("\"large language model\" au:Hinton").unwrap(),
            "all:\"large language model\" AND au:Hinton"
        );
        assert_eq!(
            build_search_query("ti:transformer cat:cs.CL").unwrap(),
            "ti:transformer AND cat:cs.CL"
        );
        // 記号は区切りとして扱い、クエリ構文を注入させない
        assert_eq!(
            build_search_query("bert) OR (all:x&start=0").unwrap(),
            "all:bert AND all:x AND all:start AND all:0"
        );
        assert_eq!(
            build_search_query("self-attention").unwrap(),
            "all:self-attention"
        );
        assert!(build_search_query("   ").is_err());
        assert!(build_search_query("！？").is_err());
        assert!(build_search_query(&"a".repeat(201)).is_err());
        let many = (0..20)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            build_search_query(&many).unwrap().matches(" AND ").count(),
            MAX_TERMS - 1
        );
    }

    #[test]
    fn plans() {
        // 題名らしい入力は語句で探し、0 件ならキーワードで
        assert_eq!(
            plan_search("Attention Is All You Need").unwrap(),
            SearchPlan {
                primary: "all:\"Attention Is All You Need\"".into(),
                fallback: Some("all:Attention AND all:Need".into()),
            }
        );
        // キーワードの並びはそのまま AND
        assert_eq!(
            plan_search("diffusion model image").unwrap(),
            SearchPlan {
                primary: "all:diffusion AND all:model AND all:image".into(),
                fallback: None,
            }
        );
        // 引用符や欄の指定があれば利用者の指定どおり
        assert_eq!(
            plan_search("\"in context learning\" of llm")
                .unwrap()
                .fallback,
            None
        );
        assert_eq!(
            plan_search("au:Vaswani attention is all").unwrap().fallback,
            None
        );
    }

    #[test]
    fn exact_title_first() {
        let item = |id: &str, title: &str| SearchItem {
            id: id.into(),
            title: title.into(),
            authors: vec![],
            summary: String::new(),
            published: String::new(),
            primary_category: String::new(),
        };
        let mut r = SearchResponse {
            total: 3,
            start: 0,
            items: vec![
                item("a", "Tool Attention Is All You Need"),
                item("b", "Attention Is All You Need"),
                item("c", "Other"),
            ],
        };
        promote_exact_title(&mut r, "attention is all you need");
        let ids: Vec<_> = r.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["b", "a", "c"]);
    }

    #[test]
    fn backoff_doubles_and_resets() {
        let now = Instant::now();
        let mut b = Backoff::default();
        assert_eq!(b.remaining(now), None);
        assert_eq!(b.limited(now), Duration::from_secs(60));
        assert_eq!(
            b.remaining(now + Duration::from_secs(10)),
            Some(Duration::from_secs(50))
        );
        assert_eq!(b.remaining(now + Duration::from_secs(61)), None);
        assert_eq!(b.limited(now), Duration::from_secs(120));
        assert_eq!(b.limited(now), Duration::from_secs(240));
        for _ in 0..10 {
            b.limited(now);
        }
        assert_eq!(b.limited(now), BACKOFF_MAX);
        b.succeeded();
        assert_eq!(b.remaining(now), None);
        assert_eq!(b.limited(now), Duration::from_secs(60));
        assert!(is_rate_limited(429) && is_rate_limited(503) && !is_rate_limited(500));
        assert!(rate_limited_message(Duration::from_millis(10)).contains("約 1 秒後"));
    }

    #[test]
    fn url_is_fixed_host() {
        let u = api_url("all:x AND all:\"a b\"", 20, SearchSort::Newest).unwrap();
        assert_eq!(u.scheme(), "https");
        assert_eq!(u.host_str(), Some(API_HOST));
        let q: HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["search_query"], "all:x AND all:\"a b\"");
        assert_eq!(q["start"], "20");
        assert_eq!(q["sortBy"], "submittedDate");
        assert_eq!(q["max_results"], SEARCH_PAGE_SIZE.to_string());
    }

    const SAMPLE: &str = r#"<?xml version='1.0' encoding='UTF-8'?>
<feed xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/" xmlns:arxiv="http://arxiv.org/schemas/atom" xmlns="http://www.w3.org/2005/Atom">
  <opensearch:totalResults>19692</opensearch:totalResults>
  <opensearch:startIndex>0</opensearch:startIndex>
  <entry>
    <id>http://arxiv.org/abs/2209.15001v3</id>
    <title>Dilated Neighborhood
      Attention Transformer</title>
    <summary>Transformers are  quickly
    becoming &lt;popular&gt;.</summary>
    <published>2022-09-29T17:57:08Z</published>
    <arxiv:primary_category term="cs.CV"/>
    <author><name>Ali Hassani</name></author>
    <author><name>Humphrey Shi</name></author>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/hep-th/9901001v1</id>
    <title>Old</title>
    <summary>s</summary>
    <published>1999-01-01T00:00:00Z</published>
  </entry>
</feed>"#;

    #[test]
    fn feed_parsing() {
        let Feed::Results(r) = parse_feed(SAMPLE).unwrap() else {
            panic!("results expected");
        };
        assert_eq!((r.total, r.start), (19692, 0));
        assert_eq!(r.items.len(), 2);
        let a = &r.items[0];
        assert_eq!(a.id, "2209.15001");
        assert_eq!(a.title, "Dilated Neighborhood Attention Transformer");
        assert_eq!(a.summary, "Transformers are quickly becoming <popular>.");
        assert_eq!(a.published, "2022-09-29");
        assert_eq!(a.primary_category, "cs.CV");
        assert_eq!(a.authors, vec!["Ali Hassani", "Humphrey Shi"]);
        assert_eq!(r.items[1].id, "hep-th/9901001");
        assert!(r.items[1].authors.is_empty());
    }

    #[test]
    fn feed_error_entry() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry>
          <id>http://arxiv.org/api/errors#incorrect_id_format_for_x</id>
          <title>Error</title><summary>incorrect id format for x</summary></entry></feed>"#;
        assert_eq!(
            parse_feed(xml).unwrap(),
            Feed::ApiError("incorrect id format for x".into())
        );
        assert!(parse_feed("<broken").is_err());
    }
}
