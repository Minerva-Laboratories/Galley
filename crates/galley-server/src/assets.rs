//! The web app, embedded at build time from `web/dist`. An unknown path falls back to
//! `index.html`, so a client-side route can deep-link. In `--dev` the Vite server proxies to this
//! server instead.

use axum::http::{header, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../web/dist"]
struct WebAssets;

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if let Some(file) = WebAssets::get(path).filter(|_| !path.is_empty()) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let cache = if path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            [
                (header::CONTENT_TYPE, mime.as_ref().to_string()),
                (header::CACHE_CONTROL, cache.to_string()),
            ],
            file.data.into_owned(),
        )
            .into_response();
    }
    match WebAssets::get("index.html") {
        Some(index) => Html(index.data.into_owned()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Html(
                "<p>The web app is not built into this binary. Run <code>cd web &amp;&amp; npm run build</code> \
                 and rebuild, or start <code>npm run dev</code> and open the Vite URL.</p>",
            ),
        )
            .into_response(),
    }
}
