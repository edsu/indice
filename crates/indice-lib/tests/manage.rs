//! Integration tests for the write surface. These exercise the real HTTP path:
//! POST a job, stream its Server-Sent-Events progress to completion, and confirm
//! the read-only searcher hot-reloads so the newly-indexed crawl becomes
//! searchable without a restart. Also that an anonymous caller is refused, and
//! that local access means local.

use std::path::Path;
use std::sync::{Arc, Mutex};

/// Local trust, as a workstation has it. The helper binds `127.0.0.1:0`, so the
/// address handed to `Access::local` matches what the listener will get.
fn local_access() -> indice_lib::server::ServerConfig {
    indice_lib::server::ServerConfig::new(
        indice_lib::server::Access::local("127.0.0.1:0".parse().unwrap()).unwrap(),
    )
}

/// A home carrying a roster, for the tests that run as a server.
///
/// A server refuses to start without one, so that everybody the identity
/// provider admits is not silently an admin. `alice@x.edu` is listed as admin
/// because that is who these tests sign in as, and she was an admin under the
/// old permissive default too, which keeps every assertion about what she may
/// do unchanged.
fn home_with_roster() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("users.yaml"),
        "users:\n  - id: alice@x.edu\n    role: admin\n",
    )
    .unwrap();
    tmp
}

/// Upload a fixture WACZ and return the job JSON.
///
/// These tests want a crawl in the index and do not care how it got there.
/// They used to POST the fixture's path to `/api/archives`; a path on the
/// server's own disk is no longer accepted over HTTP in either shape (see
/// `a_crawl_is_added_by_url_or_upload_never_by_a_server_side_path`), so they
/// upload the bytes instead, which is the route a curator actually has.
async fn upload_fixture(base: &str, wacz: &str, collection: &str) -> serde_json::Value {
    let bytes = std::fs::read(fixture(wacz)).unwrap();
    let boundary = "----indiceManageUpload";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"collection\"\r\n\r\n{collection}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{wacz}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let url = format!("{base}/api/archives/upload");
    tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&url)
            .header(
                "content-type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&body[..])
            .unwrap();
        assert_eq!(res.status().as_u16(), 202, "upload should be accepted");
        serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap()
}

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
    config: indice_lib::server::ServerConfig,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let replay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = config;
    // A server will not start without its public URL; this one's is the
    // address it is about to listen on.
    if config.site_url().is_none() {
        config
            .set_site_url(&format!("http://127.0.0.1:{port}"))
            .unwrap();
    }
    config.replay_origin = Some(indice_lib::server::ReplayOrigin::for_listener(
        replay.local_addr().unwrap(),
    ));
    let handle = tokio::spawn(async move {
        indice_lib::server::serve_on_listeners(
            listener,
            replay,
            &home,
            None,
            config,
            indice_lib::server::Providers::default(),
        )
        .await
        .unwrap();
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

#[tokio::test]
async fn an_ingest_reloads_the_searcher_and_its_progress_is_consumed_once() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let (base, server) = serve(home.clone(), local_access()).await;

    // Precondition: empty index, so search finds nothing.
    let (status, body) = get(format!("{base}/api/search?q=example")).await;
    assert_eq!(status, 200);
    let before: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(before["total"], 0, "index should start empty");

    // Accession a fixture WACZ.
    let job = upload_fixture(&base, "simple.wacz", "test").await;
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
    let (base, server) = serve(home.clone(), local_access()).await;

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
    let (base, server) = serve(tmp.path().to_path_buf(), local_access()).await;

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
        "collection page has the edit affordance for the operator"
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

    // The edit affordance is gated: an anonymous visitor to the same home does
    // not get it on the collection page. Serving that home as a server needs a
    // roster; empty is right, since this half is about the anonymous view.
    std::fs::write(tmp.path().join("users.yaml"), "users: []\n").unwrap();
    let (ro_base, ro_server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
            "x-forwarded-email",
            "secret-the-test-never-sends",
        )),
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
    // Present for the operator.
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(tmp.path().to_path_buf(), local_access()).await;
    // The accession desk renders for the operator.
    let (status, body) = get(format!("{base}/manage/add")).await;
    assert_eq!(status, 200);
    assert!(body.contains("Add crawls"), "accession desk renders");
    // Upload and URL. This server has no Browsertrix or Archive-It
    // credentials, so neither import tab is offered; with them, they are
    // (`a_workstation_operator_is_offered_the_import_tabs`).
    for needle in [
        r#"data-src="upload""#,
        r#"data-src="url""#,
        r#"src="/assets/manage.js""#,
    ] {
        assert!(body.contains(needle), "accession desk wires up: {needle}");
    }
    for absent in [r#"data-src="bx""#, r#"data-src="ait""#] {
        assert!(!body.contains(absent), "no credentials, so no {absent}");
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
    assert!(
        home.contains("Add a crawl to start your first collection"),
        "empty-state CTA"
    );
    server.abort();

    // Refused for an anonymous visitor. The route is mounted — who may use it is
    // an authorization question — so this is a 403, not a 404.
    let tmp2 = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp2.path().join("users.yaml"), "users: []\n").unwrap();
    let (base2, server2) = serve(
        tmp2.path().to_path_buf(),
        indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
            "x-forwarded-email",
            "secret-the-test-never-sends",
        )),
    )
    .await;
    let (status2, _) = get(format!("{base2}/manage/add")).await;
    assert_eq!(status2, 403, "management routes refuse an anonymous caller");
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
    let tmp = home_with_roster();
    let cfg = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "x-forwarded-email",
        "s3cret",
    ));
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
    // The appbar shows the *display* name, not the login identity. These pages
    // get screenshotted for the docs, so a full address here would end up
    // published; and with a users.yaml the operator can set a real name.
    assert!(body.contains("alice"), "shows who is signed in");
    assert!(
        !body.contains("alice@x.edu"),
        "but never the login address: {body}"
    );

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
async fn browsertrix_import_reports_unconfigured_without_creds() {
    // Management on, but the test server injects no Browsertrix provider (no
    // creds) — the browse/import endpoints should say so clearly, not 500.
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(tmp.path().to_path_buf(), local_access()).await;

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
    let (base, server) = serve(tmp.path().to_path_buf(), local_access()).await;

    let (status, body) = get(format!("{base}/api/archiveit/collections")).await;
    assert_eq!(status, 503, "unconfigured Archive-It is a 503");
    assert!(
        body.contains("not configured"),
        "clear unconfigured message; got: {body}"
    );

    server.abort();
}

#[tokio::test]
async fn manage_delete_crawl_removes_it_from_index_and_disk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let (base, server) = serve(home.clone(), local_access()).await;

    // Add a crawl (upload + drain its SSE to completion).
    let job = upload_fixture(&base, "simple.wacz", "test").await;
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
    let (base, server) = serve(home, local_access()).await;

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

    // The notes list links each note's page with the note named, so the page
    // opens on its passage.
    let id = created["id"].as_str().unwrap();
    let (_, index) = get(format!("{base}/collection/test/annotations")).await;
    let named = format!("note={}", id.replace(':', "%3A"));
    assert!(
        index.contains(&named),
        "the notes list should link to {named}: {index}"
    );

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

/// The heart of the CSRF fix: a *local* instance has no auth proxy
/// and therefore no forward-auth middleware, so before the same-origin guard the
/// write routes ran completely unprotected. A loopback bind is not a boundary a
/// browser honors — any page the operator visited could POST a form here.
#[tokio::test]
async fn local_manage_mode_is_csrf_protected_too() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let (base, server) = serve(home.clone(), local_access()).await;

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
    let (base, server) = serve(home.clone(), local_access()).await;

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
    let (base, server) = serve(tmp.path().to_path_buf(), local_access()).await;

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
    let tmp = home_with_roster();
    let cfg = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "x-forwarded-email",
        "s3cret",
    ));
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
    let tmp = home_with_roster();
    let home = tmp.path().to_path_buf();
    let cfg = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "x-forwarded-email",
        "s3cret",
    ));
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

    // On disk the two values are now distinct: a canonical id to compare
    // against, and a display name that is safe to publish and to commit.
    let stored = std::fs::read_to_string(home.join("collections/notes/annotations.jsonl")).unwrap();
    let record: serde_json::Value = serde_json::from_str(stored.trim()).unwrap();
    assert_eq!(record["creator"]["id"], "mailto:alice@x.edu");
    assert_eq!(record["creator"]["name"], "alice");

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

/// What [`get_accepting`] returns.
struct Fetched {
    status: u16,
    content_type: String,
    vary: String,
    link: String,
    cors: String,
    body: String,
}

/// GET with an `Accept` header, or none for `""`.
async fn get_accepting(url: String, accept: &'static str) -> Fetched {
    tokio::task::spawn_blocking(move || {
        let mut req = agent().get(&url);
        if !accept.is_empty() {
            req = req.header("accept", accept);
        }
        let mut res = req.call().unwrap();
        let header = |name: &str| {
            res.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string()
        };
        let (content_type, vary, link) = (header("content-type"), header("vary"), header("link"));
        let cors = header("access-control-allow-origin");
        Fetched {
            status: res.status().as_u16(),
            content_type,
            vary,
            link,
            cors,
            body: res.body_mut().read_to_string().unwrap(),
        }
    })
    .await
    .unwrap()
}

/// A workstation's notes are not readable by other sites: with `ACAO: *`, any
/// page the operator visits could read one from `127.0.0.1` by its URL. A
/// server's are public, as `a_note_has_a_url_of_its_own` pins.
#[tokio::test]
async fn a_workstation_lets_no_other_site_read_its_notes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(tmp.path().to_path_buf(), local_access()).await;
    let (status, _) =
        post_form_with_headers(format!("{base}/api/collections"), "name=Notes", vec![]).await;
    assert_eq!(status, 303);
    let url = format!("{base}/api/annotations");
    let body = serde_json::json!({
        "collection": "notes",
        "url": "https://example.org/",
        "timestamp": "20260101000000",
        "selector": { "exact": "Example Domain" },
        "note": "private",
    })
    .to_string();
    let created: serde_json::Value = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&url)
            .header("content-type", "application/json")
            .send(body)
            .unwrap();
        serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap();
    let href = created["href"].as_str().unwrap();
    for accept in ["application/ld+json", ""] {
        let r = get_accepting(format!("{base}{href}"), accept).await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.cors, "", "{accept:?}: no other site may read a note");
    }
    server.abort();
}

/// A note has a URL of its own, which its JSON-LD names as its `id`: asked for
/// JSON-LD, it answers with the note as a W3C Web Annotation; asked by a
/// browser, with a page carrying the same JSON-LD and a link into the
/// archive. Both are public, so neither may carry the author's login address,
/// and a note's text can't close the page's `<script>`.
#[tokio::test]
async fn a_note_has_a_url_of_its_own() {
    let tmp = home_with_roster();
    let cfg = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "x-forwarded-email",
        "s3cret",
    ));
    let (base, server) = serve(tmp.path().to_path_buf(), cfg).await;
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

    let text = "Worth reading. </script><script>alert(1)</script>";
    let url = format!("{base}/api/annotations");
    let body = serde_json::json!({
        "collection": "notes",
        "url": "https://example.org/",
        "timestamp": "20260101000000",
        "selector": { "exact": "Example Domain" },
        "note": text,
    })
    .to_string();
    let created: serde_json::Value = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&url)
            .header("content-type", "application/json")
            .header("x-indice-auth-secret", "s3cret")
            .header("x-forwarded-email", "alice@x.edu")
            .send(body)
            .unwrap();
        serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap();
    let id = created["id"].as_str().unwrap();
    let key = id.strip_prefix("urn:indice:annotation:").unwrap();
    let href = created["href"].as_str().unwrap();
    assert_eq!(href, format!("/collection/notes/annotations/{key}"));

    // As JSON-LD, by content negotiation.
    let r = get_accepting(format!("{base}{href}"), "application/ld+json").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.content_type,
        r#"application/ld+json; profile="http://www.w3.org/ns/anno.jsonld""#
    );
    assert!(r.vary.to_ascii_lowercase().contains("accept"), "{}", r.vary);
    // Public, so any site's annotation client may read it.
    assert_eq!(r.cors, "*");
    assert!(
        !r.body.contains("x.edu"),
        "a login address leaked: {}",
        r.body
    );
    let ld: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(ld["@context"], "http://www.w3.org/ns/anno.jsonld");
    assert_eq!(ld["id"], format!("{base}{href}"));
    assert_eq!(ld["type"], "Annotation");
    assert_eq!(ld["creator"]["name"], "alice");
    assert_eq!(ld["body"]["value"], text);
    assert_eq!(ld["body"]["format"], "text/markdown");
    assert_eq!(ld["target"]["source"], "https://example.org/");
    assert_eq!(ld["target"]["selector"]["type"], "TextQuoteSelector");
    assert_eq!(ld["target"]["selector"]["exact"], "Example Domain");
    assert_eq!(ld["target"]["state"]["type"], "TimeState");
    assert_eq!(ld["target"]["state"]["sourceDate"], "2026-01-01T00:00:00Z");
    assert_eq!(
        ld["target"]["state"]["cached"],
        format!("{base}/collection/notes/20260101000000/https://example.org/")
    );
    // Plain JSON, from a client that doesn't name HTML, gets it too.
    let r = get_accepting(format!("{base}{href}"), "application/json").await;
    assert!(
        r.content_type.starts_with("application/ld+json"),
        "{}",
        r.content_type
    );
    // And at its own URL whatever is asked, with the same id: the note's
    // name is still the URL without the suffix.
    let jsonld = format!("{href}.jsonld");
    for accept in ["", "text/html"] {
        let r = get_accepting(format!("{base}{jsonld}"), accept).await;
        assert_eq!(r.status, 200, "{accept:?}");
        assert!(
            r.content_type.starts_with("application/ld+json"),
            "{accept:?}: {}",
            r.content_type
        );
        assert_eq!(r.cors, "*", "{accept:?}");
        let same: serde_json::Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(same, ld, "{accept:?}");
    }

    // As a page, for a browser, with the same JSON-LD in it.
    let r = get_accepting(
        format!("{base}{href}"),
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    )
    .await;
    let page = r.body;
    assert_eq!(r.status, 200);
    assert!(
        r.content_type.starts_with("text/html"),
        "{}",
        r.content_type
    );
    assert!(r.vary.to_ascii_lowercase().contains("accept"), "{}", r.vary);
    // It says where the JSON-LD is, to a reader and in a header.
    assert!(
        page.contains(&format!(
            r#"href="{jsonld}" rel="alternate" type="application/ld+json""#
        )),
        "{page}"
    );
    assert_eq!(
        r.link,
        format!(r#"<{jsonld}>; rel="alternate"; type="application/ld+json""#)
    );
    assert!(!page.contains("x.edu"), "a login address leaked: {page}");
    assert!(
        !page.contains("<script>alert"),
        "a note closed the script: {page}"
    );
    let embedded = page
        .split(r#"<script type="application/ld+json">"#)
        .nth(1)
        .and_then(|rest| rest.split("</script>").next())
        .unwrap_or_else(|| panic!("no JSON-LD in the page: {page}"));
    let embedded: serde_json::Value = serde_json::from_str(embedded).unwrap();
    assert_eq!(embedded, ld, "the page's JSON-LD is the note's");
    assert!(
        page.contains(&format!(
            "/collection/notes/20260101000000/https://example.org/#note={}",
            id.replace(':', "%3A")
        )),
        "the page links into the archive: {page}"
    );

    // Its crumb goes back to its place on the Annotations page.
    assert!(
        page.contains(&format!(r#"href="/collection/notes/annotations#{key}""#)),
        "{page}"
    );

    // Found from the Annotations page, anchored by its key.
    let (_, index) = get(format!("{base}/collection/notes/annotations")).await;
    assert!(index.contains(&format!(r#"href="{href}""#)), "{index}");
    assert!(index.contains(&format!(r#"id="{key}""#)), "{index}");

    // Nothing else answers.
    for missing in [
        "/collection/notes/annotations/not-a-key".to_string(),
        format!("/collection/notes/annotations/{}", "0".repeat(32)),
        format!("/collection/notes/annotations/{}.jsonld", "0".repeat(32)),
        format!("{href}.jsonld.jsonld"),
        format!("/collection/elsewhere/annotations/{key}"),
    ] {
        let (status, _) = get(format!("{base}{missing}")).await;
        assert_eq!(status, 404, "{missing}");
    }
    server.abort();
}

/// On a workstation a note's id is under `http://localhost:{port}`, the
/// address its browser opens, rather than the listener's `127.0.0.1`.
#[tokio::test]
async fn a_workstations_note_is_named_under_localhost() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(tmp.path().to_path_buf(), local_access()).await;
    let port = base.rsplit(':').next().unwrap().to_string();
    let (status, _) = post_form_with_headers(
        format!("{base}/api/collections"),
        "name=Notes",
        vec![("origin", base.clone())],
    )
    .await;
    assert_eq!(status, 303);
    let url = format!("{base}/api/annotations");
    let origin = base.clone();
    let created: serde_json::Value = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&url)
            .header("content-type", "application/json")
            .header("origin", &origin)
            .send(
                serde_json::json!({
                    "collection": "notes",
                    "url": "https://example.org/",
                    "timestamp": "20260101000000",
                    "note": "a note",
                })
                .to_string(),
            )
            .unwrap();
        serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap();
    let href = created["href"].as_str().unwrap();
    let r = get_accepting(format!("{base}{href}"), "application/ld+json").await;
    let ld: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(ld["id"], format!("http://localhost:{port}{href}"));
    server.abort();
}

/// Saving a finding aid must not erase a concurrent ingest's manifest entry.
///
/// `Manifest::save` rewrites `waczs.json` wholesale whatever changed, so a
/// finding-aid edit is a full read-modify-write of the manifest. Before this
/// test's fix, `create_collection` took no write lock at all, so a curator
/// editing a description while an add-crawls job ran could install a stale
/// manifest and drop the job's crawl — leaving its documents in Tantivy with
/// no manifest entry, which is a crawl whose page 404s and which can no longer
/// be deleted.
///
/// Note on what this test is: the invariant it asserts is deterministic (a
/// crawl that was indexed is still in the manifest), but the *interleaving* is
/// not forced. The save is issued immediately after the job is accepted, while
/// the job runs on a blocking thread, and a 4.7 MB fixture is used so the job
/// is long enough to overlap. So it never fails when the code is right, and it
/// fails often — not always — when it is wrong. Several rounds make that
/// likelier. Forcing the overlap properly needs the library-level lock, which
/// is `rustyweb-durable-writes-f4h5`.
#[tokio::test]
async fn a_finding_aid_save_cannot_erase_a_concurrent_ingest() {
    for round in 0..3 {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        let (base, server) = serve(home.clone(), local_access()).await;

        // The collection the crawl goes into has to exist first, so the save
        // below is an *edit* rather than a create.
        let (status, _) = post_form_with_headers(
            format!("{base}/api/collections"),
            "name=Contended",
            vec![("origin", base.clone())],
        )
        .await;
        assert_eq!(status, 303);

        // Start an ingest of the largest fixture, so the job is still running
        // when the edit lands.
        let job = upload_fixture(&base, "a.wacz", "Contended").await;
        let job_id = job["job"].as_u64().unwrap();

        // ...and edit the finding aid while it runs.
        let (status, _) = post_form_with_headers(
            format!("{base}/api/collections"),
            "name=Contended&description=edited+mid-ingest",
            vec![("origin", base.clone())],
        )
        .await;
        assert_eq!(status, 303);

        // Let the job finish (draining its SSE stream blocks until it ends).
        let (_, events) = get(format!("{base}/api/archives/{job_id}/events")).await;
        assert!(
            events.contains("event: done"),
            "round {round}: ingest should complete; got:\n{events}"
        );

        // The invariant: the crawl is still recorded. Before the fix, the
        // finding-aid save could install a manifest copy read before the crawl
        // existed, silently dropping it.
        let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
        assert_eq!(
            manifest.waczs.len(),
            1,
            "round {round}: the ingested crawl was erased by a concurrent \
             finding-aid save; manifest: {:?}",
            manifest.waczs
        );
        // And the edit landed too, so the fix serializes rather than drops.
        let c = manifest.collection_by_id("contended").expect("collection");
        assert_eq!(c.description.as_deref(), Some("edited mid-ingest"));

        server.abort();
    }
}

/// A request-path write returns 503 rather than blocking when another process
/// holds the index lock.
///
/// Background jobs and the CLI queue; a request must not, because a rebuild can
/// hold the lock for hours and a browser (and the thread serving it) cannot wait
/// that long. The reply names the holder and carries `Retry-After`.
///
/// The lock is taken here from a second thread, which is what another *process*
/// would look like: the re-entrancy count is per thread, so this contends on the
/// real flock rather than being handed a nested guard.
#[tokio::test]
async fn a_write_returns_503_while_the_index_is_locked() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();

    // Index something so there is a crawl to try to delete.
    let archive = home.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let staged = archive.join("simple.wacz");
    std::fs::copy(fixture("simple.wacz"), &staged).unwrap();
    indice_lib::index::index_path(&staged, &home, Some("held"), "held-coll").unwrap();
    let id = indice_lib::collections::Manifest::open(&home.join("index"))
        .unwrap()
        .waczs[0]
        .id
        .clone();

    let (base, server) = serve(home.clone(), local_access()).await;

    // Hold the lock for longer than the handler is willing to wait.
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let home = home.clone();
        std::thread::spawn(move || {
            let path = home.join("index").join(".index.lock");
            let f = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&path)
                .unwrap();
            f.lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv();
        })
    };
    held_rx.recv().unwrap();

    let url = format!("{base}/api/crawls/{id}/delete");
    let origin = base.clone();
    let (status, retry_after) = tokio::task::spawn_blocking(move || {
        let res = agent()
            .post(&url)
            // Same-origin, or the CSRF check refuses it before the lock matters.
            .header("Origin", &origin)
            .send_empty()
            .unwrap();
        let retry = res
            .headers()
            .get("retry-after")
            .map(|v| v.to_str().unwrap().to_string());
        (res.status().as_u16(), retry)
    })
    .await
    .unwrap();

    assert_eq!(status, 503, "a busy index must not block the request");
    // Not a literal: the header is derived from the wait budget, so hard-coding
    // it here would fail with a complaint about Retry-After when someone tunes
    // the budget, which is not what went wrong.
    let retry: u64 = retry_after.expect("Retry-After is set").parse().unwrap();
    assert!(
        retry >= 1,
        "Retry-After must never tell a client to retry immediately; got {retry}"
    );

    // The crawl is still there: a 503 means the write did not happen.
    let after = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    assert_eq!(after.waczs.len(), 1, "the refused delete changed nothing");

    release_tx.send(()).unwrap();
    holder.join().unwrap();
    server.abort();
}

/// Every management route refuses an anonymous caller.
///
/// This replaces five tests that asserted those routes were *absent* without
/// read-only mode. There is no longer a mode in which they are absent: the write
/// surface is always mounted and who may use it is an authorization question,
/// answered by the `Curator`/`Admin` extractors and `users.yaml`.
///
/// 403 rather than 404 is the point. A 404 for an authorization failure is
/// obscurity, and it meant the same request answered differently depending on a
/// startup flag rather than on who was asking.
///
/// What this does *not* guard, despite the obvious reading: a route whose
/// extractor someone drops while refactoring. In proxy mode the forward-auth
/// middleware refuses an anonymous request before any extractor runs, so
/// deleting `Admin` from a handler would leave this test green. The matrix in
/// `authz.rs` covers that, by sending authenticated-but-unprivileged requests.
/// What this one proves is narrower and still worth having: every route exists
/// and refuses a stranger, with no mode in which that answer changes.
#[tokio::test]
async fn anonymous_is_refused_from_every_management_route() {
    let tmp = home_with_roster();
    let (base, server) = serve(
        tmp.path().to_path_buf(),
        indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
            "x-forwarded-email",
            "secret-the-test-never-sends",
        )),
    )
    .await;

    for path in [
        "/manage/add",
        "/manage/collections/new",
        "/manage/edit/anything",
        "/api/archiveit/collections",
        "/api/archiveit/crawls",
        "/api/browsertrix/orgs",
        "/api/browsertrix/collections",
        "/api/browsertrix/items",
        "/api/archives/1/events",
    ] {
        let (status, _) = get(format!("{base}{path}")).await;
        assert_eq!(status, 403, "GET {path} must refuse an anonymous caller");
    }

    for path in [
        "/api/archives",
        "/api/archives/upload",
        "/api/collections",
        "/api/crawls/anything/delete",
        "/api/collections/anything/delete",
        "/api/archiveit/import",
        "/api/browsertrix/import",
        "/api/annotations",
        "/api/annotations/anything",
        "/api/annotations/anything/delete",
    ] {
        let url = format!("{base}{path}");
        let origin = base.clone();
        let status = tokio::task::spawn_blocking(move || {
            agent()
                .post(&url)
                .header("Origin", &origin)
                .header("content-type", "application/x-www-form-urlencoded")
                .send("")
                .unwrap()
                .status()
                .as_u16()
        })
        .await
        .unwrap();
        assert_eq!(status, 403, "POST {path} must refuse an anonymous caller");
    }

    server.abort();
}

/// A workstation serves whoever reaches the port, including through a tunnel,
/// and answers only to this machine's names.
///
/// This pins two decisions, and they look as if they contradict each other, so
/// both are written down here.
///
/// The first is that `Host` is not access control. The workstation shape
/// promises nothing about who reaches the port: a second shell on the same
/// machine, an `ssh -L`, a tunnel that says `Host: localhost`, all of them get
/// the operator's rights. There were two attempts at a guard against tunnelled
/// callers and both were bypassable. The first read the authority from
/// `X-Forwarded-Host` in preference to `Host`, so `X-Forwarded-Host: localhost`
/// walked through. The second read `Host` only, and fell to
/// `curl -H 'Host: 127.0.0.1:8080'` through any Host-preserving proxy, which is
/// what Go's `httputil.ReverseProxy` and therefore `tailscale serve` is. Header
/// inspection cannot authenticate a caller. Sharing an archive is the server
/// shape.
///
/// The second is that a workstation refuses any request, read or write, whose
/// `Host` is not a loopback name. That is not the old guard back: it is aimed
/// at DNS rebinding, where a page the operator visits points its own name at
/// 127.0.0.1 and reads the archive as same-origin. A browser cannot choose
/// `Host`, so for a browser the name is decisive, and a browser is the only
/// thing that rebinds. The cost is that a browser reaching a workstation
/// through `tailscale serve` is refused, which its writes already were (its
/// `Origin` is not loopback).
///
/// So if the first half goes red, the fix is not a better header check. Change
/// the threat model in DESIGN first, and then add something a caller must hold
/// rather than something a caller can say.
#[tokio::test]
async fn a_workstation_serves_a_tunnelled_caller_under_a_loopback_name() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(tmp.path().to_path_buf(), local_access()).await;

    // A tunnel that keeps (or claims) `localhost`, as `ssh -L` does, reads the
    // site and writes to it. No Origin: it is not a browser.
    let (status, _) = get_with_headers(format!("{base}/"), vec![("Host", "localhost:8080")]).await;
    assert_eq!(status, 200, "a loopback name is served whoever sends it");
    let tunnelled = post_headers(
        format!("{base}/api/collections"),
        vec![("Host", "localhost:8080")],
        "name=Tunnelled",
    )
    .await;
    // 200, not 303: the agent follows the post-redirect-get.
    assert_eq!(
        tunnelled, 200,
        "a workstation write is not access-controlled"
    );
    assert!(
        tmp.path().join("collections").join("tunnelled").exists(),
        "the collection should have been created"
    );

    // DNS rebinding: `evil.example` resolving to 127.0.0.1 makes the browser
    // send a self-consistent Host and Origin pair. Refused for a write...
    let rebound = post_headers(
        format!("{base}/api/collections"),
        vec![("Host", "evil.example"), ("Origin", "http://evil.example")],
        "name=Rebound",
    )
    .await;
    assert_eq!(rebound, 403, "a rebound browser must not write");
    assert!(
        !tmp.path().join("collections").join("rebound").exists(),
        "a refused write must not reach the archive"
    );
    // ...and for the reads it was after. A same-origin GET carries no Origin.
    for path in ["/", "/search?q=x", "/api/search?q=x", "/health"] {
        let (status, _) =
            get_with_headers(format!("{base}{path}"), vec![("Host", "evil.example:8080")]).await;
        assert_eq!(status, 403, "a rebound browser must not read {path}");
    }
    // A tunnel that forwards its own name, as `tailscale serve` does, is refused
    // the same way: indice cannot tell it from a rebound browser.
    let (status, body) = get_with_headers(format!("{base}/"), vec![("Host", "box.ts.net")]).await;
    assert_eq!(status, 403);
    assert!(body.contains("run indice as a server"), "{body}");

    server.abort();
}

/// POST a form body with extra headers, returning the status.
async fn post_headers(url: String, headers: Vec<(&'static str, &'static str)>, body: &str) -> u16 {
    let body = body.to_string();
    tokio::task::spawn_blocking(move || {
        let mut req = agent().post(&url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req.header("content-type", "application/x-www-form-urlencoded")
            .send(body)
            .unwrap()
            .status()
            .as_u16()
    })
    .await
    .unwrap()
}

/// A crawl is added by URL or by upload, never by a path on the server's disk.
///
/// `path` on `POST /api/archives` went to `Source::parse`, where an `http(s)`
/// prefix makes it a URL and *anything else is a path on the server's disk*.
/// Over HTTP that is a bad trade in either shape. On a server an approved
/// stranger could make the machine read any `.wacz` it can reach — another
/// tenant's archive, a backup staged on the same volume — and file it into a
/// collection they can read back. On a workstation it was safe and unused: the
/// file is already on the disk, and `indice index` is a shorter route to it
/// than a web form.
///
/// An earlier version of this test asserted the path was refused on a server
/// and accepted on a workstation, behind a flag to opt back in. That bought a
/// capability nobody wanted at the price of a setting whose right value
/// differed by deployment shape. The command line keeps the capability, which
/// is where a mounted archive volume gets accessioned from anyway.
#[tokio::test]
async fn a_crawl_is_added_by_url_or_upload_never_by_a_server_side_path() {
    let tmp = home_with_roster();
    let cfg = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "x-forwarded-email",
        "s3cret",
    ));
    let (base, server) = serve(tmp.path().to_path_buf(), cfg).await;

    let signed_in = || {
        vec![
            ("x-indice-auth-secret", "s3cret".to_string()),
            ("x-forwarded-email", "alice@x.edu".to_string()),
            ("content-type", "application/json".to_string()),
        ]
    };
    let add = |body: serde_json::Value| {
        let url = format!("{base}/api/archives");
        let headers = signed_in();
        async move {
            tokio::task::spawn_blocking(move || {
                let mut req = ureq::Agent::config_builder()
                    .http_status_as_error(false)
                    .build()
                    .new_agent()
                    .post(&url);
                for (k, v) in headers {
                    req = req.header(k, &v);
                }
                let mut res = req.send(body.to_string()).unwrap();
                (
                    res.status().as_u16(),
                    res.body_mut().read_to_string().unwrap(),
                )
            })
            .await
            .unwrap()
        }
    };

    let (status, body) = add(serde_json::json!({
        "path": "/etc/secrets/other-tenant.wacz",
        "collection": "Sneaky",
    }))
    .await;
    assert_eq!(status, 400, "a server-side path must be refused");
    assert!(
        body.contains("indice index"),
        "and should point at the command line, which still takes one: {body}"
    );
    assert!(
        body.contains("URL") && body.contains("upload"),
        "the refusal should say what to do instead, got: {body}"
    );

    // That a URL *is* accepted, and what happens to it, is covered end to end
    // by `a_url_is_fetched_only_from_a_public_address` below. Asserting a 202
    // here would queue a real ingest of whatever host the URL names, which on
    // an offline runner means a detached job retrying DNS for seconds after
    // the test has already passed.

    server.abort();
}

/// On a server, a URL must resolve to a public address; the opt-out restores it.
///
/// This is the half of the SSRF guard that the server actually wires up, and
/// the review found it untested: `public_only_refuses_a_fetch_that_lands_on
/// _this_machine` calls the library function with an explicit policy, so
/// deleting `.fetch_policy(fetch)` from `start_index_job`, or inverting the
/// condition that chooses it, left the whole suite green.
///
/// It doubles as the end-to-end cover for adding by URL at all — job creation,
/// the SSE relay, the searcher reload — which nothing else exercises now that
/// adding by path is gone.
#[tokio::test]
async fn a_url_is_fetched_only_from_a_public_address() {
    // A local server standing in for "an address on the operator's network".
    // 127.0.0.1 is the one such address a test can count on existing, and it
    // is the same family as the cloud metadata case this guards.
    let wacz = std::fs::read(fixture("simple.wacz")).unwrap();
    let origin = axum::Router::new().route(
        "/simple.wacz",
        axum::routing::get(move || {
            let wacz = wacz.clone();
            async move { wacz }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = listener.local_addr().unwrap();
    let origin_task = tokio::spawn(async move {
        axum::serve(listener, origin.into_make_service())
            .await
            .unwrap();
    });
    let wacz_url = format!("http://{origin_addr}/simple.wacz");

    // Returns the job's SSE transcript, which carries the ingest's outcome.
    async fn add_and_drain(base: &str, url: &str) -> String {
        let post = format!("{base}/api/archives");
        let body = serde_json::json!({ "path": url, "collection": "net" }).to_string();
        let job: serde_json::Value = {
            let post = post.clone();
            tokio::task::spawn_blocking(move || {
                let mut res = agent()
                    .post(&post)
                    .header("content-type", "application/json")
                    .header("x-indice-auth-secret", "s3cret")
                    .header("x-forwarded-email", "alice@x.edu")
                    .send(body)
                    .unwrap();
                assert_eq!(res.status().as_u16(), 202, "the job should be queued");
                serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
            })
            .await
            .unwrap()
        };
        let events = format!(
            "{base}/api/archives/{}/events",
            job["job"].as_u64().unwrap()
        );
        tokio::task::spawn_blocking(move || {
            let mut res = agent()
                .get(&events)
                .header("x-indice-auth-secret", "s3cret")
                .header("x-forwarded-email", "alice@x.edu")
                .call()
                .unwrap();
            res.body_mut().read_to_string().unwrap()
        })
        .await
        .unwrap()
    }

    // Default: refused, and the transcript says why rather than claiming the
    // host does not support range requests.
    let tmp = home_with_roster();
    let cfg = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "x-forwarded-email",
        "s3cret",
    ));
    let (base, server) = serve(tmp.path().to_path_buf(), cfg).await;
    let transcript = add_and_drain(&base, &wacz_url).await;
    assert!(
        transcript.contains("refusing to fetch") && transcript.contains("127.0.0.1"),
        "the refusal should reach the curator and name the address: {transcript}"
    );
    assert!(
        !tmp.path().join("archive").join("net").exists(),
        "nothing should have been accessioned"
    );
    server.abort();

    // With the opt-out, the same URL indexes, which is what an institution
    // serving WACZs from an internal host needs.
    let tmp2 = home_with_roster();
    let mut cfg2 = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "x-forwarded-email",
        "s3cret",
    ));
    cfg2.allow_internal_fetch = true;
    let (base2, server2) = serve(tmp2.path().to_path_buf(), cfg2).await;
    let transcript2 = add_and_drain(&base2, &wacz_url).await;
    assert!(
        transcript2.contains("event: done"),
        "with --allow-internal-fetch the ingest should finish: {transcript2}"
    );
    let manifest = indice_lib::collections::Manifest::open(&tmp2.path().join("index")).unwrap();
    assert_eq!(manifest.waczs.len(), 1, "the crawl should be indexed");
    // And the searcher was reloaded, so adding by URL is covered end to end
    // rather than only as far as the manifest.
    let (status, body) = get(format!("{base2}/api/search?q=example")).await;
    assert_eq!(status, 200);
    let found: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        found["total"].as_u64().unwrap() > 0,
        "the reloaded searcher should find the fetched crawl: {body}"
    );
    server2.abort();
    origin_task.abort();
}

// ── Bounding what an upload may add ────────────────────────────────────────

/// A multipart upload body: the collection, then a file of `file` bytes.
fn upload_body(collection: &str, file: &[u8]) -> (String, Vec<u8>) {
    let boundary = "----indiceLimitTest";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"collection\"\r\n\r\n{collection}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.wacz\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// POST an upload body, either with a `Content-Length` or chunked without one,
/// the way a browser does: the body goes out on one thread while the reply is
/// read on another.
///
/// That is the point of it. A server refusing an upload answers before the body
/// has all arrived, then closes rather than read the rest. ureq writes the whole
/// body before it reads anything, so it met that refusal as a broken pipe and
/// never saw the status, which on Linux, where the reset comes sooner, failed
/// these tests now and then (`rustyweb-6a1b`).
async fn post_upload(
    base: &str,
    content_type: String,
    body: Vec<u8>,
    chunked: bool,
) -> (u16, String) {
    send_upload(base, content_type, body, chunked, std::time::Duration::ZERO).await
}

/// [`post_upload`], pausing between chunks of a chunked body, so a test can be
/// sure the server answers while the body is still being sent.
async fn send_upload(
    base: &str,
    content_type: String,
    body: Vec<u8>,
    chunked: bool,
    pause: std::time::Duration,
) -> (u16, String) {
    use std::io::Write;
    let addr = base.trim_start_matches("http://").to_string();
    tokio::task::spawn_blocking(move || {
        let mut sock = std::net::TcpStream::connect(&addr).unwrap();
        let limit = Some(std::time::Duration::from_secs(30));
        sock.set_read_timeout(limit).unwrap();
        sock.set_write_timeout(limit).unwrap();
        let mut out = sock.try_clone().unwrap();
        let send = std::thread::spawn(move || -> std::io::Result<()> {
            let framing = if chunked {
                "Transfer-Encoding: chunked".to_string()
            } else {
                format!("Content-Length: {}", body.len())
            };
            write_upload_head(&mut out, &addr, &content_type, &framing)?;
            if chunked {
                for chunk in body.chunks(64 * 1024) {
                    write!(out, "{:x}\r\n", chunk.len())?;
                    out.write_all(chunk)?;
                    out.write_all(b"\r\n")?;
                    std::thread::sleep(pause);
                }
                out.write_all(b"0\r\n\r\n")
            } else {
                out.write_all(&body)
            }
        });
        let reply = read_reply(&mut sock);
        // The reply is in, so the send is over whatever state it is in. Shut
        // the socket so a write the server will never read fails now, rather
        // than leaving the join below waiting on it.
        let _ = sock.shutdown(std::net::Shutdown::Both);
        // Ignored on purpose: a refused upload's send fails, and that is the
        // case this client exists to survive.
        let _ = send.join().unwrap();
        reply
    })
    .await
    .unwrap()
}

/// The head of an upload request, up to where the body starts.
fn write_upload_head(
    out: &mut impl std::io::Write,
    addr: &str,
    content_type: &str,
    framing: &str,
) -> std::io::Result<()> {
    write!(
        out,
        "POST /api/archives/upload HTTP/1.1\r\nHost: {addr}\r\n\
         Content-Type: {content_type}\r\n{framing}\r\nConnection: close\r\n\r\n"
    )
}

/// Read one HTTP/1.1 reply: its head, then exactly `Content-Length` bytes of
/// body. Returns as soon as the reply is whole, so a reset that follows it, the
/// server closing on a body it would not read, is never seen. Anything short of
/// a whole reply panics with what did arrive, rather than passing a truncated
/// one off as complete.
fn read_reply(sock: &mut std::net::TcpStream) -> (u16, String) {
    use std::io::Read;
    let mut got = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        if let Some(reply) = whole_reply(&got) {
            return reply;
        }
        let read = sock.read(&mut buf);
        let partial = String::from_utf8_lossy(&got).into_owned();
        match read {
            Ok(0) => panic!("the connection closed mid-reply: {partial:?}"),
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) => panic!("{e} before the whole reply arrived: {partial:?}"),
        }
    }
}

/// The status and body, once `got` holds a whole reply.
fn whole_reply(got: &[u8]) -> Option<(u16, String)> {
    let end = got.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&got[..end]).expect("a reply head is ASCII");
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status line: {head:?}"));
    let mut length = None;
    for (name, value) in head.lines().skip(1).filter_map(|l| l.split_once(':')) {
        // Every upload reply is a string or JSON, which hyper sends with its
        // length. A chunked one would need a decoder this does not have, so it
        // is a failure here rather than a body read as text.
        assert!(
            !name.trim().eq_ignore_ascii_case("transfer-encoding"),
            "a reply without a length: {head:?}"
        );
        if name.trim().eq_ignore_ascii_case("content-length") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.unwrap_or_else(|| panic!("a reply without Content-Length: {head:?}"));
    let body = got.get(end + 4..end + 4 + length)?;
    Some((status, String::from_utf8_lossy(body).into_owned()))
}

/// Serve a fresh workstation home with this `config.yaml`.
async fn serve_with_config(
    config_yaml: &str,
) -> (tempfile::TempDir, String, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("config.yaml"), config_yaml).unwrap();
    let (base, server) = serve(tmp.path().to_path_buf(), local_access()).await;
    (tmp, base, server)
}

/// `site:` names the archive on every page and in the viewer, and its tagline
/// goes through the safe Markdown renderer. Both carry markup to show it is
/// escaped, not interpreted.
#[tokio::test]
async fn site_text_names_the_archive_everywhere() {
    let (tmp, base, server) = serve_with_config(
        "site:\n  name: Ada & Co <Archive>\n  tagline: |\n    \
         Pages <i>we</i> kept. **Demo**: see [about](https://example.org).\n    \
         <script>alert(1)</script> [x](javascript:alert(1))\n",
    )
    .await;
    let name = "Ada &amp; Co &lt;Archive&gt;";

    let (status, home) = get(format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        home.contains(&format!("<title>{name}</title>")),
        "title: {home}"
    );
    // The tab icon is named, and served from the public assets.
    assert!(
        home.contains(r#"rel="icon" type="image/svg+xml" href="/assets/favicon.svg""#),
        "{home}"
    );
    let (status, icon) = get(format!("{base}/assets/favicon.svg")).await;
    assert_eq!(status, 200);
    assert!(icon.starts_with("<svg"), "{icon}");
    // A browser drops an SVG that isn't well-formed XML, silently, and XML
    // forbids `--` inside a comment: an earlier copy broke on exactly that.
    for comment in icon.split("<!--").skip(1) {
        let body = comment.split("-->").next().unwrap_or_default();
        assert!(!body.contains("--"), "`--` inside an SVG comment: {body}");
    }
    assert!(
        home.contains(&format!(r#"class="wordmark" href="/">{name}</a>"#)),
        "{home}"
    );
    assert!(
        home.contains(&format!(r#"<h1 class="home-hero">{name}</h1>"#)),
        "{home}"
    );
    assert!(
        home.contains("Pages &lt;i&gt;we&lt;/i&gt; kept"),
        "tagline: {home}"
    );
    assert!(
        home.contains("<strong>Demo</strong>"),
        "tagline Markdown: {home}"
    );
    assert!(home.contains(r#"href="https://example.org""#), "{home}");
    assert!(
        !home.contains("<script>"),
        "raw HTML in the tagline must be escaped"
    );
    // Escaped hostile text may still spell the scheme; no live link may carry it.
    assert!(
        !home.contains(r#"href="javascript"#),
        "an unsafe link must be dropped"
    );

    // Every page, not just the homepage: the name in the title. The tagline
    // is the homepage's alone.
    let (_, search) = get(format!("{base}/search?q=anything")).await;
    assert!(
        search.contains(&format!("<title>anything - {name}</title>")),
        "{search}"
    );
    assert!(!search.contains("home-tagline"), "{search}");

    // The viewer, on a page of a collection: a finding aid is enough.
    std::fs::create_dir_all(tmp.path().join("collections/demo")).unwrap();
    std::fs::write(
        tmp.path().join("collections/demo/README.md"),
        "---\nname: demo\n---\n",
    )
    .unwrap();
    let (_, viewer) = get(format!("{base}/collection/demo/2025/https://example.org/")).await;
    assert!(
        viewer.contains(&format!("<title>Replay - {name}</title>")),
        "{viewer}"
    );
    assert!(
        viewer.contains(&format!(r#"<a class="home" href="/">{name}</a>"#)),
        "{viewer}"
    );
    assert!(
        !viewer.contains("{{site_name}}"),
        "every slot must be filled"
    );
    assert!(
        viewer.contains("/assets/favicon.svg"),
        "the viewer has the tab icon too"
    );
    server.abort();
}

/// With no `site:` section, the archive is called indice and says what it is.
#[tokio::test]
async fn site_text_defaults_without_config() {
    let (_tmp, base, server) = serve_with_config("").await;
    let (_, home) = get(format!("{base}/")).await;
    assert!(home.contains("<title>indice</title>"), "{home}");
    assert!(home.contains("Web archive search and replay"), "{home}");
    server.abort();
}

/// Pretend the archive holds `bytes`, with a sparse file that reports that size
/// and takes no disk. The archive limit counts what `archive/` holds, so this is
/// exactly what it measures.
fn fill_archive(home: &std::path::Path, bytes: u64) {
    let dir = home.join("archive").join("filler");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::File::create(dir.join("filler.bin"))
        .unwrap()
        .set_len(bytes)
        .unwrap();
}

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// An upload larger than `uploads.max_mb` is refused, and says which limit.
///
/// Sent chunked, with no length, so the refusal can only come from counting as
/// it streams. A body that declares its length is refused from its headers
/// before any of it is read, which is
/// `a_body_that_declares_itself_too_large_is_refused_before_it_is_sent`.
#[tokio::test]
async fn an_upload_over_the_size_limit_is_refused() {
    let (tmp, base, server) = serve_with_config("uploads:\n  max_mb: 1\n").await;
    let (ct, body) = upload_body("Big", &vec![0u8; 2 * MIB as usize]);
    let (status, text) = post_upload(&base, ct, body, true).await;
    assert_eq!(status, 413, "an over-limit upload must be refused");
    // The setting's name, not just the word "limit": axum's own refusal says
    // "length limit exceeded", and this must not be that.
    assert!(
        text.contains("uploads.max_mb"),
        "the refusal should name the setting, got: {text}"
    );

    // The desk carries the limit, so a browser refuses before sending.
    let (_, desk) = get(format!("{base}/manage/add")).await;
    assert!(
        desk.contains(&format!("data-max-upload=\"{MIB}\"")),
        "the desk should carry the per-upload limit"
    );

    // Under the limit still works: the bound is a bound, not a refusal of all.
    let wacz = std::fs::read(fixture("simple.wacz")).unwrap();
    let (ct, body) = upload_body("Small", &wacz);
    let (status, _) = post_upload(&base, ct, body, false).await;
    assert_eq!(status, 202, "an upload under the limit is accepted");
    drop(tmp);
    server.abort();
}

/// A refusal reaches a client that is still sending.
///
/// The server answers an over-limit upload as soon as it has counted past the
/// limit, and closes rather than read the rest. Sent slowly, the body is
/// certainly still going out when that happens, so this is the race the other
/// upload tests can only lose by chance, made certain: a client that reads
/// while it sends must still get the 413.
///
/// On loopback the reply reliably arrives ahead of the reset that follows it
/// (200 runs of 200 on Linux). Over a real network it need not, because a reset
/// can discard a reply the client has not yet read (RFC 9112, section 9.6), and
/// a browser may then report a network error instead. That is why the desk
/// checks both limits before it sends anything (`views/manage.rs`).
#[tokio::test]
async fn a_refusal_reaches_a_client_still_sending() {
    let (_tmp, base, server) = serve_with_config("uploads:\n  max_mb: 1\n").await;
    // 4 MiB in 64 KiB chunks, 20 ms apart: over a second of sending, and the
    // limit is passed about a quarter of the way in.
    let (ct, body) = upload_body("Slow", &vec![0u8; 4 * MIB as usize]);
    let (status, text) =
        send_upload(&base, ct, body, true, std::time::Duration::from_millis(20)).await;
    assert_eq!(status, 413, "the refusal must arrive: {text}");
    assert!(text.contains("uploads.max_mb"), "{text}");
    server.abort();
}

/// A copy of `simple.wacz` with every WARC deflated, so it is indexed by inflating
/// them to temp files, plus a deflated `archive/zeros.warc` that inflates to
/// `inflated` bytes from a few KiB: a small upload that asks for a lot of disk.
fn deflated_wacz_that_inflates_to(inflated: usize) -> Vec<u8> {
    use std::io::{Read, Write};
    let mut src =
        zip::ZipArchive::new(std::fs::File::open(fixture("simple.wacz")).unwrap()).unwrap();
    let mut out = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for i in 0..src.len() {
        let mut e = src.by_index(i).unwrap();
        let name = e.name().unwrap().into_owned();
        let options = if name.starts_with("archive/") {
            deflated
        } else {
            zip::write::SimpleFileOptions::default().compression_method(e.compression())
        };
        let mut bytes = Vec::new();
        e.read_to_end(&mut bytes).unwrap();
        out.start_file(name, options).unwrap();
        out.write_all(&bytes).unwrap();
    }
    // A real record, so the command line can index it: a resource whose
    // payload is zeros, which deflate a thousandfold.
    out.start_file("archive/zeros.warc", deflated).unwrap();
    write!(
        out,
        "WARC/1.0\r\nWARC-Type: resource\r\n\
         WARC-Record-ID: <urn:uuid:00000000-0000-0000-0000-000000000000>\r\n\
         WARC-Date: 2026-01-01T00:00:00Z\r\nWARC-Target-URI: https://ex.test/zeros\r\n\
         Content-Type: application/octet-stream\r\nContent-Length: {inflated}\r\n\r\n"
    )
    .unwrap();
    out.write_all(&vec![0u8; inflated]).unwrap();
    out.write_all(b"\r\n\r\n").unwrap();
    out.finish().unwrap().into_inner()
}

/// What a WACZ inflates to temp files while it is indexed counts against the
/// archive's room, so a small upload cannot fill the temp volume by naming a
/// zip entry that inflates a thousandfold. The CLI is not limited.
#[tokio::test]
async fn an_upload_may_not_inflate_past_the_archives_room() {
    let (tmp, base, server) = serve_with_config("uploads:\n  max_total_gb: 1\n").await;
    // Four MiB of room, and an upload of a few KiB that inflates to sixteen.
    fill_archive(tmp.path(), GIB - 4 * MIB);
    let wacz = deflated_wacz_that_inflates_to(16 * MIB as usize);
    assert!(
        (wacz.len() as u64) < MIB,
        "the upload itself fits: {}",
        wacz.len()
    );
    let (ct, body) = upload_body("Bomb", &wacz);
    let (status, text) = post_upload(&base, ct, body, false).await;
    assert_eq!(status, 202, "accepted, since the upload fits: {text}");
    let job: serde_json::Value = serde_json::from_str(&text).unwrap();
    let (_, transcript) = get(format!(
        "{base}/api/archives/{}/events",
        job["job"].as_u64().unwrap()
    ))
    .await;
    assert!(
        transcript.contains("event: error")
            && transcript.contains("zeros.warc")
            && transcript.contains("uploads.max_total_gb"),
        "indexing should stop at the room left, naming the entry: {transcript}"
    );
    // What it inflated went under the home, on the archive's volume, and is
    // gone. (The staged upload is a directory there, removed as the job ends.)
    let scratch = indice_lib::index::scratch_dir(tmp.path());
    let left: Vec<_> = std::fs::read_dir(&scratch)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .collect();
    assert!(left.is_empty(), "temp files left in {scratch:?}: {left:?}");
    server.abort();

    // The same file from the command line is the operator's, and indexes.
    let cli = tempfile::TempDir::new().unwrap();
    let path = cli.path().join("bomb.wacz");
    std::fs::write(&path, &wacz).unwrap();
    indice_lib::index::index_path(&path, cli.path(), None, "cli").unwrap();
}

/// An upload that would take the archive past `uploads.max_total_gb` is refused
/// with the numbers, as is any upload once it is full, and the desk says which.
#[tokio::test]
async fn an_upload_that_would_overfill_the_archive_is_refused() {
    let (tmp, base, server) = serve_with_config("uploads:\n  max_total_gb: 1\n").await;

    // One MiB of room, and a two-MiB upload: refused while streaming.
    fill_archive(tmp.path(), GIB - MIB);
    let (_, desk) = get(format!("{base}/manage/add")).await;
    assert!(
        desk.contains("left") && !desk.contains("is full"),
        "the desk should show the room left"
    );
    assert!(
        desk.contains(&format!("data-room=\"{MIB}\"")),
        "and carry it, so a browser refuses a file that will not fit"
    );
    let (ct, body) = upload_body("More", &vec![0u8; 2 * MIB as usize]);
    let (status, text) = post_upload(&base, ct, body, true).await;
    assert_eq!(
        status, 507,
        "an upload past the archive limit must be refused"
    );
    assert!(
        text.contains("uploads.max_total_gb") && text.contains("left"),
        "the refusal should give the setting and the room left, got: {text}"
    );

    // Full: refused before any of the body is read.
    fill_archive(tmp.path(), GIB);
    let (ct, body) = upload_body("More", b"tiny");
    let (status, text) = post_upload(&base, ct, body, false).await;
    assert_eq!(status, 507, "a full archive must refuse every upload");
    assert!(text.contains("holds"), "and say what it holds: {text}");
    let (_, desk) = get(format!("{base}/manage/add")).await;
    assert!(
        desk.contains("is full") && desk.contains("Adding by URL still works"),
        "the desk should say it is full, and what still works"
    );
    server.abort();
}

/// With both limits set, the tighter one is the one enforced and named.
#[tokio::test]
async fn the_tighter_of_the_two_limits_is_the_one_reported() {
    let config = "uploads:\n  max_mb: 1\n  max_total_gb: 1\n";

    // Plenty of archive room: the per-upload cap is tighter, so 413.
    let (_tmp, base, server) = serve_with_config(config).await;
    let (ct, body) = upload_body("A", &vec![0u8; 2 * MIB as usize]);
    let (status, text) = post_upload(&base, ct, body, true).await;
    assert_eq!(status, 413, "per-upload cap is the tighter: {text}");
    assert!(text.contains("uploads.max_mb"), "{text}");
    server.abort();

    // Half a MiB of room: the archive is tighter, so a 768 KiB upload, under
    // the per-upload cap, is still refused, and as 507.
    let (tmp, base, server) = serve_with_config(config).await;
    fill_archive(tmp.path(), GIB - MIB / 2);
    let (ct, body) = upload_body("B", &vec![0u8; 3 * MIB as usize / 4]);
    let (status, text) = post_upload(&base, ct, body, true).await;
    assert_eq!(status, 507, "archive room is the tighter: {text}");
    assert!(text.contains("uploads.max_total_gb"), "{text}");
    server.abort();
}

/// A body that declares itself too large is refused from its headers.
///
/// The route's body limit counts frames as they arrive and never reads
/// `Content-Length`, so without a check on the header a client declaring 10 GB
/// would stream up to the cap before being told. This sends the headers and then
/// nothing: refused promptly means the header was read; a hang means the server
/// was waiting for a body it should never have wanted.
#[tokio::test]
async fn a_body_that_declares_itself_too_large_is_refused_before_it_is_sent() {
    let (_tmp, base, server) = serve_with_config("uploads:\n  max_mb: 1\n").await;
    let addr = base.trim_start_matches("http://").to_string();
    let (status, text) = tokio::task::spawn_blocking(move || {
        let mut sock = std::net::TcpStream::connect(&addr).unwrap();
        // Short: a server waiting for the body would never answer.
        sock.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        write_upload_head(
            &mut sock,
            &addr,
            "multipart/form-data; boundary=x",
            "Content-Length: 10737418240",
        )
        .unwrap();
        read_reply(&mut sock)
    })
    .await
    .unwrap();
    assert_eq!(
        status, 413,
        "a declared 10 GB body should be refused at once, got: {text}"
    );
    server.abort();
}

/// A URL that cannot be streamed is downloaded to index, and that download is
/// bounded by `uploads.max_mb` like an upload.
///
/// The fallback copies the whole response to a temp file, and the response is
/// whatever the host chooses to send, so unbounded it was a way to fill the
/// volume with one URL even when the archive itself was full.
#[tokio::test]
async fn a_url_that_must_be_downloaded_is_bounded_by_the_upload_limit() {
    // No range support, so indice has to download it rather than stream.
    let origin = axum::Router::new().route(
        "/big.wacz",
        axum::routing::get(|| async { vec![0u8; 2 * MIB as usize] }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = listener.local_addr().unwrap();
    let origin_task = tokio::spawn(async move {
        axum::serve(listener, origin.into_make_service())
            .await
            .unwrap();
    });

    let (_tmp, base, server) = serve_with_config("uploads:\n  max_mb: 1\n").await;
    let url = format!("http://{origin_addr}/big.wacz");
    let post = format!("{base}/api/archives");
    let job: serde_json::Value = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&post)
            .header("content-type", "application/json")
            .send(serde_json::json!({ "path": url, "collection": "net" }).to_string())
            .unwrap();
        serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap();
    let (_, transcript) = get(format!(
        "{base}/api/archives/{}/events",
        job["job"].as_u64().unwrap()
    ))
    .await;
    assert!(
        transcript.contains("uploads.max_mb"),
        "the download should be refused past the limit: {transcript}"
    );
    server.abort();
    origin_task.abort();
}

/// A full archive accepts nothing new, a URL included: indexing one writes
/// temp files, which count against the archive's room. Refused at once with
/// the numbers, before any job starts.
#[tokio::test]
async fn a_full_archive_refuses_a_url_too() {
    let (tmp, base, server) = serve_with_config("uploads:\n  max_total_gb: 1\n").await;
    fill_archive(tmp.path(), GIB);
    let post = format!("{base}/api/archives");
    let (status, text) = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&post)
            .header("content-type", "application/json")
            .send(
                serde_json::json!({ "path": "https://example.com/a.wacz", "collection": "net" })
                    .to_string(),
            )
            .unwrap();
        (
            res.status().as_u16(),
            res.body_mut().read_to_string().unwrap(),
        )
    })
    .await
    .unwrap();
    assert_eq!(status, 507, "{text}");
    assert!(text.contains("uploads.max_total_gb"), "{text}");
    server.abort();
}

/// The same download counts against the archive's room when only
/// `uploads.max_total_gb` is set: it is temp disk the accession costs.
#[tokio::test]
async fn a_url_that_must_be_downloaded_is_bounded_by_the_archives_room() {
    let origin = axum::Router::new().route(
        "/big.wacz",
        axum::routing::get(|| async { vec![0u8; 2 * MIB as usize] }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = listener.local_addr().unwrap();
    let origin_task = tokio::spawn(async move {
        axum::serve(listener, origin.into_make_service())
            .await
            .unwrap();
    });

    let (tmp, base, server) = serve_with_config("uploads:\n  max_total_gb: 1\n").await;
    fill_archive(tmp.path(), GIB - MIB);
    let url = format!("http://{origin_addr}/big.wacz");
    let post = format!("{base}/api/archives");
    let job: serde_json::Value = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&post)
            .header("content-type", "application/json")
            .send(serde_json::json!({ "path": url, "collection": "net" }).to_string())
            .unwrap();
        serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap();
    let (_, transcript) = get(format!(
        "{base}/api/archives/{}/events",
        job["job"].as_u64().unwrap()
    ))
    .await;
    assert!(
        transcript.contains("uploads.max_total_gb") && transcript.contains("temp files"),
        "the download should stop at the room left: {transcript}"
    );
    server.abort();
    origin_task.abort();
}

/// Uploads arriving together cannot each spend the same room.
///
/// Each one passes the request-time check, because each fits on its own; that
/// check cannot see the others. The job-time check runs under the write lock,
/// which serialises every job that files into the archive, so the second to get
/// there sees the first. Asserted as the property that matters — the archive
/// never goes past its limit — whichever of the two checks catches it.
#[tokio::test]
async fn concurrent_uploads_never_take_the_archive_past_its_limit() {
    let (tmp, base, server) = serve_with_config("uploads:\n  max_total_gb: 1\n").await;
    let wacz = std::fs::read(fixture("a.wacz")).unwrap();
    let one = wacz.len() as u64;
    // Room for one copy and a half: either alone fits, both do not.
    fill_archive(tmp.path(), GIB - one - one / 2);

    let mut posts = Vec::new();
    for collection in ["first", "second"] {
        let (ct, body) = upload_body(collection, &wacz);
        let base = base.clone();
        posts.push(tokio::spawn(async move {
            post_upload(&base, ct, body, false).await
        }));
    }
    let mut accepted = Vec::new();
    for p in posts {
        let (status, text) = p.await.unwrap();
        match status {
            202 => accepted.push(serde_json::from_str::<serde_json::Value>(&text).unwrap()),
            507 => {}
            other => panic!("unexpected status {other}: {text}"),
        }
    }
    let mut finished = 0;
    for job in &accepted {
        let (_, transcript) = get(format!(
            "{base}/api/archives/{}/events",
            job["job"].as_u64().unwrap()
        ))
        .await;
        if transcript.contains("event: done") {
            finished += 1;
        }
    }
    assert!(finished <= 1, "at most one of the two may be filed");
    assert!(
        indice_lib::index::archive_bytes(tmp.path()).unwrap() <= GIB,
        "the archive must not go past its limit"
    );
    server.abort();
}

/// On a workstation the operator is the admin, so the accession desk offers
/// both import wizards when both services are configured. The forward-auth
/// side, where a curator is not offered them, is
/// `authz.rs::only_an_admin_is_offered_the_import_tabs`.
#[tokio::test]
async fn a_workstation_operator_is_offered_the_import_tabs() {
    struct StubBx;
    impl indice_lib::browsertrix::BrowsertrixProvider for StubBx {
        fn client(&self) -> anyhow::Result<indice_lib::browsertrix::Client> {
            anyhow::bail!("a stub: this test never reaches Browsertrix")
        }
    }
    struct StubAit;
    impl indice_lib::archiveit::ArchiveItProvider for StubAit {
        fn client(&self) -> anyhow::Result<indice_lib::archiveit::Client> {
            anyhow::bail!("a stub: this test never reaches Archive-It")
        }
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = tmp.path().to_path_buf();
    let server = tokio::spawn(async move {
        indice_lib::server::serve_on_listeners(
            listener,
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
            &home,
            None,
            local_access(),
            indice_lib::server::Providers {
                browsertrix: Some(std::sync::Arc::new(StubBx)),
                archiveit: Some(std::sync::Arc::new(StubAit)),
                llm_key: None,
            },
        )
        .await
        .unwrap();
    });
    let (status, desk) = get(format!("http://127.0.0.1:{port}/manage/add")).await;
    assert_eq!(status, 200, "{desk}");
    for t in [r#"data-src="bx""#, r#"data-src="ait""#] {
        assert!(
            desk.contains(t),
            "the workstation operator should be offered {t}"
        );
    }
    server.abort();
}

/// A full archive refuses a download import before the import starts.
///
/// The stub providers fail if they are ever asked for a client. The refusal has
/// to come first: without it the handler answers 202 and the job fails later,
/// after the curator has been told the import is under way. A streamed
/// Browsertrix import stores nothing locally, so it is not refused for space.
#[tokio::test]
async fn a_full_archive_refuses_a_download_import_before_it_starts() {
    struct NeverBx;
    impl indice_lib::browsertrix::BrowsertrixProvider for NeverBx {
        fn client(&self) -> anyhow::Result<indice_lib::browsertrix::Client> {
            anyhow::bail!("the provider must not be reached when the archive is full")
        }
    }
    struct NeverAit;
    impl indice_lib::archiveit::ArchiveItProvider for NeverAit {
        fn client(&self) -> anyhow::Result<indice_lib::archiveit::Client> {
            anyhow::bail!("the provider must not be reached when the archive is full")
        }
    }

    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("config.yaml"),
        "uploads:\n  max_total_gb: 1\n",
    )
    .unwrap();
    fill_archive(tmp.path(), GIB);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = tmp.path().to_path_buf();
    let server = tokio::spawn(async move {
        indice_lib::server::serve_on_listeners(
            listener,
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
            &home,
            None,
            local_access(),
            indice_lib::server::Providers {
                browsertrix: Some(std::sync::Arc::new(NeverBx)),
                archiveit: Some(std::sync::Arc::new(NeverAit)),
                llm_key: None,
            },
        )
        .await
        .unwrap();
    });
    let base = format!("http://127.0.0.1:{port}");

    let post = |path: &'static str, body: serde_json::Value| {
        let url = format!("{base}{path}");
        async move {
            tokio::task::spawn_blocking(move || {
                let mut res = agent()
                    .post(&url)
                    .header("content-type", "application/json")
                    .send(body.to_string())
                    .unwrap();
                (
                    res.status().as_u16(),
                    res.body_mut().read_to_string().unwrap_or_default(),
                )
            })
            .await
            .unwrap()
        }
    };
    let item = serde_json::json!([{ "id": "i", "name": "n" }]);

    let (status, text) = post(
        "/api/browsertrix/import",
        serde_json::json!({ "org": "o", "collection": "c", "items": item, "download": true }),
    )
    .await;
    assert_eq!(status, 507, "a Browsertrix download import: {text}");

    let (status, text) = post(
        "/api/archiveit/import",
        serde_json::json!({ "collection_id": 1, "collection": "c", "crawls": [1] }),
    )
    .await;
    assert_eq!(status, 507, "an Archive-It import: {text}");

    let (status, _) = post(
        "/api/browsertrix/import",
        serde_json::json!({ "org": "o", "collection": "c", "items": item, "download": false }),
    )
    .await;
    assert_ne!(status, 507, "a streamed import uses no local disk");
    server.abort();
}

/// A text field in an upload is bounded whatever `config.yaml` says.
///
/// `Field::text` buffers the whole field, and the body limit is lifted on this
/// route when no per-upload cap is set, so without this a client could send a
/// collection name of several gigabytes and exhaust the server's memory.
#[tokio::test]
async fn an_oversized_text_field_is_refused_even_with_no_limits_set() {
    let (tmp, base, server) = serve_with_config("").await;
    let huge_name = "a".repeat(64 * 1024);
    let (ct, body) = upload_body(&huge_name, b"x");
    let (status, text) = post_upload(&base, ct, body, true).await;
    assert_eq!(status, 413, "an oversized text field must be refused");
    assert!(text.contains("text field"), "and say which part: {text}");
    assert!(!tmp.path().join("archive").exists());
    server.abort();
}

/// An OpenAI-compatible service answering every request with `content` as
/// the model's message. Returns its `/v1` endpoint, and the request bodies it
/// was sent.
fn fake_llm(content: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let sent = Arc::new(Mutex::new(Vec::new()));
    let record = sent.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            record
                .lock()
                .unwrap()
                .push(String::from_utf8(body).unwrap());
            let answer =
            serde_json::json!({"choices": [{"message": {"role": "assistant", "content": content}}]})
                .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{answer}",
                answer.len()
            )
            .unwrap();
        }
    });
    (endpoint, sent)
}

/// POST a urlencoded form, following the redirect; returns status and body.
async fn post_fields(url: String, fields: Vec<(&'static str, String)>) -> (u16, String) {
    tokio::task::spawn_blocking(move || {
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &fields {
            ser.append_pair(k, v);
        }
        let mut res = agent()
            .post(&url)
            .header("content-type", "application/x-www-form-urlencoded")
            .send(ser.finish())
            .unwrap();
        (
            res.status().as_u16(),
            res.body_mut().read_to_string().unwrap(),
        )
    })
    .await
    .unwrap()
}

/// The editor's Draft button end to end: the button names the model and
/// service; a draft fills only empty fields, offers itself beside written ones
/// and writes nothing; Save keeps what the curator chose and records which
/// fields began as a draft; the collection page says so.
#[tokio::test]
async fn the_editor_drafts_a_finding_aid_and_save_records_it() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let archive = home.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let dest = archive.join("github-bitcoin-mining.wacz");
    std::fs::copy(fixture("github-bitcoin-mining.wacz"), &dest).unwrap();
    indice_lib::index::index_path(&dest, home, None, "coll").unwrap();
    let (endpoint, sent) = fake_llm(
        r#"{"description": "Drafted abstract.", "narrative": "Drafted narrative.",
            "subjects": ["Bitcoin"], "dates": "2024", "rights": "Never accepted"}"#,
    );
    std::fs::write(
        home.join("config.yaml"),
        format!(
            "llm:\n  endpoint: {endpoint}\n  model: fake-model\n  notice: \
             \"A test service. [Privacy](https://example.org/privacy) <script>x</script>\"\n"
        ),
    )
    .unwrap();
    let service = endpoint
        .trim_start_matches("http://")
        .trim_end_matches("/v1")
        .to_string();
    let readme = home.join("collections/coll/README.md");
    let (base, server) = serve(home.to_path_buf(), local_access()).await;

    let (status, edit) = get(format!("{base}/manage/edit/coll")).await;
    assert_eq!(status, 200);
    assert!(edit.contains("Draft a description"), "{edit}");
    assert!(edit.contains("Drafts with <b>fake-model</b>"), "{edit}");
    assert!(edit.contains(&format!("<b>{service}</b>")), "{edit}");
    // The operator's note, as safe Markdown, before anything is sent.
    assert!(
        edit.contains(r#"<a href="https://example.org/privacy">Privacy</a>"#),
        "{edit}"
    );
    assert!(
        !edit.contains("<script>x"),
        "raw HTML in the note is escaped"
    );
    // And what would be sent, down to the pages and the prompt.
    assert!(edit.contains("What will be sent: "), "{edit}");
    assert!(edit.contains("The full prompt"), "{edit}");
    assert!(edit.contains("/assets/editor.js"), "the waiting script");

    // The curator has written a description, and nothing else, then asks.
    let before = std::fs::read_to_string(&readme).unwrap();
    let (status, drafted) = post_fields(
        format!("{base}/manage/edit/coll/draft"),
        vec![
            ("name", "coll".into()),
            ("description", "The curator's abstract.".into()),
        ],
    )
    .await;
    assert_eq!(status, 200, "{drafted}");
    assert!(
        drafted.contains("drafted, review before saving"),
        "{drafted}"
    );
    assert!(
        drafted.contains("Drafted narrative."),
        "the empty narrative is filled"
    );
    assert!(
        drafted.contains("The draft suggests:"),
        "offered beside the written field"
    );
    assert!(drafted.contains("Drafted abstract."), "{drafted}");
    assert!(
        drafted.contains("value=\"The curator&#39;s abstract.\"")
            || drafted.contains("value=\"The curator's abstract.\""),
        "the curator's text stays in its field"
    );
    // The drafted text of each field rides along, so Save can tell which
    // ones the curator left as drafted.
    for hidden in [
        r#"name="draft_narrative" value="Drafted narrative.""#,
        r#"name="draft_dates" value="2024""#,
        r#"name="draft_description" value="Drafted abstract.""#,
    ] {
        assert!(drafted.contains(hidden), "{hidden}: {drafted}");
    }
    assert!(
        !drafted.contains("Never accepted"),
        "rights are not the model's"
    );
    assert_eq!(
        std::fs::read_to_string(&readme).unwrap(),
        before,
        "a draft writes nothing"
    );

    // Save: keep the filled fields, and choose the draft's description too.
    let (status, page) = post_fields(
        format!("{base}/api/collections"),
        vec![
            ("name", "coll".into()),
            ("description", "The curator's abstract.".into()),
            ("dates", "2024".into()),
            ("subjects", "Bitcoin".into()),
            ("narrative", "Drafted narrative.".into()),
            ("draft_model", "fake-model".into()),
            ("draft_service", service.clone()),
            ("draft_at", "2026-11-01T12:00:00Z".into()),
            ("draft_dates", "2024".into()),
            ("draft_subjects", "Bitcoin".into()),
            ("draft_narrative", "Drafted narrative.".into()),
            ("use_draft_description", "on".into()),
            ("draft_description", "Drafted abstract.".into()),
        ],
    )
    .await;
    assert_eq!(status, 200, "{page}");
    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    let c = manifest.collection_by_id("coll").unwrap();
    assert_eq!(
        c.description.as_deref(),
        Some("Drafted abstract."),
        "chosen, so swapped in"
    );
    let d = c.drafted.as_ref().expect("provenance recorded");
    assert_eq!(
        d.fields,
        vec!["description", "narrative", "subjects", "dates"]
    );
    assert_eq!(
        (d.model.as_str(), d.service.as_str()),
        ("fake-model", service.as_str())
    );
    assert!(
        page.contains(&format!("drafted with fake-model ({service})")),
        "the collection page says so: {page}"
    );
    drop(manifest);

    // Draft again, over a form still holding the first draft's narrative: it
    // is the model's text, not the curator's, so it is replaced and marked,
    // and the model isn't told the archivist wrote it.
    let (status, again) = post_fields(
        format!("{base}/manage/edit/coll/draft"),
        vec![
            ("name", "coll".into()),
            ("description", "The curator's abstract.".into()),
            ("narrative", "Drafted narrative.".into()),
            ("draft_model", "fake-model".into()),
            ("draft_narrative", "Drafted narrative.".into()),
        ],
    )
    .await;
    assert_eq!(status, 200, "{again}");
    assert!(
        again.contains(r#"name="draft_narrative" value="Drafted narrative.""#),
        "{again}"
    );
    let asked = sent.lock().unwrap().last().cloned().unwrap();
    assert!(asked.contains("The curator's abstract."), "{asked}");
    assert!(
        !asked.contains("narrative: Drafted narrative."),
        "the earlier draft isn't the archivist's: {asked}"
    );

    // A hand edit makes a field the curator's: it leaves the record, and the
    // rest stay.
    let saved = |narrative: &str, dates: &str, subjects: &str, description: &str| {
        vec![
            ("name", "coll".into()),
            ("description", description.to_string()),
            ("dates", dates.to_string()),
            ("subjects", subjects.to_string()),
            ("narrative", narrative.to_string()),
        ]
    };
    let (status, page) = post_fields(
        format!("{base}/api/collections"),
        saved("Rewritten by hand.", "2024", "Bitcoin", "Drafted abstract."),
    )
    .await;
    assert_eq!(status, 200, "{page}");
    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    let d = manifest.collection_by_id("coll").unwrap().drafted.clone();
    assert_eq!(
        d.expect("the untouched fields are still drafted").fields,
        vec!["description", "subjects", "dates"]
    );
    drop(manifest);

    // Every drafted field rewritten: no record, and the page stops saying so.
    let (_, page) = post_fields(
        format!("{base}/api/collections"),
        saved("Rewritten by hand.", "2023-2024", "Mining", "Mine."),
    )
    .await;
    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    assert_eq!(manifest.collection_by_id("coll").unwrap().drafted, None);
    assert!(!page.contains("drafted with"), "{page}");
    server.abort();
}

/// With no model configured there is no Draft button, and the route says why.
#[tokio::test]
async fn without_a_model_there_is_nothing_to_draft_with() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (base, server) = serve(tmp.path().to_path_buf(), local_access()).await;
    let (status, _) = post_fields(
        format!("{base}/api/collections"),
        vec![("name", "coll".into())],
    )
    .await;
    assert_eq!(status, 200);
    let (_, edit) = get(format!("{base}/manage/edit/coll")).await;
    assert!(!edit.contains("Draft a description"), "{edit}");
    let (status, page) = post_fields(
        format!("{base}/manage/edit/coll/draft"),
        vec![("name", "coll".into()), ("description", "kept".into())],
    )
    .await;
    assert_eq!(status, 503);
    assert!(page.contains("No language model is configured"), "{page}");
    assert!(
        page.contains("value=\"kept\""),
        "the curator's text survives: {page}"
    );
    server.abort();
}
