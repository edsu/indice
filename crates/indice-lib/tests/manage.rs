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
    // All four import-source tabs, including the Archive-It browse wizard.
    for needle in [
        r#"data-src="bx""#,
        r#"data-src="ait""#,
        "ait-collection",
        r#"src="/assets/manage.js""#,
    ] {
        assert!(body.contains(needle), "accession desk wires up: {needle}");
    }
    // The browse wizards are driven by that script (they used to be inline JS,
    // so this assertion used to read off the page itself). Check it's actually
    // served and still calls the import APIs.
    let (status, js) = get(format!("{base}/assets/manage.js")).await;
    assert_eq!(status, 200, "accession-desk script is served");
    for needle in ["/api/archiveit/collections", "/api/browsertrix/orgs"] {
        assert!(js.contains(needle), "accession-desk script calls: {needle}");
    }
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

/// End-to-end proof that an annotation body cannot carry executable HTML to the
/// browser.
///
/// `annotations.js` does `body.innerHTML = a.note_html`, which CodeQL flags as
/// `js/xss` (alert #56) because the JS analysis sees `fetch -> json -> innerHTML`
/// and cannot see that the sanitizing happens server-side, in Rust. That's a
/// cross-language boundary no static analyzer here can cross — so the invariant
/// has to be pinned by a test instead of inferred.
///
/// Annotations are authored by one user and rendered to others, so this is a
/// stored-XSS shape whose only defence is `markdown::render`. This exercises the
/// real HTTP path rather than that function in isolation.
#[tokio::test]
async fn annotation_note_html_cannot_carry_executable_markup() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    // create_annotation requires the collection to exist in the manifest.
    let h = home.clone();
    tokio::task::spawn_blocking(move || {
        indice_lib::index::index_path(&fixture("simple.wacz"), &h, None, "test").unwrap();
    })
    .await
    .unwrap();
    let (base, server) = serve(home, indice_lib::server::ManageConfig::local()).await;

    let hostile = concat!(
        "<script>alert('xss')</script>\n\n",
        "<img src=x onerror=alert('xss')>\n\n",
        "<svg/onload=alert('xss')>\n\n",
        "<iframe src=\"javascript:alert('xss')\"></iframe>\n\n",
        "[click me](javascript:alert('xss'))\n\n",
        "[data uri](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)\n\n",
        "<a href=\"javascript:alert('xss')\">link</a>\n\n",
        "<div onmouseover=\"alert('xss')\">hover</div>\n\n",
        "harmless **bold** text"
    );

    let post_url = format!("{base}/api/annotations");
    let body = serde_json::json!({
        "collection": "test",
        "url": "https://example.org/p",
        "timestamp": "20260101000000",
        "note": hostile,
    })
    .to_string();
    let created: serde_json::Value = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&post_url)
            .header("content-type", "application/json")
            .send(body)
            .unwrap();
        assert_eq!(res.status().as_u16(), 201, "annotation should be created");
        serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap();

    // Check the create response AND the read-back, since the panel renders both.
    let (status, listed) = get(format!("{base}/api/annotations?collection=test")).await;
    assert_eq!(status, 200);
    let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
    let read_back = listed["annotations"][0]["note_html"].as_str().unwrap();

    for (label, html) in [
        ("create response", created["note_html"].as_str().unwrap()),
        ("read-back", read_back),
    ] {
        // The property that matters is NOT "the string 'javascript:' is absent"
        // — that appears harmlessly inside escaped text like
        // `&lt;a href="javascript:…"&gt;`, which renders as visible characters.
        // It's that user input never becomes an *element*: the only tags in the
        // output are ones markdown::render itself emits.
        const ALLOWED: &[&str] = &[
            "p",
            "strong",
            "em",
            "code",
            "pre",
            "ul",
            "ol",
            "li",
            "blockquote",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "a",
            "hr",
            "br",
        ];
        let bytes = html.as_bytes();
        let mut found = Vec::new();
        for (i, _) in html.match_indices('<') {
            let rest = &bytes[i + 1..];
            let rest = if rest.first() == Some(&b'/') {
                &rest[1..]
            } else {
                rest
            };
            if !rest.first().is_some_and(|c| c.is_ascii_alphabetic()) {
                continue; // not a tag opener
            }
            let name: String = rest
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric())
                .map(|c| (*c as char).to_ascii_lowercase())
                .collect();
            if !ALLOWED.contains(&name.as_str()) {
                found.push(name);
            }
        }
        assert!(
            found.is_empty(),
            "{label} contains element(s) markdown::render never emits: {found:?}\n{html}"
        );
        // Given the check above, the only tags present are ones render() emits,
        // and the only attribute it emits is href on <a>. So the residual risk
        // is a dangerous scheme on a *real* anchor — note `<a` matches only live
        // markup, since an escaped one reads `&lt;a`.
        let lower = html.to_lowercase();
        for (i, _) in lower.match_indices("<a") {
            let tag = &lower[i..lower[i..].find('>').map(|e| i + e).unwrap_or(lower.len())];
            for scheme in ["javascript:", "data:", "vbscript:"] {
                assert!(
                    !tag.contains(scheme),
                    "{label} has a live anchor with a {scheme:?} URL: {tag}"
                );
            }
        }
        // ...and it isn't simply empty: the harmless content still renders.
        assert!(
            html.contains("harmless") && html.contains("<strong>bold</strong>"),
            "{label} should still render the safe Markdown; got: {html}"
        );
    }

    server.abort();
}

// ── Cross-site request forgery ──────────────────────────────────────────────

/// POST a form body with extra request headers; returns `(status, body)`.
/// `redirects(0)` keeps a successful POST-redirect-GET from following through to
/// the page, so the status we assert on is the handler's own.
async fn post_form_with_headers(
    url: String,
    form: &'static str,
    headers: Vec<(&'static str, String)>,
) -> (u16, String) {
    tokio::task::spawn_blocking(move || {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .new_agent();
        let mut req = agent
            .post(&url)
            .header("content-type", "application/x-www-form-urlencoded");
        for (k, v) in &headers {
            req = req.header(*k, v);
        }
        let mut res = req.send(form).unwrap();
        (
            res.status().as_u16(),
            res.body_mut().read_to_string().unwrap_or_default(),
        )
    })
    .await
    .unwrap()
}

/// The heart of the CSRF fix: a *local* `--manage` instance has no auth proxy
/// and therefore no forward-auth middleware, so before the same-origin guard the
/// write routes ran completely unprotected. A loopback bind is not a boundary a
/// browser honors — any page the operator visited could POST a form here.
#[tokio::test]
async fn local_manage_mode_is_csrf_protected_too() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let (base, server) = serve(home.clone(), indice_lib::server::ManageConfig::local()).await;

    // A same-origin POST still works: create a collection the normal way.
    let (status, _) = post_form_with_headers(
        format!("{base}/api/collections"),
        "name=Keepsakes",
        vec![("origin", base.clone())],
    )
    .await;
    assert_eq!(status, 303, "a same-origin form post is untouched");
    let dir = home.join("collections").join("keepsakes");
    assert!(dir.is_dir(), "collection was created at {dir:?}");

    // Now the attack: a form on another site aimed at the loopback server.
    let (status, body) = post_form_with_headers(
        format!("{base}/api/collections/keepsakes/delete"),
        "with_crawls=on",
        vec![("origin", "https://evil.example".to_string())],
    )
    .await;
    assert_eq!(status, 403, "cross-site delete must be refused");
    assert!(body.contains("cross-site request blocked"), "{body}");
    assert!(
        dir.is_dir(),
        "the collection must survive a cross-site delete"
    );

    server.abort();
}

/// Every management write reachable without a CORS preflight (`Form` and
/// `Multipart` bodies are "simple" content types) must refuse a foreign Origin.
/// The `Json` routes are preflighted and so already unreachable cross-site, but
/// they are covered here too so the guard can't regress to a partial rollout.
#[tokio::test]
async fn cross_site_post_is_rejected_on_every_management_write() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let (base, server) = serve(home.clone(), indice_lib::server::ManageConfig::local()).await;

    for (path, form) in [
        ("/api/collections", "name=Sneaky"),
        ("/api/collections/anything/delete", "with_crawls=on"),
        ("/api/crawls/anything/delete", ""),
        ("/api/archives/upload", ""),
    ] {
        let (status, body) = post_form_with_headers(
            format!("{base}{path}"),
            form,
            vec![("origin", "https://evil.example".to_string())],
        )
        .await;
        assert_eq!(status, 403, "{path} should refuse a cross-site POST");
        assert!(
            body.contains("cross-site request blocked"),
            "{path}: {body}"
        );
    }

    // Nothing was created by the refused create-collection attempt.
    assert!(
        !home.join("collections").join("sneaky").exists(),
        "a refused request must not have run the handler"
    );

    server.abort();
}

/// `Sec-Fetch-Site` is the fallback when a request carries no `Origin`.
#[tokio::test]
async fn sec_fetch_site_cross_site_is_rejected_without_origin() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ManageConfig::local(),
    )
    .await;

    let (status, _) = post_form_with_headers(
        format!("{base}/api/collections"),
        "name=Sneaky",
        vec![("sec-fetch-site", "cross-site".to_string())],
    )
    .await;
    assert_eq!(status, 403);

    // ...while a request with neither header (curl, our own tests, a health
    // checker) is allowed: no browser, so no ambient credentials to ride.
    let (status, _) =
        post_form_with_headers(format!("{base}/api/collections"), "name=Fine", vec![]).await;
    assert_eq!(status, 303);

    server.abort();
}

/// The guard is outermost, so a cross-site request is refused *before*
/// forward-auth looks at credentials — the attacker learns nothing about whether
/// their forged identity would have been accepted.
#[tokio::test]
async fn csrf_guard_runs_before_forward_auth() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = indice_lib::server::ManageConfig::forward_auth("x-forwarded-email", "s3cret");
    let (base, server) = serve(tmp.path().to_path_buf(), cfg).await;

    // Fully valid proxy credentials, but a foreign Origin: still refused, and
    // with the CSRF message rather than the forward-auth one.
    let (status, body) = post_form_with_headers(
        format!("{base}/api/collections"),
        "name=Sneaky",
        vec![
            ("origin", "https://evil.example".to_string()),
            ("x-indice-auth-secret", "s3cret".to_string()),
            ("x-forwarded-email", "alice@x.edu".to_string()),
        ],
    )
    .await;
    assert_eq!(status, 403);
    assert!(
        body.contains("cross-site request blocked"),
        "CSRF is judged first: {body}"
    );

    server.abort();
}

/// Annotations are world-readable and the JSONL store is meant to be committed,
/// so a login address that reaches a public surface is published for good. Write
/// a note as `alice@x.edu` through the real forward-auth path, then read every
/// public surface *anonymously* and assert the address never appears.
#[tokio::test]
async fn public_annotation_api_never_exposes_a_login_address() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let cfg = indice_lib::server::ManageConfig::forward_auth("x-forwarded-email", "s3cret");
    let (base, server) = serve(home.clone(), cfg).await;

    // A collection to hang the note on (annotations require a known collection).
    let (status, _) = post_form_with_headers(
        format!("{base}/api/collections"),
        "name=Notes",
        vec![
            ("x-indice-auth-secret", "s3cret".to_string()),
            ("x-forwarded-email", "alice@x.edu".to_string()),
        ],
    )
    .await;
    assert_eq!(status, 303);

    // Create a note as an SSO-authenticated user whose identity is an email.
    let url = format!("{base}/api/annotations");
    let body = serde_json::json!({
        "collection": "notes",
        "url": "https://example.org/",
        "timestamp": "20260101000000",
        "note": "a public note",
    })
    .to_string();
    let status = tokio::task::spawn_blocking(move || {
        agent()
            .post(&url)
            .header("content-type", "application/json")
            .header("x-indice-auth-secret", "s3cret")
            .header("x-forwarded-email", "alice@x.edu")
            .send(body)
            .unwrap()
            .status()
            .as_u16()
    })
    .await
    .unwrap();
    assert!((200..300).contains(&status), "note created (got {status})");

    // Now read as an anonymous visitor. None of these may carry the address.
    for path in [
        "/api/annotations?collection=notes",
        "/collection/notes/annotations",
    ] {
        let (status, body) = get(format!("{base}{path}")).await;
        assert_eq!(status, 200, "{path}");
        assert!(
            body.contains("alice"),
            "{path} should still attribute the note: {body}"
        );
        assert!(
            !body.contains("alice@x.edu") && !body.contains("x.edu"),
            "{path} leaked a login address: {body}"
        );
    }

    server.abort();
}
