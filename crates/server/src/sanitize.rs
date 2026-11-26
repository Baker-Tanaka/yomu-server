//! 論文 HTML の本文抽出と無害化。

use std::borrow::Cow;
use std::collections::HashSet;

use scraper::{Html, Selector};
use url::Url;

/// 抽出した論文本文（未無害化）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub title: String,
    pub body_html: String,
    /// `<base href>` があればそれ、無ければ取得元 URL
    pub base_url: Url,
}

/// アプリ側の DOM と衝突させないために論文側で使わせない id。
const RESERVED_IDS: &[&str] = &[
    "paper",
    "paper-column",
    "recent-words",
    "egui_canvas",
    "app",
    "layout",
    "splitter",
    "explain-pane",
    "search-input",
    "cite-peek",
    "selection-menu",
    "toc-button",
    "toc-panel",
    "reading-progress",
    "command-palette",
];

/// MathML の要素（ammonia は名前空間を検証したうえで許可する）。
const MATHML_TAGS: &[&str] = &[
    "math",
    "semantics",
    "annotation",
    "annotation-xml",
    "mrow",
    "mi",
    "mo",
    "mn",
    "ms",
    "mtext",
    "mspace",
    "msup",
    "msub",
    "msubsup",
    "mfrac",
    "msqrt",
    "mroot",
    "mover",
    "munder",
    "munderover",
    "mmultiscripts",
    "mprescripts",
    "none",
    "mtable",
    "mtr",
    "mtd",
    "mlabeledtr",
    "mstyle",
    "mpadded",
    "mphantom",
    "menclose",
    "merror",
    "mfenced",
];

const MATHML_ATTRS: &[&str] = &[
    "display",
    "displaystyle",
    "mathvariant",
    "mathsize",
    "stretchy",
    "fence",
    "separator",
    "separators",
    "lspace",
    "rspace",
    "largeop",
    "movablelimits",
    "accent",
    "accentunder",
    "symmetric",
    "minsize",
    "maxsize",
    "linethickness",
    "scriptlevel",
    "columnalign",
    "rowalign",
    "columnspacing",
    "rowspacing",
    "columnlines",
    "rowlines",
    "frame",
    "notation",
    "open",
    "close",
    "width",
    "height",
    "depth",
    "voffset",
    "encoding",
    "alttext",
    "columnspan",
    "rowspan",
];

/// 本文（`article` → `main` → `body` の順に探す）とタイトルを取り出す。
/// 本文らしき要素が見つからなければ `None`。
pub fn extract_article(html: &str, page_url: &Url) -> Option<Extracted> {
    let doc = Html::parse_document(html);

    let body_html = ["article", "main", "div.ltx_page_content"]
        .iter()
        .filter_map(|s| Selector::parse(s).ok())
        .find_map(|sel| doc.select(&sel).next())
        .map(|el| el.html())?;

    let title = select_text(&doc, "h1.ltx_title_document")
        .or_else(|| select_text(&doc, "article h1"))
        .or_else(|| select_text(&doc, "title"))
        .unwrap_or_default();

    let base_url = Selector::parse("base[href]")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        .and_then(|el| el.value().attr("href"))
        .and_then(|href| page_url.join(href).ok())
        .unwrap_or_else(|| page_url.clone());

    Some(Extracted {
        title,
        body_html,
        base_url,
    })
}

fn select_text(doc: &Html, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    let el = doc.select(&sel).next()?;
    let text = paper_reader_shared::normalize_whitespace(&el.text().collect::<String>());
    (!text.is_empty()).then_some(text)
}

/// ammonia で無害化する。相対 URL は `base` で絶対 URL に書き換える（`#...` はそのまま）。
pub fn sanitize(html: &str, base: &Url) -> String {
    let base = base.clone();
    let rewrite = url_rewriter(move |u| {
        if u.starts_with('#') {
            return Some(Cow::Borrowed(u));
        }
        base.join(u).ok().map(|abs| Cow::Owned(abs.to_string()))
    });

    let mut builder = ammonia::Builder::default();
    builder
        .add_tags(MATHML_TAGS)
        .add_tags(["article", "section", "figure", "figcaption", "cite", "math"])
        .add_generic_attributes(["class", "id", "title", "lang", "dir"])
        .add_tag_attributes("img", ["width", "height", "loading"])
        .add_tag_attributes("td", ["colspan", "rowspan", "align"])
        .add_tag_attributes("th", ["colspan", "rowspan", "align", "scope"])
        .url_relative(ammonia::UrlRelative::Custom(Box::new(rewrite)))
        .attribute_filter(|_element, attr, value| {
            if attr == "id" && RESERVED_IDS.contains(&value) {
                None
            } else {
                Some(value.into())
            }
        });
    for tag in MATHML_TAGS {
        builder.add_tag_attributes(tag, MATHML_ATTRS);
    }
    // 念のため明示的に除去する（ammonia の既定でも許可されていない）
    builder.rm_tags([
        "script", "style", "iframe", "form", "input", "button", "object", "embed",
    ]);
    let mut clean_content: HashSet<&str> = HashSet::new();
    clean_content.extend(["script", "style", "noscript", "template"]);
    builder.clean_content_tags(clean_content);

    builder.clean(html).to_string()
}

/// クロージャの引数と戻り値のライフタイムを結びつけるための補助。
fn url_rewriter<F>(f: F) -> F
where
    F: Fn(&str) -> Option<Cow<'_, str>> + Send + Sync,
{
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://arxiv.org/html/1706.03762").unwrap()
    }

    #[test]
    fn removes_scripts_and_handlers() {
        let dirty = r#"<p onclick="alert(1)" style="color:red">Hello<script>alert(2)</script></p>
            <img src="x.png" onerror="alert(3)">
            <a href="javascript:alert(4)">link</a>
            <iframe src="https://evil.example/"></iframe>
            <form action="/steal"><input name="pw"></form>
            <style>body{display:none}</style>"#;
        let clean = sanitize(dirty, &base());
        for bad in [
            "onclick",
            "onerror",
            "style",
            "<script",
            "alert(2)",
            "javascript:",
            "<iframe",
            "<form",
            "<input",
            "display:none",
        ] {
            assert!(!clean.contains(bad), "{bad} が残っている: {clean}");
        }
        assert!(clean.contains("Hello"));
    }

    #[test]
    fn keeps_mathml() {
        let dirty = r#"<p>x <math display="inline" alttext="h_{t}"><semantics><msub><mi>h</mi><mi>t</mi></msub><annotation encoding="application/x-tex">h_{t}</annotation></semantics></math></p>
            <math><mfrac><mn>1</mn><msqrt><mi>d</mi></msqrt></mfrac><msup><mi>x</mi><mn>2</mn></msup><mrow><mo>+</mo><mtext>ok</mtext></mrow></math>"#;
        let clean = sanitize(dirty, &base());
        for tag in [
            "<math",
            "<semantics>",
            "<msub>",
            "<mi>",
            "<annotation",
            "<mfrac>",
            "<msqrt>",
            "<msup>",
            "<mn>",
            "<mrow>",
            "<mo>",
            "<mtext>",
            "display=\"inline\"",
            "encoding=\"application/x-tex\"",
        ] {
            assert!(clean.contains(tag), "{tag} が消えている: {clean}");
        }
    }

    #[test]
    fn mathml_cannot_smuggle_html() {
        let dirty = r#"<math><mtext><table><mglyph><style><img src=x onerror=alert(1)></style></mglyph></table></mtext></math>"#;
        let clean = sanitize(dirty, &base());
        assert!(!clean.contains("onerror"), "{clean}");
    }

    #[test]
    fn keeps_structure() {
        let dirty = r#"<section><h2>1 Intro</h2><p>a</p><ul><li>b</li></ul><ol><li>c</li></ol>
            <table><thead><tr><th colspan="2">h</th></tr></thead><tbody><tr><td rowspan="2">d</td></tr></tbody></table>
            <figure><img src="x.png" alt="fig"><figcaption>Figure 1</figcaption></figure></section>"#;
        let clean = sanitize(dirty, &base());
        for tag in [
            "<section>",
            "<h2>",
            "<p>",
            "<ul>",
            "<ol>",
            "<li>",
            "<table>",
            "<th colspan=\"2\">",
            "<td rowspan=\"2\">",
            "<figure>",
            "<figcaption>",
        ] {
            assert!(clean.contains(tag), "{tag} が消えている: {clean}");
        }
    }

    #[test]
    fn rewrites_relative_urls() {
        let dirty = r##"<img src="1706.03762v7/Figures/a.png"><img src="/static/b.svg">
            <a href="#S1">sec</a><a href="https://example.com/x">ext</a>"##;
        let clean = sanitize(dirty, &base());
        assert!(clean.contains(r#"src="https://arxiv.org/html/1706.03762v7/Figures/a.png""#));
        assert!(clean.contains(r#"src="https://arxiv.org/static/b.svg""#));
        assert!(clean.contains(r##"href="#S1""##));
        assert!(clean.contains(r#"href="https://example.com/x""#));
    }

    #[test]
    fn strips_reserved_ids() {
        let clean = sanitize(
            r#"<div id="egui_canvas">x</div><p id="S1.p1">y</p>"#,
            &base(),
        );
        assert!(!clean.contains("egui_canvas"));
        assert!(clean.contains(r#"id="S1.p1""#));
    }

    #[test]
    fn extracts_article_and_title() {
        let page = r#"<html><head><title>[1706.03762] Attention</title></head><body>
            <nav>menu</nav><article class="ltx_document"><h1 class="ltx_title ltx_title_document">Attention Is
            All You Need</h1><p>Body</p></article><footer>foot</footer></body></html>"#;
        let ex = extract_article(page, &base()).unwrap();
        assert_eq!(ex.title, "Attention Is All You Need");
        assert!(ex.body_html.contains("<p>Body</p>"));
        assert!(!ex.body_html.contains("menu"));
        assert!(!ex.body_html.contains("foot"));
        assert_eq!(ex.base_url, base());
    }

    #[test]
    fn extract_uses_base_href_and_title_fallback() {
        let page = r#"<html><head><base href="/html/2301.00001v2/"><title>T</title></head>
            <body><main><p>x</p></main></body></html>"#;
        let ex = extract_article(page, &base()).unwrap();
        assert_eq!(ex.title, "T");
        assert_eq!(ex.base_url.as_str(), "https://arxiv.org/html/2301.00001v2/");
    }

    #[test]
    fn extract_none_without_body() {
        assert!(extract_article("<html><body><p>no article</p></body></html>", &base()).is_none());
    }
}
