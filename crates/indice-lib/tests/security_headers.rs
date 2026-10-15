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

/// Logout is a form whose POST redirects to `INDICE_LOGOUT_REDIRECT`, and
/// browsers check `form-action` on that redirect, so an absolute one is
/// allowed as a form target and a relative one adds nothing.
#[tokio::test]
async fn the_logout_redirect_is_an_allowed_form_target() {
    let tmp = TempDir::new().unwrap();
    let mut c = config();
    c.logout_redirect = Some("https://idp.example.test/logout?rd=x".into());
    let Routers { app, .. } = routers(tmp.path(), c);
    let policy = csp(&headers(&app, Method::GET, "/", &[]).await);
    assert_eq!(
        directive(&policy, "form-action"),
        Some("'self' https://idp.example.test"),
        "{policy}"
    );

    let mut c = config();
    c.logout_redirect = Some("/oauth2/sign_out?rd=/".into());
    let Routers { app, .. } = routers(tmp.path(), c);
    let policy = csp(&headers(&app, Method::GET, "/", &[]).await);
    assert_eq!(
        directive(&policy, "form-action"),
        Some("'self'"),
        "{policy}"
    );
}

/// The replay origin sets no script policy, since running archived script is
/// its job, and says who may frame it when it knows: itself, for ReplayWeb.page's
/// own shell, and the site.
#[tokio::test]
async fn the_replay_origin_says_only_who_may_frame_it() {
    let tmp = TempDir::new().unwrap();
    let mut c = config();
    c.site_authority = Some("archive.example.test".into());
    let Routers { replay, .. } = routers(tmp.path(), c);
    for path in ["/replay-frame", "/replay/", "/replay/sw.js"] {
        let h = headers(&replay, Method::GET, path, &[]).await;
        assert_common(&h, path);
        assert_eq!(
            csp(&h),
            "frame-ancestors 'self' archive.example.test",
            "{path}"
        );
    }

    // A server that does not know its own address says nothing about framing
    // rather than guess, and still refuses sniffing.
    let Routers { replay, .. } = routers(tmp.path(), config());
    let h = headers(&replay, Method::GET, "/replay-frame", &[]).await;
    assert_common(&h, "replay without --site-url");
    assert!(csp(&h).is_empty(), "{}", csp(&h));
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
