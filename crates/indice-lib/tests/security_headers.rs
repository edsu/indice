//! Security headers, origin by origin.
//!
//! The app origin runs only indice's code and says so to the browser; the
//! replay origin runs archived code and says only who may frame it. Both refuse
//! MIME sniffing and keep their URLs out of other sites' `Referer`. The browser
//! test `every_page_runs_under_its_content_security_policy` shows the pages
//! work under the app's policy; these pin what the policy is.

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request};
use indice_lib::server::{Access, ReplayOrigin, Routers, ServerConfig};
use tempfile::TempDir;
use tower::ServiceExt;

const REPLAY: &str = "https://replay.example.test";

fn config() -> ServerConfig {
    let mut config = ServerConfig::new(Access::proxy("x-forwarded-email", "secret"));
    config.set_site_url("https://archive.example.test").unwrap();
    config.replay_origin = Some(ReplayOrigin::parse(REPLAY).unwrap());
    config
}

fn routers(home: &std::path::Path, config: ServerConfig) -> Routers {
    std::fs::write(home.join("users.yaml"), "users: []\n").unwrap();
    indice_lib::server::routers(home, config).unwrap()
}

async fn headers(
    router: &axum::Router,
    method: Method,
    path: &str,
    extra: &[(&str, &str)],
) -> HeaderMap {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .headers()
        .clone()
}

fn csp(h: &HeaderMap) -> String {
    h.get("content-security-policy")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

/// The directives of a policy, so a test can ask for one without caring about
/// the order they are written in.
fn directive<'a>(policy: &'a str, name: &str) -> Option<&'a str> {
    policy
        .split(';')
        .map(str::trim)
        .find_map(|d| d.strip_prefix(name).filter(|rest| rest.starts_with(' ')))
        .map(str::trim)
}

fn assert_common(h: &HeaderMap, what: &str) {
    assert_eq!(
        h.get("x-content-type-options").unwrap(),
        "nosniff",
        "{what}"
    );
    assert_eq!(h.get("referrer-policy").unwrap(), "same-origin", "{what}");
}

#[tokio::test]
async fn the_app_origin_allows_only_its_own_code() {
    let tmp = TempDir::new().unwrap();
    let Routers { app, .. } = routers(tmp.path(), config());
    std::fs::create_dir_all(tmp.path().join("collections/demo")).unwrap();
    std::fs::write(
        tmp.path().join("collections/demo/README.md"),
        "---\nname: demo\n---\n",
    )
    .unwrap();
    for path in [
        "/",
        "/collection/demo/2025/https://example.org/",
        "/assets/app.css",
        "/no-such-page",
    ] {
        let h = headers(&app, Method::GET, path, &[]).await;
        assert_common(&h, path);
        assert_eq!(
            h.get("cross-origin-opener-policy").unwrap(),
            "same-origin",
            "{path}"
        );
        let policy = csp(&h);
        assert_eq!(
            directive(&policy, "script-src"),
            Some("'self'"),
            "{path}: {policy}"
        );
        assert_eq!(
            directive(&policy, "style-src"),
            Some("'self'"),
            "{path}: {policy}"
        );
        assert_eq!(
            directive(&policy, "frame-ancestors"),
            Some("'none'"),
            "{path}: {policy}"
        );
        assert_eq!(
            directive(&policy, "frame-src"),
            Some(REPLAY),
            "{path}: {policy}"
        );
        assert_eq!(
            directive(&policy, "object-src"),
            Some("'none'"),
            "{path}: {policy}"
        );
        assert_eq!(
            directive(&policy, "base-uri"),
            Some("'none'"),
            "{path}: {policy}"
        );
    }
}

/// A refusal is a response like any other: the CSRF guard's 403 carries the
/// policy, because the headers are applied outside it.
#[tokio::test]
async fn a_refusal_carries_the_headers_too() {
    let tmp = TempDir::new().unwrap();
    let Routers { app, .. } = routers(tmp.path(), config());
    let h = headers(
        &app,
        Method::POST,
        "/api/collections",
        &[("origin", "https://elsewhere.example")],
    )
    .await;
    assert_common(&h, "a CSRF refusal");
    assert!(!csp(&h).is_empty());
}

/// No `form-action`: logout is a form whose POST redirects to the login
/// service's sign-out, which may redirect on to an identity provider, and
/// browsers check `form-action` at every hop. A policy naming only the site
/// would break single sign-out.
#[tokio::test]
async fn forms_may_follow_logout_to_the_identity_provider() {
    let tmp = TempDir::new().unwrap();
    let mut c = config();
    c.logout_redirect = Some("/oauth2/sign_out?rd=https%3A%2F%2Fidp.example.test%2Flogout".into());
    let Routers { app, .. } = routers(tmp.path(), c);
    let policy = csp(&headers(&app, Method::GET, "/", &[]).await);
    assert_eq!(directive(&policy, "form-action"), None, "{policy}");
}

/// The replay origin sets no script policy, since running archived script is
/// its job, and says who may frame it: itself, for ReplayWeb.page's own shell,
/// and the site.
#[tokio::test]
async fn the_replay_origin_says_only_who_may_frame_it() {
    let tmp = TempDir::new().unwrap();
    let mut c = config();
    c.set_site_url("https://archive.example.test/").unwrap();
    let Routers { replay, .. } = routers(tmp.path(), c);
    for path in ["/replay-frame", "/replay/", "/replay/sw.js"] {
        let h = headers(&replay, Method::GET, path, &[]).await;
        assert_common(&h, path);
        assert_eq!(
            csp(&h),
            "frame-ancestors 'self' https://archive.example.test",
            "{path}"
        );
    }

    // The site's scheme is kept, so a site on plain http still matches when
    // replay is on https: a host alone there would match only https.
    let mut c = config();
    c.set_site_url("http://archive.internal:8080").unwrap();
    let Routers { replay, .. } = routers(tmp.path(), c);
    let h = headers(&replay, Method::GET, "/replay-frame", &[]).await;
    assert_eq!(
        csp(&h),
        "frame-ancestors 'self' http://archive.internal:8080"
    );
}

/// A workstation knows its own address, under both names a browser may use
/// for it, so replay is framed by the workstation's viewer and nothing else.
#[tokio::test]
async fn a_workstations_replay_may_be_framed_only_by_its_viewer() {
    let tmp = TempDir::new().unwrap();
    let app = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let replay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let app_port = app.local_addr().unwrap().port();
    let replay_url = format!("http://{}/replay-frame", replay.local_addr().unwrap());
    let home = tmp.path().to_path_buf();
    let server = tokio::spawn(async move {
        indice_lib::server::serve_on_listeners(
            app,
            replay,
            &home,
            None,
            ServerConfig::new(Access::local("127.0.0.1:0".parse().unwrap()).unwrap()),
            indice_lib::server::Providers::default(),
        )
        .await
        .unwrap();
    });
    let policy = tokio::task::spawn_blocking(move || {
        ureq::get(&replay_url)
            .call()
            .unwrap()
            .headers()
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    })
    .await
    .unwrap();
    assert_eq!(
        policy,
        format!("frame-ancestors 'self' http://127.0.0.1:{app_port} http://localhost:{app_port}")
    );
    server.abort();
}

/// GET `path` and return the body.
async fn body(router: &axum::Router, path: &str) -> String {
    let res = router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn noindex(h: &HeaderMap) -> bool {
    h.get("x-robots-tag").is_some_and(|v| v == "noindex")
}

/// The paths a `robots.txt` disallows for every crawler.
fn disallowed(robots: &str) -> Vec<String> {
    robots
        .lines()
        .filter_map(|l| l.strip_prefix("Disallow: "))
        .map(str::to_string)
        .collect()
}

/// Whether a crawler reading these `Disallow` patterns would fetch `path`: a
/// pattern is a prefix in which `*` stands for any run of characters.
fn fetchable(patterns: &[String], path: &str) -> bool {
    !patterns.iter().any(|pattern| {
        let mut parts = pattern.split('*');
        let Some(mut rest) = path.strip_prefix(parts.next().unwrap_or_default()) else {
            return false;
        };
        parts.all(|part| match rest.find(part) {
            Some(i) => {
                rest = &rest[i + part.len()..];
                true
            }
            None => false,
        })
    })
}

/// Crawlers may list only indice's own pages, and may fetch anything but what
/// is expensive to serve. The two are different lists because a crawler that
/// obeys `robots.txt` never fetches a disallowed URL and so never sees its
/// `noindex`: what should stay out of results is left fetchable and marked
/// `noindex`, and only what should not be fetched at all is disallowed.
#[tokio::test]
async fn crawlers_list_only_indices_own_pages_and_skip_what_is_costly() {
    let tmp = TempDir::new().unwrap();
    let Routers { app, replay } = routers(tmp.path(), config());

    let robots = body(&app, "/robots.txt").await;
    assert!(robots.starts_with("User-agent: *\n"), "{robots}");
    let patterns = disallowed(&robots);

    // Expensive to serve: not fetched, and not listed either.
    for path in ["/files/x", "/search", "/api/search", "/manage/add"] {
        assert!(!fetchable(&patterns, path), "{path} should be disallowed");
        assert!(
            noindex(&headers(&app, Method::GET, path, &[]).await),
            "{path}"
        );
    }
    // Not indice's own: fetchable, so the crawler sees that it is not to be listed.
    for path in [
        "/crawl/x/2025/https://example.org/",
        "/collection/x/2025/https://example.org/",
        "/thumb/x",
        "/collection-thumb/x",
        "/collection/x/replay.json",
        "/collection/x/pages",
    ] {
        assert!(fetchable(&patterns, path), "{path} should be fetchable");
        assert!(
            noindex(&headers(&app, Method::GET, path, &[]).await),
            "{path} should carry noindex"
        );
    }
    // indice's own pages: fetched and listed.
    for path in [
        "/",
        "/collection/x",
        "/crawl/x",
        "/collection/x/annotations",
    ] {
        assert!(fetchable(&patterns, path), "{path} should be fetchable");
        assert!(
            !noindex(&headers(&app, Method::GET, path, &[]).await),
            "{path} should be indexable"
        );
    }

    // The replay origin: nothing listed, and no WACZ fetched.
    let robots = body(&replay, "/robots.txt").await;
    assert_eq!(robots, "User-agent: *\nDisallow: /files/\n");
    for path in [
        "/replay-frame",
        "/replay/",
        "/files/x",
        "/collection/x/pages",
    ] {
        assert!(
            noindex(&headers(&replay, Method::GET, path, &[]).await),
            "{path} on the replay origin should carry noindex"
        );
    }
}

const SNIPPET: &str = "<script defer data-domain=\"archive.example.test\" \
    src=\"https://plausible.io/js/script.js\"></script>\n\
    <script>window.plausible = window.plausible || function () {};</script>";

/// The policy's `sha256-…` source for an inline script.
fn hash_source(script: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    let digest = sha2::Sha256::digest(script.as_bytes());
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// An analytics snippet in config.yaml goes into every page on the app origin,
/// the replay viewer included, and the app's policy lets in exactly what it
/// needs: its origins for script, requests and images, and its inline script
/// by hash. The replay origin gets neither the snippet nor a looser policy.
#[tokio::test]
async fn an_analytics_snippet_is_let_in_on_the_app_origin_only() {
    let tmp = TempDir::new().unwrap();
    let yaml = serde_saphyr::to_string(&serde_json::json!({
        "analytics": {
            "head": SNIPPET,
            "origins": ["https://plausible.io/js/script.js", "https://stats.example.test:8443"],
        }
    }))
    .unwrap();
    std::fs::write(tmp.path().join("config.yaml"), yaml).unwrap();
    std::fs::create_dir_all(tmp.path().join("collections/demo")).unwrap();
    std::fs::write(
        tmp.path().join("collections/demo/README.md"),
        "---\nname: demo\n---\n",
    )
    .unwrap();
    let Routers { app, replay } = routers(tmp.path(), config());

    let origins = "https://plausible.io https://stats.example.test:8443";
    let inline = hash_source("window.plausible = window.plausible || function () {};");
    for path in [
        "/",
        "/search?q=anything",
        "/collection/demo",
        "/collection/demo/2025/https://example.org/",
    ] {
        let policy = csp(&headers(&app, Method::GET, path, &[]).await);
        assert_eq!(
            directive(&policy, "script-src"),
            Some(format!("'self' {origins} {inline}").as_str()),
            "{path}: {policy}"
        );
        for d in ["connect-src", "img-src"] {
            assert_eq!(
                directive(&policy, d),
                Some(format!("'self' {origins}").as_str()),
                "{path}: {policy}"
            );
        }
        assert_eq!(
            directive(&policy, "style-src"),
            Some("'self'"),
            "{path}: {policy}"
        );
        let page = body(&app, path).await;
        assert!(page.contains(SNIPPET), "{path}: {page}");
        assert!(!page.contains("{{analytics_head}}"), "{path}: {page}");
    }

    for path in ["/replay-frame", "/replay/"] {
        let h = headers(&replay, Method::GET, path, &[]).await;
        assert_eq!(
            csp(&h),
            "frame-ancestors 'self' https://archive.example.test",
            "{path}"
        );
        let page = body(&replay, path).await;
        assert!(!page.contains("plausible"), "{path}: {page}");
    }
}

/// Without `analytics:`, the policy lets in nothing but the app's own code,
/// and the viewer's slot for a snippet is left empty.
#[tokio::test]
async fn without_analytics_the_policy_names_no_other_origin() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("collections/demo")).unwrap();
    std::fs::write(
        tmp.path().join("collections/demo/README.md"),
        "---\nname: demo\n---\n",
    )
    .unwrap();
    let Routers { app, .. } = routers(tmp.path(), config());
    let policy = csp(&headers(&app, Method::GET, "/", &[]).await);
    for d in ["script-src", "connect-src", "img-src"] {
        assert_eq!(directive(&policy, d), Some("'self'"), "{policy}");
    }
    let viewer = body(&app, "/collection/demo/2025/https://example.org/").await;
    assert!(!viewer.contains("{{analytics_head}}"), "{viewer}");
}

/// An origin that can't be written into a policy stops the server, rather
/// than reaching a header.
#[tokio::test]
async fn an_analytics_origin_a_policy_cant_name_stops_the_server() {
    for origins in ["[\"'unsafe-inline'\"]", "[\"plausible.io\"]"] {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("config.yaml"),
            format!("analytics:\n  head: <script src=x></script>\n  origins: {origins}\n"),
        )
        .unwrap();
        std::fs::write(tmp.path().join("users.yaml"), "users: []\n").unwrap();
        let err = indice_lib::server::routers(tmp.path(), config())
            .err()
            .unwrap_or_else(|| panic!("{origins} must stop the server"));
        assert!(
            format!("{err:#}").contains("analytics.origins"),
            "{origins}: {err:#}"
        );
    }
}
