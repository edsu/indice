//! Replay on an origin of its own.
//!
//! Archived pages run their own JavaScript. indice serves replay from a second
//! listener, so that script runs on an origin that holds nothing: no write
//! routes, no identity, and no way into the viewer that frames it. These tests
//! pin which origin serves what. The browser test
//! `a_hostile_capture_cannot_write_as_the_reader` shows the browser keeping a
//! capture's script out; these show there is nothing on the replay side for it
//! to use, and nothing on the app side that would replay a capture.

use std::path::Path;

use axum::body::{to_bytes, Body};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use indice_lib::server::{Access, ReplayOrigin, Routers, ServerConfig};
use tempfile::TempDir;
use tower::ServiceExt;

const REPLAY: &str = "https://replay.example.test";
const USER_HEADER: &str = "x-forwarded-email";
const SECRET: &str = "replay-origin-secret";

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(name)
}

/// A server's two routers, with a curator on the roster so a write that
/// reached a handler would be allowed to run.
fn server(home: &Path) -> Routers {
    std::fs::write(
        home.join("users.yaml"),
        "users:\n  - id: curator@x.edu\n    role: curator\n",
    )
    .unwrap();
    let mut config = ServerConfig::new(Access::proxy(USER_HEADER, SECRET));
    config.replay_origin = Some(ReplayOrigin::parse(REPLAY).unwrap());
    indice_lib::server::routers(home, config).unwrap()
}

/// The headers the trusted proxy adds for a signed-in curator.
fn as_curator() -> Vec<(&'static str, &'static str)> {
    vec![
        (USER_HEADER, "curator@x.edu"),
        ("x-indice-auth-secret", SECRET),
    ]
}

async fn call(
    router: &axum::Router,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

async fn get(router: &axum::Router, path: &str) -> (StatusCode, String) {
    let (status, _, body) = call(router, Method::GET, path, &[]).await;
    (status, body)
}

// ── The app origin ────────────────────────────────────────────────────────────

/// The viewer frames the configured replay origin, and does not run
/// ReplayWeb.page itself.
#[tokio::test]
async fn the_viewer_frames_the_configured_replay_origin() {
    let tmp = TempDir::new().unwrap();
    let Routers { app, .. } = server(tmp.path());
    let (status, html) = get(&app, "/replay/viewer").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains(&format!("content=\"{REPLAY}\"")),
        "the viewer must be told the replay origin"
    );
    assert!(
        !html.contains("{{replay_origin}}"),
        "the placeholder must be filled"
    );
    // The code that builds the frame's URL, not a comment naming it.
    let (status, js) = get(&app, "/replay/viewer.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        js.contains("'/replay-frame?'"),
        "viewer must embed the frame"
    );
    assert!(
        !html.contains("ui.js"),
        "viewer must not load ReplayWeb.page"
    );
}

/// Anything on the app origin that could boot ReplayWeb.page could be framed
/// by a stranger's page with a `source` of their choosing, which would put
/// archived script back on indice's origin. So none of it is there.
#[tokio::test]
async fn the_app_origin_serves_nothing_that_replays() {
    let tmp = TempDir::new().unwrap();
    let Routers { app, .. } = server(tmp.path());
    for path in [
        "/replay-frame",
        "/replay/",
        "/replay/?source=/files/x",
        "/replay/ui.js",
        "/replay/sw.js",
        "/replay/agent.js",
        "/replay/index.html",
        "/replay/frame.html",
        "/replay/viewer.html",
    ] {
        // 404, or on a server the forward-auth guard's 403 for a path the app
        // does not have (rustyweb-n887); either way, not served.
        let (status, body) = get(&app, path).await;
        assert!(
            !status.is_success() && !body.contains("ui.js") && !body.contains("replay-web-page"),
            "the app must not serve {path}: {status}"
        );
    }
}

/// The viewer's own script and style are the app's, cached by ETag.
#[tokio::test]
async fn the_viewer_files_are_the_apps() {
    let tmp = TempDir::new().unwrap();
    let Routers { app, replay } = server(tmp.path());
    for path in [
        "/replay/viewer.js",
        "/replay/viewer.css",
        "/replay/annotations.js",
        "/replay/annotations.css",
    ] {
        let (status, headers, _) = call(&app, Method::GET, path, &[]).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers.get("cache-control").unwrap(), "no-cache");
        let etag = headers.get("etag").expect("an ETag").to_str().unwrap();
        let (again, _, body) = call(&app, Method::GET, path, &[("if-none-match", etag)]).await;
        assert_eq!(again, StatusCode::NOT_MODIFIED, "{path}");
        assert!(body.is_empty());
        // And not replay's: the panel there would talk to an API that is not.
        assert_eq!(get(&replay, path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
}

/// The viewer's ETag covers the replay origin, so a server restarted with a
/// new replay URL is not answered from a cached page framing the old one.
#[tokio::test]
async fn the_viewers_etag_follows_the_replay_origin() {
    let tmp = TempDir::new().unwrap();
    let Routers { app, .. } = server(tmp.path());
    let (_, a, _) = call(&app, Method::GET, "/replay/viewer", &[]).await;
    let mut config = ServerConfig::new(Access::proxy(USER_HEADER, SECRET));
    config.replay_origin = Some(ReplayOrigin::parse("https://elsewhere.example.test").unwrap());
    let other = indice_lib::server::routers(tmp.path(), config).unwrap().app;
    let (_, b, _) = call(&other, Method::GET, "/replay/viewer", &[]).await;
    assert_ne!(a.get("etag"), b.get("etag"));
}

/// A router built without a replay origin says so, rather than framing
/// replay somewhere it is not.
#[tokio::test]
async fn a_viewer_without_a_replay_origin_says_so() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("users.yaml"), "users: []\n").unwrap();
    let app = indice_lib::server::router(
        tmp.path(),
        ServerConfig::new(Access::proxy(USER_HEADER, SECRET)),
    )
    .unwrap();
    let (status, body) = get(&app, "/replay/viewer").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("replay"), "{body}");
}

// ── The replay origin ─────────────────────────────────────────────────────────

/// The frame mounts ReplayWeb.page, refuses to run unframed, and loads the
/// agent; the shell and ReplayWeb.page's files are served beside it.
#[tokio::test]
async fn the_replay_origin_serves_the_frame_and_replayweb() {
    let tmp = TempDir::new().unwrap();
    let Routers { replay, .. } = server(tmp.path());

    let (status, frame) = get(&replay, "/replay-frame").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        frame.contains("replay-web-page"),
        "frame must mount the component"
    );
    // The frame is outside /replay/, so only an absolute replaybase makes the
    // service worker resolve to /replay/sw.js.
    assert!(frame.contains("setAttribute('replaybase', '/replay/')"));
    assert!(
        frame.contains("setAttribute('requireSubdomainIframe', '')"),
        "replay must refuse to run outside a cross-origin frame"
    );
    assert!(
        frame.contains("/replay/agent.js"),
        "frame must load the agent"
    );

    // ReplayWeb.page frames its replay base before the service worker may
    // control the page; the shell it gets then has to boot ReplayWeb.page.
    // See `replay_index` in server/assets.rs.
    let (status, shell) = get(&replay, "/replay/?source=x").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        shell.contains("ui.js"),
        "the shell must load ReplayWeb.page"
    );

    assert_eq!(get(&replay, "/replay/sw.js").await.0, StatusCode::OK);
    // ReplayWeb.page comes with indice's guard ahead of it, in one file, so
    // whatever page loads it is checked first; the guard alone is not served.
    let (status, ui) = get(&replay, "/replay/ui.js").await;
    assert_eq!(status, StatusCode::OK);
    let guard_at = ui.find("indice: replay refused").expect("the guard");
    let rwp_at = ui
        .find("customElements.define(\"replay-web-page\"")
        .expect("ReplayWeb.page");
    assert!(
        guard_at < rwp_at,
        "the guard must run before ReplayWeb.page"
    );
    assert_eq!(
        get(&replay, "/replay/guard.js").await.0,
        StatusCode::NOT_FOUND
    );
    // The agent is what tells the viewer where the reader is.
    let (status, agent) = get(&replay, "/replay/agent.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(agent.contains("rwp-url-change"));
}

/// Nothing on the replay origin writes, manages, or renders a page with an
/// identity in it, even for a request carrying a curator's proxy headers.
/// Absent routes, rather than refused ones: there is nothing to get wrong.
#[tokio::test]
async fn the_replay_origin_has_no_write_or_page_routes() {
    let tmp = TempDir::new().unwrap();
    let Routers { replay, .. } = server(tmp.path());
    let curator = as_curator();
    for path in [
        "/api/collections",
        "/api/archives",
        "/api/annotations",
        "/api/collections/x/delete",
        "/api/crawls/x/delete",
        "/logout",
    ] {
        let (status, _, _) = call(&replay, Method::POST, path, &curator).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "POST {path} on replay");
    }
    for path in [
        "/",
        "/search?q=x",
        "/manage/add",
        "/manage/login",
        "/api/search?q=x",
        "/api/annotations?collection=x",
        "/replay/viewer",
        "/crawl/x",
        "/collection/x",
    ] {
        let (status, _, _) = call(&replay, Method::GET, path, &curator).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {path} on replay");
    }
}

/// The replay origin never resolves who is asking, so it never sets the
/// display cookie the app uses to show a signed-in name.
#[tokio::test]
async fn the_replay_origin_resolves_no_identity() {
    let tmp = TempDir::new().unwrap();
    let Routers { replay, .. } = server(tmp.path());
    let (status, headers, _) = call(&replay, Method::GET, "/replay-frame", &as_curator()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("set-cookie").is_none(), "{headers:?}");
}

/// Archive bytes and collection manifests are served on both origins: replay
/// reads them, and the app links to them for download.
#[tokio::test]
async fn replay_data_is_served_on_both_origins() {
    let tmp = TempDir::new().unwrap();
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, "test").unwrap();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let wacz = manifest.waczs[0].id.clone();
    let coll = manifest.collections[0].id.to_string();
    let Routers { app, replay } = server(tmp.path());
    for router in [&app, &replay] {
        let (status, _, _) = call(
            router,
            Method::GET,
            &format!("/files/{wacz}"),
            &[("range", "bytes=0-3")],
        )
        .await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        let (status, _) = get(router, &format!("/collection/{coll}/replay.json")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(get(router, "/health").await.0, StatusCode::OK);
    }
}

// ── Starting up ───────────────────────────────────────────────────────────────

/// A server with no replay origin refuses to start: replay would otherwise
/// run on indice's own origin, which is the hole this closes.
#[tokio::test]
async fn a_server_without_a_replay_origin_refuses_to_start() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("users.yaml"), "users: []\n").unwrap();
    let app = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let replay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let err = indice_lib::server::serve_on_listeners(
        app,
        replay,
        tmp.path(),
        None,
        ServerConfig::new(Access::proxy(USER_HEADER, SECRET)),
        indice_lib::server::Providers::default(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("replay origin"), "{err}");
}

/// A workstation trusts every caller, so its replay listener must be on
/// loopback as much as its app listener.
#[tokio::test]
async fn a_workstation_refuses_a_replay_listener_off_loopback() {
    let tmp = TempDir::new().unwrap();
    let app = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let replay = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let err = indice_lib::server::serve_on_listeners(
        app,
        replay,
        tmp.path(),
        None,
        ServerConfig::new(Access::local("127.0.0.1:0".parse().unwrap()).unwrap()),
        indice_lib::server::Providers::default(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("loopback"), "{err}");
}

/// The viewer compares the replay origin with `MessageEvent.origin` byte for
/// byte, so anything but a bare origin is refused or normalised here, where
/// the operator can see it, rather than silently dropping every message.
#[test]
fn a_replay_url_must_be_an_origin() {
    for (raw, origin, host) in [
        (
            "https://replay.example.org",
            "https://replay.example.org",
            "replay.example.org",
        ),
        (
            "https://replay.example.org/",
            "https://replay.example.org",
            "replay.example.org",
        ),
        (
            "HTTPS://Replay.Example.org",
            "https://replay.example.org",
            "replay.example.org",
        ),
        (
            "https://replay.example.org:443",
            "https://replay.example.org",
            "replay.example.org",
        ),
        (
            "http://127.0.0.1:8081",
            "http://127.0.0.1:8081",
            "127.0.0.1",
        ),
        ("http://[::1]:8081", "http://[::1]:8081", "::1"),
    ] {
        let o = ReplayOrigin::parse(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
        assert_eq!(o.as_str(), origin, "{raw}");
        assert_eq!(o.host(), host, "{raw}");
    }
    for raw in [
        "replay.example.org",
        "/replay",
        "ftp://replay.example.org",
        "https://replay.example.org/replay/",
        "https://replay.example.org/?x=1",
        "https://replay.example.org/#x",
        "https://user:pw@replay.example.org",
        "file:///tmp",
    ] {
        assert!(ReplayOrigin::parse(raw).is_err(), "{raw} should be refused");
    }
    assert_eq!(
        ReplayOrigin::for_listener("[::1]:8081".parse().unwrap()).as_str(),
        "http://[::1]:8081"
    );
}
