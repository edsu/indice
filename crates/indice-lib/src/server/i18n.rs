//! The reader's interface language on the app origin: negotiated from
//! `Accept-Language` against the shipped catalogs, falling back to
//! `site.language`, and announced to caches with `Vary`.

use std::sync::Arc;

use axum::http::{header, request::Parts, HeaderMap, HeaderValue, Method};
use axum::response::Response;

use super::AppState;
use crate::i18n::{negotiate, L10n};

/// The language for a request with these headers. A header that is not text
/// (or not there) is the same as none.
pub(super) fn negotiate_headers(state: &AppState, headers: &HeaderMap) -> L10n {
    let accept = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());
    negotiate(accept, &state.site.language)
}

/// For handlers that answer without a page (errors, event streams) and so have
/// no [`crate::views::Chrome`] to take the language from.
impl axum::extract::FromRequestParts<Arc<AppState>> for L10n {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        Ok(negotiate_headers(state, &parts.headers))
    }
}

/// Whether the answer at `path` is the same for every reader, whatever their
/// language: static files, archive bytes and thumbnails, the health probe and
/// robots.txt, the documented public API (`GET /api/search`, `GET
/// /api/annotations`), and the manifests and page lists ReplayWeb.page reads.
/// Every route the replay router serves is here, so the replay origin never
/// translates. These answer in English, refusals included, and do not vary.
pub(super) fn same_for_every_language(method: &Method, path: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "/assets/",
        "/replay/",
        "/files/",
        "/thumb/",
        "/collection-thumb/",
    ];
    const EXACT: &[&str] = &["/health", "/robots.txt", "/replay-frame", "/api/search"];
    if PREFIXES.iter().any(|p| path.starts_with(p)) || EXACT.contains(&path) {
        return true;
    }
    // The read; a curator's `POST /api/annotations` is the workroom's, and
    // translated.
    if path == "/api/annotations" {
        return matches!(*method, Method::GET | Method::HEAD);
    }
    // `/crawl/{id}/replay.json`, `/collection/{id}/pages` and the like, and not
    // a replayed page whose own address ends in `/pages`, which has more parts.
    matches!(
        path.split('/').collect::<Vec<_>>()[..],
        ["", "crawl" | "collection", _, "replay.json" | "pages"]
    )
}

/// The language for a refusal made before any handler runs (a rate limit, a
/// busy server): the reader's on pages, English where
/// [`same_for_every_language`] holds.
pub(super) fn refusal_language(state: &AppState, req: &axum::extract::Request) -> L10n {
    if same_for_every_language(req.method(), req.uri().path()) {
        L10n::default()
    } else {
        negotiate_headers(state, req.headers())
    }
}

/// Add `Accept-Language` to `Vary` on every response whose text may depend on
/// the language: everything but what [`same_for_every_language`] names, which
/// should stay cacheable as one copy. Appended, because a handler may already
/// vary on something else (`Accept`, on a note). The viewer page itself is at
/// `/crawl/…` and `/collection/…`, not under `/replay/` (which holds only its
/// script and style), so it varies like any page.
pub(super) async fn vary_accept_language(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let same = same_for_every_language(req.method(), req.uri().path());
    let mut res = next.run(req).await;
    if !same {
        res.headers_mut()
            .append(header::VARY, HeaderValue::from_static("accept-language"));
    }
    res
}
