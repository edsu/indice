//! Integration tests for management mode (`serve --manage`): the opt-in
//! add-archive endpoints. These exercise the real HTTP path — POST a job, stream
//! its Server-Sent-Events progress to completion, and confirm the read-only
//! searcher hot-reloads so the newly-indexed crawl becomes searchable without a
//! restart — and confirm the routes are absent in the default read-only server.

use std::path::Path;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(FIXTURES).join(name)
}

/// A ureq agent that returns 4xx/5xx as normal responses (rather than errors), so
/// tests can assert on status codes uniformly.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// GET a URL and return `(status, body)`. ureq is blocking, so run it off the
/// executor via `spawn_blocking`; awaiting the handle lets the in-process server
/// task make progress meanwhile (mirrors `tests/app_server.rs`).
async fn get(url: String) -> (u16, String) {
    tokio::task::spawn_blocking(move || {
        let mut res = agent().get(&url).call().unwrap();
        let status = res.status().as_u16();
        let body = res.body_mut().read_to_string().unwrap();
        (status, body)
    })
    .await
    .unwrap()
}

/// Start a server on an ephemeral localhost port; returns `(base_url, handle)`.
async fn serve(home: std::path::PathBuf, manage: bool) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        indice_lib::server::serve_on_listener(listener, &home, None, manage)
            .await
            .unwrap();
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

#[tokio::test]
async fn manage_add_archive_indexes_and_reloads_search() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let (base, server) = serve(home.clone(), true).await;

    // Precondition: empty index, so search finds nothing.
    let (status, body) = get(format!("{base}/api/search?q=example")).await;
    assert_eq!(status, 200);
    let before: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(before["total"], 0, "index should start empty");

    // POST an add-archive job for a local fixture WACZ (the native-dialog path case).
    let post_url = format!("{base}/api/archives");
    let path = fixture("simple.wacz").to_string_lossy().to_string();
    let body = serde_json::json!({ "path": path, "collection": "test" }).to_string();
    let job: serde_json::Value = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&post_url)
            .header("content-type", "application/json")
            .send(body)
            .unwrap();
        assert_eq!(res.status().as_u16(), 202, "add-archive should be accepted");
        serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap();
    let job_id = job["job"].as_u64().expect("response carries a job id");

    // Stream the job's SSE progress to completion — the stream closes when the job
    // finishes and its sender drops.
    let (status, events) = get(format!("{base}/api/archives/{job_id}/events")).await;
    assert_eq!(status, 200, "SSE endpoint should stream");
    assert!(
        events.contains("event: done"),
        "job should signal completion; got:\n{events}"
    );
    assert!(
        !events.contains("event: error"),
        "job should not error; got:\n{events}"
    );

    // The crawl is now recorded in the manifest...
    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    assert_eq!(manifest.waczs.len(), 1, "exactly one crawl indexed");
    assert_eq!(manifest.waczs[0].collection, "test");

    // ...and the read-only searcher was hot-reloaded, so it finds it now. This is
    // the behavior the whole reload machinery exists for.
    let (status, body) = get(format!("{base}/api/search?q=example")).await;
    assert_eq!(status, 200);
    let after: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        after["total"].as_u64().unwrap() > 0,
        "reloaded searcher should find the new crawl; got:\n{body}"
    );

    // A consumed job's SSE receiver is gone; reconnecting is a 404.
    let (status, _) = get(format!("{base}/api/archives/{job_id}/events")).await;
    assert_eq!(status, 404, "a job's progress is consumed once");

    server.abort();
}

#[tokio::test]
async fn read_only_server_has_no_add_archive_route() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(tmp.path().to_path_buf(), false).await;

    // The write route is not mounted in the default (read-only) server.
    let post_url = format!("{base}/api/archives");
    let body = serde_json::json!({ "path": "x", "collection": "y" }).to_string();
    let status = tokio::task::spawn_blocking(move || {
        agent()
            .post(&post_url)
            .header("content-type", "application/json")
            .send(body)
            .unwrap()
            .status()
            .as_u16()
    })
    .await
    .unwrap();
    assert_eq!(
        status, 404,
        "management route must be absent in read-only mode"
    );

    server.abort();
}
