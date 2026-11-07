//! The reader's interface language on the app origin: negotiated from
//! `Accept-Language` against the shipped catalogs, falling back to
//! `site.language`, and announced to caches with `Vary`.

use std::sync::Arc;

use axum::http::{header, request::Parts, HeaderMap, HeaderValue};
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

/// Add `Accept-Language` to `Vary` on every response whose text may depend on
/// the language, which is all of them but the static files: those are the same
/// for everyone and should stay cacheable as one copy. Appended, because a
/// handler may already vary on something else (`Accept`, on a note). The
/// viewer page itself is at `/crawl/…` and `/collection/…`, not under `/replay/`
/// (which holds only its script and style), so it varies like any page.
pub(super) async fn vary_accept_language(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path();
    let is_static = path.starts_with("/assets/") || path.starts_with("/replay/");
    let mut res = next.run(req).await;
    if !is_static {
        res.headers_mut()
            .append(header::VARY, HeaderValue::from_static("accept-language"));
    }
    res
}
