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
    for path in ["/", "/replay/viewer", "/assets/app.css", "/no-such-page"] {
        let h = headers(&app, Method::GET, path, &[]).await;
        assert_common(&h, path);
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

/// Every crawler may index indice's own pages and none may index the rest:
/// replay, the bytes and images it serves, search results, the workroom and
/// the API. `robots.txt` says so to crawlers that read it, and `X-Robots-Tag`
/// to those that don't. The replay origin is kept out entirely.
#[tokio::test]
async fn crawlers_may_index_only_indices_own_pages() {
    let tmp = TempDir::new().unwrap();
    let Routers { app, replay } = routers(tmp.path(), config());

    let robots = body(&app, "/robots.txt").await;
    assert!(robots.contains("User-agent: *\n"), "{robots}");
    let kept_out = [
        "/replay/viewer",
        "/files/x",
        "/thumb/x",
        "/collection-thumb/x",
        "/collection/x/replay.json",
        "/collection/x/pages",
        "/search?q=a",
        "/api/search?q=a",
        "/manage/add",
    ];
    for path in kept_out {
        assert!(
            noindex(&headers(&app, Method::GET, path, &[]).await),
            "{path} should carry noindex"
        );
    }
    for prefix in ["/replay/", "/files/", "/search", "/api/", "/manage/"] {
        assert!(
            robots.contains(&format!("Disallow: {prefix}\n")),
            "{prefix} missing from {robots}"
        );
    }
    for path in [
        "/",
        "/collection/x",
        "/crawl/x",
        "/collection/x/annotations",
    ] {
        assert!(
            !noindex(&headers(&app, Method::GET, path, &[]).await),
            "{path} should be indexable"
        );
        assert!(!robots.contains(&format!("Disallow: {path}\n")), "{robots}");
    }

    let robots = body(&replay, "/robots.txt").await;
    assert!(robots.contains("User-agent: *\nDisallow: /\n"), "{robots}");
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
