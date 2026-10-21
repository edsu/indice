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
//! What crawlers may list and fetch is decided here too, for the same two
//! origins: see [`CrawlPolicy`].
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

/// What crawlers may do on one origin, in two halves:
///
/// - **List** only indice's own pages: the homepage, collections, crawls and
///   notes, which are finding aids and exist to be found. Everything else is
///   marked `X-Robots-Tag: noindex`: replay and the images it serves, which are
///   other people's pages captured by someone else, search results, the
///   workroom and the API.
/// - **Fetch** anything except what is expensive to serve: whole WACZs, search
///   results (every facet makes a new URL), the workroom and the API. Those are
///   `Disallow`ed in `robots.txt`.
///
/// The halves differ because a crawler that obeys `robots.txt` never fetches a
/// disallowed URL, so it never sees that URL's `noindex`, and may still list
/// the bare address it found in a link. So what should stay out of results is
/// left fetchable and marked `noindex`, and only what should not be fetched at
/// all is disallowed.
///
/// Patterns are `robots.txt` prefixes, with `*` for any run of characters.
pub(super) struct CrawlPolicy {
    disallow: &'static [&'static str],
    noindex: &'static [&'static str],
}

const APP_CRAWL: CrawlPolicy = CrawlPolicy {
    disallow: &["/files/", "/search", "/api/", "/manage/"],
    noindex: &[
        "/replay/",
        "/files/",
        "/thumb/",
        "/collection-thumb/",
        "/collection/*/replay.json",
        "/collection/*/pages",
        "/search",
        "/api/",
        "/manage/",
    ],
};

/// The replay origin serves nothing to list, and its WACZs are not to be
/// fetched.
const REPLAY_CRAWL: CrawlPolicy = CrawlPolicy {
    disallow: &["/files/"],
    noindex: &["/"],
};

impl CrawlPolicy {
    fn robots(&self) -> String {
        let mut out = String::from("User-agent: *\n");
        for path in self.disallow {
            out.push_str(&format!("Disallow: {path}\n"));
        }
        out
    }

    fn noindex(&self, path: &str) -> bool {
        self.noindex.iter().any(|p| robots_match(p, path))
    }
}

/// The app origin's `robots.txt`, built once.
pub(super) fn app_robots() -> &'static str {
    static ROBOTS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| APP_CRAWL.robots());
    &ROBOTS
}

/// The replay origin's `robots.txt`, built once.
pub(super) fn replay_robots() -> &'static str {
    static ROBOTS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| REPLAY_CRAWL.robots());
    &ROBOTS
}

/// Whether `path` matches a `robots.txt` pattern: a prefix in which `*`
/// stands for any run of characters.
fn robots_match(pattern: &str, path: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = path.strip_prefix(first) else {
        return false;
    };
    for part in parts {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

/// The headers one router adds to every response.
#[derive(Clone)]
pub(super) struct SecurityHeaders {
    pairs: Arc<Vec<(HeaderName, HeaderValue)>>,
    crawl: &'static CrawlPolicy,
}

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
        Self::from_pairs(
            vec![
                (header::CONTENT_SECURITY_POLICY, csp),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
                (header::REFERRER_POLICY, REFERRER_POLICY.into()),
            ],
            &APP_CRAWL,
        )
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
        Self::from_pairs(pairs, &REPLAY_CRAWL)
    }

    fn from_pairs(pairs: Vec<(HeaderName, String)>, crawl: &'static CrawlPolicy) -> Self {
        Self {
            pairs: Arc::new(
                pairs
                    .into_iter()
                    // Every value is built here from validated origins and fixed
                    // text, so one that is not a header value is a bug in this
                    // file.
                    .map(|(k, v)| (k, HeaderValue::from_str(&v).expect("a header value")))
                    .collect(),
            ),
            crawl,
        }
    }

    pub(super) async fn apply(
        self,
        req: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> Response {
        let noindex = self.crawl.noindex(req.uri().path());
        let mut res = next.run(req).await;
        for (name, value) in self.pairs.iter() {
            res.headers_mut().insert(name.clone(), value.clone());
        }
        if noindex {
            res.headers_mut().insert(
                HeaderName::from_static("x-robots-tag"),
                HeaderValue::from_static("noindex"),
            );
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

#[cfg(test)]
mod tests {
    use super::robots_match;

    #[test]
    fn a_pattern_is_a_prefix_with_wildcards() {
        assert!(robots_match("/a/*/b/*/c", "/a/1/2/b/3/c"));
        assert!(!robots_match("/a/*/b/*/c", "/a/1/c/b"));
        assert!(robots_match("/replay/", "/replay/viewer"));
        assert!(robots_match("/search", "/search"));
        assert!(robots_match(
            "/collection/*/pages",
            "/collection/demo/pages"
        ));
        assert!(!robots_match("/collection/*/pages", "/collection/demo"));
        assert!(!robots_match("/replay/", "/collection/replay/x"));
        assert!(!robots_match("/files/", "/"));
    }
}
