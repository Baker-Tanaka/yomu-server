//! dist/ の静的配信（trunk build の成果物）。
//!
//! - `xxx.br` / `xxx.gz`（deploy/build.sh が作る）があればそれを返し、無ければ CompressionLayer が圧縮する
//! - ファイル名にハッシュが付いたもの（`paper-reader-web-0123456789abcdef_bg.wasm` など）は長期間キャッシュさせる

use std::path::Path;

use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use tower_http::services::{ServeDir, ServeFile};

pub fn service(static_dir: &Path) -> ServeDir<ServeFile> {
    let index = static_dir.join("index.html");
    ServeDir::new(static_dir)
        .precompressed_br()
        .precompressed_gzip()
        // クライアント側のルーティングは無いが、不明なパスでもアプリを表示する
        .fallback(
            ServeFile::new(index)
                .precompressed_br()
                .precompressed_gzip(),
        )
}

/// 静的ファイルの応答に Cache-Control を付ける。
pub async fn cache_headers(req: Request, next: Next) -> Response {
    let value = cache_control_for(req.uri().path());
    let mut resp = next.run(req).await;
    if resp.status().is_success()
        && let Some(v) = value
    {
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(v));
    }
    resp
}

/// /webllm/ の応答のヘッダー。端末側（WebLLM）が Cache Storage に保存するので、ブラウザの HTTP キャッシュには
/// 頼らず毎回確かめさせる（変わっていなければ 304）。種類の推測もさせない。
pub async fn webllm_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp
}

/// パスに応じた Cache-Control。API には付けない。
pub fn cache_control_for(path: &str) -> Option<&'static str> {
    if path.starts_with("/api/") || path == "/api" {
        return None;
    }
    let file = path.rsplit('/').next().unwrap_or("");
    if has_content_hash(file) {
        Some("public, max-age=31536000, immutable")
    } else {
        // index.html など。毎回更新を確認させる（変わっていなければ 304）
        Some("no-cache")
    }
}

/// trunk が付ける `-<16 桁の 16 進>` がファイル名にあるか。
fn has_content_hash(file: &str) -> bool {
    let stem = file.split('.').next().unwrap_or("");
    let stem = stem.strip_suffix("_bg").unwrap_or(stem);
    match stem.rsplit_once('-') {
        Some((name, hash)) => {
            !name.is_empty() && hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_control() {
        let immutable = Some("public, max-age=31536000, immutable");
        assert_eq!(
            cache_control_for("/paper-reader-web-9a20ea7eb2fd92a8_bg.wasm"),
            immutable
        );
        assert_eq!(
            cache_control_for("/paper-reader-web-9a20ea7eb2fd92a8.js"),
            immutable
        );
        assert_eq!(cache_control_for("/paper-4eb054c811991c18.css"), immutable);
        assert_eq!(cache_control_for("/"), Some("no-cache"));
        assert_eq!(cache_control_for("/index.html"), Some("no-cache"));
        // ハッシュでないもの
        assert_eq!(cache_control_for("/paper-reader.css"), Some("no-cache"));
        assert_eq!(cache_control_for("/x-0123.js"), Some("no-cache"));
        assert_eq!(cache_control_for("/-9a20ea7eb2fd92a8.js"), Some("no-cache"));
        assert_eq!(cache_control_for("/api/health"), None);
    }
}
