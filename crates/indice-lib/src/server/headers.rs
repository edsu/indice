//! Security headers, one policy per origin.
//!
//! indice's two origins want opposite things from a browser. The app origin
//! runs only indice's own code, so it gets a Content-Security-Policy that
//! allows nothing else: no inline script, nothing loaded from another site, and
//! no framing by anyone. No framing because of clickjacking: a page that frames
//! the workroom under a decoy turns a curator's click into a Delete, and the
//! CSRF check cannot see that, since the click lands on indice's own page.
//!
//! The replay origin exists to run archived pages' code, whatever it is, so a
//! script policy there would protect nothing and break replay; it gets only a
//! rule about who may frame it. What keeps archived
//! script away from indice is the origin itself (see [`super::ReplayOrigin`]),
//! not a header on it.
//!
//! Both get `nosniff` and a referrer policy. `nosniff` because `/files/{id}`
//! serves archived bytes as `application/octet-stream`, and sniffing is how a
//! stored file becomes a page. The referrer policy because an archive's URLs
//! say what someone is reading: a search query, or the archived address of the
//! page in the viewer.

use std::sync::Arc;

use axum::http::{header, HeaderName, HeaderValue};
use axum::response::Response;

use super::ReplayOrigin;

/// The headers one router adds to every response.
#[derive(Clone)]
pub(super) struct SecurityHeaders(Arc<Vec<(HeaderName, HeaderValue)>>);

/// `same-origin`: indice's own navigations keep their `Referer`, which
/// `/manage/login` reads to send you back where you were, and nothing else gets
/// one. Not the browser's default, which would still send the origin to every
/// site a reader follows a link to.
const REFERRER_POLICY: &str = "same-origin";

impl SecurityHeaders {
    /// For the app origin. `replay` is the one origin its pages may frame; the
    /// viewer frames it, and nothing else frames anything.
    ///
    /// No `form-action`. Browsers check it on every redirect a form's
    /// submission follows, and logout is a form whose POST redirects to the
    /// login service's sign-out, which may redirect on to an identity
    /// provider indice cannot name. Restricting it would break single
    /// sign-out to guard against a form injected into a page, and no markup a
    /// visitor supplies reaches a page unsanitized.
    pub(super) fn app(replay: Option<&ReplayOrigin>) -> Self {
        let frame_src = replay.map_or("'none'", ReplayOrigin::as_str);
        let csp = format!(
            "default-src 'self'; \
             script-src 'self'; \
             style-src 'self'; \
             style-src-attr 'unsafe-inline'; \
             img-src 'self'; \
             connect-src 'self'; \
             frame-src {frame_src}; \
             frame-ancestors 'none'; \
             base-uri 'none'; \
             object-src 'none'"
        );
        // `style-src-attr 'unsafe-inline'` is for two computed values that
        // have no class to live in: a thumbnail placeholder's hue and a
        // timeline bar's height (views/mod.rs, views/search.rs). Inline style
        // attributes can restyle a page but not run anything, and no markup
        // a visitor supplies reaches the page unsanitized.
        Self::from_pairs(vec![
            (header::CONTENT_SECURITY_POLICY, csp),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
            (header::REFERRER_POLICY, REFERRER_POLICY.into()),
        ])
    }

    /// For the replay origin. `ancestors` are the origins the viewer is
    /// served from; they, and the replay origin itself, may frame replay.
    /// Itself, because `/replay-frame` frames ReplayWeb.page's shell at
    /// `/replay/`, and `frame-ancestors` checks every ancestor: without
    /// `'self'` the shell is refused whenever the server rather than the service
    /// worker answers it, which is the first visit's race (see `replay_index`).
    ///
    /// Empty when indice cannot know them, which is only a workstation router
    /// built without listeners, and then replay says nothing about framing
    /// rather than guess. A server always knows, because it will not start
    /// without `--site-url`.
    pub(super) fn replay(ancestors: &[String]) -> Self {
        let mut pairs = vec![
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::REFERRER_POLICY, REFERRER_POLICY.to_string()),
        ];
        if !ancestors.is_empty() {
            pairs.push((
                header::CONTENT_SECURITY_POLICY,
                format!("frame-ancestors 'self' {}", ancestors.join(" ")),
            ));
        }
        Self::from_pairs(pairs)
    }

    fn from_pairs(pairs: Vec<(HeaderName, String)>) -> Self {
        Self(Arc::new(
            pairs
                .into_iter()
                // Every value is built here from validated origins and fixed
                // text, so one that is not a header value is a bug in this file.
                .map(|(k, v)| (k, HeaderValue::from_str(&v).expect("a header value")))
                .collect(),
        ))
    }

    pub(super) async fn apply(
        self,
        req: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> Response {
        let mut res = next.run(req).await;
        for (name, value) in self.0.iter() {
            res.headers_mut().insert(name.clone(), value.clone());
        }
        res
    }
}

/// The origins a browser may use for an app listener on this machine. A
/// loopback listener is reached as its address or as `localhost`, and the two
/// are different origins, so both are listed.
pub(super) fn loopback_origins(addr: std::net::SocketAddr) -> Vec<String> {
    vec![
        format!("http://{addr}"),
        format!("http://localhost:{}", addr.port()),
    ]
}
