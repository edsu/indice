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
async fn serve(
    home: std::path::PathBuf,
    manage: indice_lib::server::ManageConfig,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        indice_lib::server::serve_on_listener(
            listener,
            &home,
            None,
            manage,
            indice_lib::server::Providers::default(),
        )
        .await
        .unwrap();
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

#[tokio::test]
async fn manage_add_archive_indexes_and_reloads_search() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let (base, server) = serve(home.clone(), indice_lib::server::ManageConfig::local()).await;

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
async fn manage_upload_archive_indexes_and_reloads_search() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let (base, server) = serve(home.clone(), indice_lib::server::ManageConfig::local()).await;

    // Hand-build a multipart/form-data body: the `collection` text field + the
    // `.wacz` bytes as the `file` field.
    let boundary = "----indiceUploadTest";
    let wacz = std::fs::read(fixture("simple.wacz")).unwrap();
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"collection\"\r\n\r\nuploaded\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"simple.wacz\"\r\nContent-Type: application/octet-stream\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(&wacz);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let post_url = format!("{base}/api/archives/upload");
    let ct = format!("multipart/form-data; boundary={boundary}");
    let job: serde_json::Value = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&post_url)
            .header("content-type", &ct)
            .send(&body[..])
            .unwrap();
        assert_eq!(res.status().as_u16(), 202, "upload should be accepted");
        serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap();
    let job_id = job["job"].as_u64().expect("response carries a job id");

    let (status, events) = get(format!("{base}/api/archives/{job_id}/events")).await;
    assert_eq!(status, 200);
    assert!(
        events.contains("event: done"),
        "upload job should complete; got:\n{events}"
    );
    assert!(
        !events.contains("event: error"),
        "upload job should not error; got:\n{events}"
    );

    // Indexed under the given collection, and searchable after the reload.
    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    assert_eq!(manifest.waczs.len(), 1, "one crawl indexed from the upload");
    assert_eq!(manifest.waczs[0].collection, "uploaded");
    let (_, search) = get(format!("{base}/api/search?q=example")).await;
    let after: serde_json::Value = serde_json::from_str(&search).unwrap();
    assert!(
        after["total"].as_u64().unwrap() > 0,
        "reloaded searcher finds the uploaded crawl"
    );

    server.abort();
}

#[tokio::test]
async fn manage_create_collection_via_form_then_it_appears() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ManageConfig::local(),
    )
    .await;

    // Submit the create-collection form (application/x-www-form-urlencoded).
    let post_url = format!("{base}/api/collections");
    let (status, page) = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&post_url)
            .header("content-type", "application/x-www-form-urlencoded")
            .send("name=Demo+Collection&description=A+demo&subjects=alpha,+beta")
            .unwrap();
        // POST-redirect-GET: ureq follows the 303 to the new collection page.
        let status = res.status().as_u16();
        (status, res.body_mut().read_to_string().unwrap())
    })
    .await
    .unwrap();
    assert_eq!(status, 200, "create should redirect to the collection page");
    assert!(page.contains("Demo Collection"), "collection page shows it");
    assert!(
        page.contains("Edit collection"),
        "collection page has the edit affordance under --manage"
    );

    // It's persisted in the manifest with the finding-aid fields...
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let c = manifest
        .collections
        .iter()
        .find(|c| c.name == "Demo Collection")
        .expect("collection persisted");
    assert_eq!(c.description.as_deref(), Some("A demo"));
    assert_eq!(c.subjects, vec!["alpha".to_string(), "beta".to_string()]);

    // ...and it shows on the homepage.
    let (_, home) = get(format!("{base}/")).await;
    assert!(home.contains("Demo Collection"), "homepage lists it");
    server.abort();

    // The edit affordance is gated: a read-only server on the same home does not
    // render it on the collection page.
    let (ro_base, ro_server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ManageConfig::off(),
    )
    .await;
    let (status, ro_page) = get(format!("{ro_base}/collection/demo-collection")).await;
    assert_eq!(status, 200);
    assert!(
        ro_page.contains("Demo Collection"),
        "read-only page still renders"
    );
    assert!(
        !ro_page.contains("Edit collection"),
        "no edit affordance in read-only mode"
    );
    ro_server.abort();
}

#[tokio::test]
async fn manage_page_gated_on_management_mode() {
    // Present under --manage.
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ManageConfig::local(),
    )
    .await;
    // The accession desk renders under --manage.
    let (status, body) = get(format!("{base}/manage/add")).await;
    assert_eq!(status, 200);
    assert!(body.contains("Add crawls"), "accession desk renders");
    // Empty homepage shows the management CTA, not the CLI hint.
    let (_, home) = get(format!("{base}/")).await;
    assert!(home.contains("Add your first archive"), "empty-state CTA");
    server.abort();

    // Absent in the default read-only server.
    let tmp2 = tempfile::TempDir::new().unwrap();
    let (base2, server2) = serve(
        tmp2.path().to_path_buf(),
        indice_lib::server::ManageConfig::off(),
    )
    .await;
    let (status2, _) = get(format!("{base2}/manage/add")).await;
    assert_eq!(status2, 404, "no management routes in read-only mode");
    let (_, home2) = get(format!("{base2}/")).await;
    assert!(
        home2.contains("indice index"),
        "read-only empty homepage keeps the CLI hint"
    );
    assert!(!home2.contains("Add your first archive"));
    server2.abort();
}

/// GET with extra request headers; returns `(status, body)`.
async fn get_with_headers(
    url: String,
    headers: Vec<(&'static str, &'static str)>,
) -> (u16, String) {
    tokio::task::spawn_blocking(move || {
        let mut req = agent().get(&url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let mut res = req.call().unwrap();
        (
            res.status().as_u16(),
            res.body_mut().read_to_string().unwrap(),
        )
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn forward_auth_gates_management_routes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = indice_lib::server::ManageConfig::forward_auth("x-forwarded-email", "s3cret");
    let (base, server) = serve(tmp.path().to_path_buf(), cfg).await;
    let manage = format!("{base}/manage/add");

    // No proxy headers at all -> 403.
    let (status, _) = get(manage.clone()).await;
    assert_eq!(status, 403, "management requires proxy auth");

    // Wrong secret -> 403.
    let (status, _) = get_with_headers(
        manage.clone(),
        vec![
            ("x-indice-auth-secret", "wrong"),
            ("x-forwarded-email", "alice@x.edu"),
        ],
    )
    .await;
    assert_eq!(status, 403, "wrong secret is rejected");

    // Correct secret but no identity -> 403 (a forged/absent identity can't pass).
    let (status, _) =
        get_with_headers(manage.clone(), vec![("x-indice-auth-secret", "s3cret")]).await;
    assert_eq!(status, 403, "secret without identity is rejected");

    // Correct secret + identity -> 200, and the page shows who's signed in.
    let (status, body) = get_with_headers(
        manage.clone(),
        vec![
            ("x-indice-auth-secret", "s3cret"),
            ("x-forwarded-email", "alice@x.edu"),
        ],
    )
    .await;
    assert_eq!(status, 200, "valid proxy auth is allowed");
    assert!(body.contains("alice@x.edu"), "shows the signed-in user");

    // A write route is gated the same way.
    let post_url = format!("{base}/api/collections");
    let unauthed_write = tokio::task::spawn_blocking(move || {
        agent()
            .post(&post_url)
            .header("content-type", "application/x-www-form-urlencoded")
            .send("name=Nope")
            .unwrap()
            .status()
            .as_u16()
    })
    .await
    .unwrap();
    assert_eq!(unauthed_write, 403, "unauthenticated write is rejected");

    // The public read-only site is not gated.
    let (status, _) = get(format!("{base}/")).await;
    assert_eq!(status, 200, "homepage stays public");

    server.abort();
}

#[tokio::test]
async fn read_only_server_has_no_add_archive_route() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ManageConfig::off(),
    )
    .await;

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

#[tokio::test]
async fn browsertrix_import_reports_unconfigured_without_creds() {
    // Management on, but the test server injects no Browsertrix provider (no
    // creds) — the browse/import endpoints should say so clearly, not 500.
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ManageConfig::local(),
    )
    .await;

    let (status, body) = get(format!("{base}/api/browsertrix/orgs")).await;
    assert_eq!(status, 503, "unconfigured Browsertrix is a 503");
    assert!(
        body.contains("not configured"),
        "clear unconfigured message; got: {body}"
    );

    server.abort();
}

#[tokio::test]
async fn archiveit_import_reports_unconfigured_without_creds() {
    // Management on, but no Archive-It provider injected (no creds) — the
    // browse/import endpoints should say so clearly, not 500.
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ManageConfig::local(),
    )
    .await;

    let (status, body) = get(format!("{base}/api/archiveit/collections")).await;
    assert_eq!(status, 503, "unconfigured Archive-It is a 503");
    assert!(
        body.contains("not configured"),
        "clear unconfigured message; got: {body}"
    );

    server.abort();
}

#[tokio::test]
async fn archiveit_routes_absent_in_read_only_mode() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ManageConfig::off(),
    )
    .await;
    let (status, _) = get(format!("{base}/api/archiveit/collections")).await;
    assert_eq!(status, 404, "no Archive-It routes without --manage");
    server.abort();
}

#[tokio::test]
async fn browsertrix_routes_absent_in_read_only_mode() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ManageConfig::off(),
    )
    .await;
    let (status, _) = get(format!("{base}/api/browsertrix/orgs")).await;
    assert_eq!(status, 404, "no Browsertrix routes without --manage");
    server.abort();
}

#[tokio::test]
async fn read_only_server_has_no_collection_or_upload_routes() {
    // Every write route lives in one `if manage.enabled` block, so the read-only
    // server must expose none of them. `read_only_server_has_no_add_archive_route`
    // covers POST /api/archives; this covers the rest (collections, upload, and
    // the management pages) so moving one out of the gate can't slip through.
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ManageConfig::off(),
    )
    .await;

    // GET management pages are absent.
    for path in [
        "/manage/add",
        "/manage/collections/new",
        "/manage/edit/anything",
    ] {
        let (status, _) = get(format!("{base}{path}")).await;
        assert_eq!(status, 404, "GET {path} must be absent in read-only mode");
    }

    // POST /api/collections (create/edit a finding aid) is absent.
    let coll_url = format!("{base}/api/collections");
    let status = tokio::task::spawn_blocking(move || {
        agent()
            .post(&coll_url)
            .header("content-type", "application/x-www-form-urlencoded")
            .send("name=x")
            .unwrap()
            .status()
            .as_u16()
    })
    .await
    .unwrap();
    assert_eq!(
        status, 404,
        "POST /api/collections must be absent in read-only mode"
    );

    // POST /api/archives/upload is absent.
    let upload_url = format!("{base}/api/archives/upload");
    let status = tokio::task::spawn_blocking(move || {
        agent()
            .post(&upload_url)
            .send("x")
            .unwrap()
            .status()
            .as_u16()
    })
    .await
    .unwrap();
    assert_eq!(
        status, 404,
        "POST /api/archives/upload must be absent in read-only mode"
    );

    // POST delete endpoints are absent.
    for path in ["/api/crawls/x/delete", "/api/collections/x/delete"] {
        let url = format!("{base}{path}");
        let status = tokio::task::spawn_blocking(move || {
            agent().post(&url).send("").unwrap().status().as_u16()
        })
        .await
        .unwrap();
        assert_eq!(status, 404, "POST {path} must be absent in read-only mode");
    }

    server.abort();
}

#[tokio::test]
async fn manage_delete_crawl_removes_it_from_index_and_disk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let (base, server) = serve(home.clone(), indice_lib::server::ManageConfig::local()).await;

    // Add a crawl (POST + drain its SSE to completion).
    let post_url = format!("{base}/api/archives");
    let path = fixture("simple.wacz").to_string_lossy().to_string();
    let body = serde_json::json!({ "path": path, "collection": "test" }).to_string();
    let job: serde_json::Value = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&post_url)
            .header("content-type", "application/json")
            .send(body)
            .unwrap();
        serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap();
    let job_id = job["job"].as_u64().unwrap();
    let (_, events) = get(format!("{base}/api/archives/{job_id}/events")).await;
    assert!(
        events.contains("event: done"),
        "add should complete:\n{events}"
    );

    // Grab the crawl id and confirm search finds it.
    let id = {
        let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
        assert_eq!(manifest.waczs.len(), 1);
        manifest.waczs[0].id.clone()
    };
    let (_, body) = get(format!("{base}/api/search?q=example")).await;
    assert!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["total"]
            .as_u64()
            .unwrap()
            > 0
    );

    // Delete via the management endpoint (follows the redirect to a 2xx page).
    let del_url = format!("{base}/api/crawls/{id}/delete");
    let status = tokio::task::spawn_blocking(move || {
        agent().post(&del_url).send("").unwrap().status().as_u16()
    })
    .await
    .unwrap();
    assert!(
        status < 400,
        "delete should redirect to a page, got {status}"
    );

    // Gone from the manifest and from the (reloaded) searcher.
    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    assert!(manifest.waczs.is_empty(), "crawl removed from the manifest");
    let (_, body) = get(format!("{base}/api/search?q=example")).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["total"]
            .as_u64()
            .unwrap(),
        0,
        "reloaded searcher no longer finds the deleted crawl"
    );

    server.abort();
}
