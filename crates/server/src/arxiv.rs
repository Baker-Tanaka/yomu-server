//! 論文 URL の検証（SSRF 対策）と arXiv ID の抽出。

use thiserror::Error;
use url::Url;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UrlError {
    #[error("URL を解釈できません")]
    Invalid,
    #[error("https の URL のみ利用できます")]
    NotHttps,
    #[error("このホストの URL は利用できません（許可: {0}）")]
    HostNotAllowed(String),
    #[error("ポート番号付きの URL は利用できません")]
    PortNotAllowed,
    #[error("ユーザー情報付きの URL は利用できません")]
    UserInfoNotAllowed,
    #[error("arXiv の論文 URL（/abs/, /pdf/, /html/）から論文 ID を読み取れません")]
    NoPaperId,
}

/// URL が https かつ許可ホスト宛てであることを検証する。
/// リダイレクト先の検証にも同じ関数を使う。
pub fn validate_url(url: &Url, allowed_hosts: &[String]) -> Result<(), UrlError> {
    if url.scheme() != "https" {
        return Err(UrlError::NotHttps);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlError::UserInfoNotAllowed);
    }
    // `port()` は https の既定ポート（443）の場合 None になる
    if url.port().is_some() {
        return Err(UrlError::PortNotAllowed);
    }
    let host = match url.host() {
        Some(url::Host::Domain(d)) => d.to_ascii_lowercase(),
        // IP アドレス直指定は許可しない
        _ => return Err(UrlError::HostNotAllowed(allowed_hosts.join(", "))),
    };
    let host = host.trim_end_matches('.');
    if allowed_hosts.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        Ok(())
    } else {
        Err(UrlError::HostNotAllowed(allowed_hosts.join(", ")))
    }
}

/// 入力文字列を解析・検証し、arXiv の論文 ID を返す。
pub fn parse_paper_url(input: &str, allowed_hosts: &[String]) -> Result<String, UrlError> {
    let url = Url::parse(input.trim()).map_err(|_| UrlError::Invalid)?;
    validate_url(&url, allowed_hosts)?;
    extract_arxiv_id(&url).ok_or(UrlError::NoPaperId)
}

/// `/abs/<id>`, `/pdf/<id>[.pdf]`, `/html/<id>[/...]` から論文 ID を取り出す。
/// 新形式（`2301.00001`, `2301.00001v2`）と旧形式（`hep-th/9901001v1`）に対応。
pub fn extract_arxiv_id(url: &Url) -> Option<String> {
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let (kind, rest) = segments.split_first()?;
    if !matches!(*kind, "abs" | "pdf" | "html") {
        return None;
    }
    let strip_pdf = |s: &str| s.strip_suffix(".pdf").unwrap_or(s).to_string();
    match rest {
        [first, ..] if is_new_style_id(&strip_pdf(first)) => Some(strip_pdf(first)),
        [archive, number, ..] if is_old_style_archive(archive) => {
            let number = strip_pdf(number);
            is_old_style_number(&number).then(|| format!("{archive}/{number}"))
        }
        _ => None,
    }
}

/// `YYMM.NNNN(N)` + 任意の `vN`
fn is_new_style_id(s: &str) -> bool {
    let (body, version) = split_version(s);
    if !version_ok(version) {
        return false;
    }
    let Some((yymm, num)) = body.split_once('.') else {
        return false;
    };
    yymm.len() == 4
        && yymm.bytes().all(|b| b.is_ascii_digit())
        && (4..=5).contains(&num.len())
        && num.bytes().all(|b| b.is_ascii_digit())
}

fn is_old_style_archive(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'-' || b == b'.')
}

/// `YYMMNNN` + 任意の `vN`
fn is_old_style_number(s: &str) -> bool {
    let (body, version) = split_version(s);
    version_ok(version) && body.len() == 7 && body.bytes().all(|b| b.is_ascii_digit())
}

fn split_version(s: &str) -> (&str, Option<&str>) {
    match s.rfind('v') {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    }
}

fn version_ok(v: Option<&str>) -> bool {
    match v {
        None => true,
        Some(v) => !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// 論文 ID から HTML 版の URL を作る。
pub fn html_url(id: &str) -> String {
    format!("https://arxiv.org/html/{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts() -> Vec<String> {
        vec!["arxiv.org".into(), "www.arxiv.org".into()]
    }

    #[test]
    fn accepts_abs_pdf_html() {
        let h = hosts();
        assert_eq!(
            parse_paper_url("https://arxiv.org/abs/2301.00001", &h),
            Ok("2301.00001".into())
        );
        assert_eq!(
            parse_paper_url("https://arxiv.org/abs/2301.00001v3?context=cs", &h),
            Ok("2301.00001v3".into())
        );
        assert_eq!(
            parse_paper_url("https://arxiv.org/pdf/2301.12345v2.pdf", &h),
            Ok("2301.12345v2".into())
        );
        assert_eq!(
            parse_paper_url("https://www.arxiv.org/pdf/2301.12345", &h),
            Ok("2301.12345".into())
        );
        assert_eq!(
            parse_paper_url("https://arxiv.org/html/2310.06825v1/#S2", &h),
            Ok("2310.06825v1".into())
        );
        assert_eq!(
            parse_paper_url("  https://ARXIV.org/abs/1706.03762  ", &h),
            Ok("1706.03762".into())
        );
    }

    #[test]
    fn accepts_old_style_id() {
        assert_eq!(
            parse_paper_url("https://arxiv.org/abs/hep-th/9901001v1", &hosts()),
            Ok("hep-th/9901001v1".into())
        );
        assert_eq!(
            parse_paper_url("https://arxiv.org/pdf/math.GT/0309136", &hosts()),
            Ok("math.GT/0309136".into())
        );
    }

    #[test]
    fn rejects_http() {
        assert_eq!(
            parse_paper_url("http://arxiv.org/abs/2301.00001", &hosts()),
            Err(UrlError::NotHttps)
        );
        assert_eq!(
            parse_paper_url("file:///etc/passwd", &hosts()),
            Err(UrlError::NotHttps)
        );
    }

    #[test]
    fn rejects_other_hosts() {
        let h = hosts();
        for u in [
            "https://example.com/abs/2301.00001",
            "https://arxiv.org.evil.com/abs/2301.00001",
            "https://evilarxiv.org/abs/2301.00001",
            "https://export.arxiv.org/abs/2301.00001",
            "https://127.0.0.1/abs/2301.00001",
            "https://[::1]/abs/2301.00001",
            "https://localhost/abs/2301.00001",
        ] {
            assert!(
                matches!(parse_paper_url(u, &h), Err(UrlError::HostNotAllowed(_))),
                "{u}"
            );
        }
    }

    #[test]
    fn rejects_userinfo_and_port() {
        let h = hosts();
        assert_eq!(
            parse_paper_url("https://arxiv.org@evil.com/abs/2301.00001", &h),
            Err(UrlError::UserInfoNotAllowed)
        );
        assert_eq!(
            parse_paper_url("https://arxiv.org:8443/abs/2301.00001", &h),
            Err(UrlError::PortNotAllowed)
        );
        // 既定ポートの明示は url クレートが正規化するので通る
        assert!(parse_paper_url("https://arxiv.org:443/abs/2301.00001", &h).is_ok());
    }

    #[test]
    fn rejects_non_paper_paths() {
        let h = hosts();
        for u in [
            "https://arxiv.org/",
            "https://arxiv.org/list/cs.CL/recent",
            "https://arxiv.org/abs/",
            "https://arxiv.org/abs/../../etc",
            "https://arxiv.org/abs/2301.0001x",
            "https://arxiv.org/abs/2301.00001v",
            "not a url",
        ] {
            assert!(parse_paper_url(u, &h).is_err(), "{u}");
        }
    }

    #[test]
    fn redirect_target_validation() {
        let h = hosts();
        let ok = Url::parse("https://arxiv.org/html/2301.00001v1").unwrap();
        assert!(validate_url(&ok, &h).is_ok());
        let bad = Url::parse("https://evil.example/").unwrap();
        assert!(validate_url(&bad, &h).is_err());
        let downgrade = Url::parse("http://arxiv.org/html/2301.00001v1").unwrap();
        assert_eq!(validate_url(&downgrade, &h), Err(UrlError::NotHttps));
    }
}
