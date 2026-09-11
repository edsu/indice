//! Authorization: who may do what.
//!
//! The headline case is the one that motivated the whole permission layer — a
//! contributor who signs in to leave a note could previously delete the entire
//! collection it lived in, because authentication and authorization were the
//! same boolean.

use std::path::Path;

const SECRET: &str = "s3cret";
const USER_HEADER: &str = "x-forwarded-email";

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(name)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .new_agent()
}

/// A home with a roster: one admin, one curator, and (by omission) everyone
/// else a reader.
fn home_with_roster() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("users.yaml"),
        "users:\n  \
         - id: boss@x.edu\n    role: admin\n  \
         - id: curator@x.edu\n    role: curator\n",
    )
    .unwrap();
    tmp
}

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

/// Issue a request as `who` (or anonymously when `None`), returning the status.
/// `body` is `(content_type, payload)`; `None` sends no body.
async fn request(
    method: &'static str,
    url: String,
    who: Option<&'static str>,
    body: Option<(&'static str, String)>,
) -> u16 {
    tokio::task::spawn_blocking(move || {
        let agent = agent();
        // ureq encodes "may this request carry a body" in the builder's type
        // (WithoutBody vs WithBody), so GET and POST can't share one binding.
        let res = if method == "GET" {
            let mut req = agent.get(&url);
            if let Some(user) = who {
                req = req
                    .header("x-indice-auth-secret", SECRET)
                    .header(USER_HEADER, user);
            }
            req.call()
        } else {
            let mut req = agent.post(&url);
            if let Some(user) = who {
                req = req
                    .header("x-indice-auth-secret", SECRET)
                    .header(USER_HEADER, user);
            }
            match body {
                Some((ct, payload)) => req.header("content-type", ct).send(payload),
                None => req.send(""),
            }
        };
        res.unwrap().status().as_u16()
    })
    .await
    .unwrap()
}

/// What privilege a route demands.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Needs {
    Curator,
    Admin,
}

/// Every management route, the privilege it requires, and a body that gets far
/// enough into the handler to prove the *gate* ran (not that the operation
/// succeeded — hence asserting `!= 403` rather than a specific success code,
/// which would couple this table to each endpoint's semantics).
fn routes() -> Vec<(
    &'static str,
    &'static str,
    Needs,
    Option<(&'static str, String)>,
)> {
    let json = "application/json";
    let form = "application/x-www-form-urlencoded";
    vec![
        (
            "POST",
            "/api/collections",
            Needs::Curator,
            Some((form, "name=Table".into())),
        ),
        (
            "POST",
            "/api/archives",
            Needs::Curator,
            Some((json, r#"{"path":"/nope.wacz","collection":"t"}"#.into())),
        ),
        ("GET", "/api/archives/0/events", Needs::Curator, None),
        (
            "POST",
            "/api/browsertrix/import",
            Needs::Curator,
            Some((json, r#"{"org":"o","collection":"c","items":[]}"#.into())),
        ),
        ("GET", "/api/browsertrix/orgs", Needs::Curator, None),
        (
            "GET",
            "/api/browsertrix/collections?org=o",
            Needs::Curator,
            None,
        ),
        ("GET", "/api/browsertrix/items?org=o", Needs::Curator, None),
        ("GET", "/api/archiveit/collections", Needs::Curator, None),
        (
            "GET",
            "/api/archiveit/crawls?collection=1",
            Needs::Curator,
            None,
        ),
        (
            "POST",
            "/api/archiveit/import",
            Needs::Curator,
            Some((
                json,
                r#"{"collection_id":"1","collection":"c","crawls":[]}"#.into(),
            )),
        ),
        (
            "POST",
            "/api/annotations",
            Needs::Curator,
            Some((
                json,
                r#"{"collection":"t","url":"u","timestamp":"1","note":"n"}"#.into(),
            )),
        ),
        (
            "POST",
            "/api/annotations/x",
            Needs::Curator,
            Some((json, r#"{"collection":"t","note":"n"}"#.into())),
        ),
        (
            "POST",
            "/api/annotations/x/delete",
            Needs::Curator,
            Some((json, r#"{"collection":"t"}"#.into())),
        ),
        // Deaccession: the irreversible, shared acts.
        ("POST", "/api/crawls/abc/delete", Needs::Admin, None),
        (
            "POST",
            "/api/collections/abc/delete",
            Needs::Admin,
            Some((form, "with_crawls=on".into())),
        ),
    ]
}

#[tokio::test]
async fn every_management_route_demands_the_right_privilege() {
    let tmp = home_with_roster();
    let cfg = indice_lib::server::ManageConfig::forward_auth(USER_HEADER, SECRET);
    let (base, server) = serve(tmp.path().to_path_buf(), cfg).await;

    for (method, path, needs, body) in routes() {
        let url = format!("{base}{path}");

        // Anonymous: refused by the forward-auth middleware.
        let anon = request(method, url.clone(), None, body.clone()).await;
        assert_eq!(anon, 403, "{method} {path} must refuse an anonymous caller");

        // Authenticated but not on the roster: a Reader. Signing in is not
        // authorization.
        let stranger = request(method, url.clone(), Some("eve@x.edu"), body.clone()).await;
        assert_eq!(
            stranger, 403,
            "{method} {path} must refuse an authenticated non-member"
        );

        // A curator: allowed everywhere except deaccession.
        let curator = request(method, url.clone(), Some("curator@x.edu"), body.clone()).await;
        match needs {
            Needs::Curator => assert_ne!(
                curator, 403,
                "{method} {path} should be allowed for a curator"
            ),
            Needs::Admin => assert_eq!(
                curator, 403,
                "{method} {path} is deaccession — a curator must NOT be allowed"
            ),
        }

        // An admin: allowed everywhere.
        let admin = request(method, url.clone(), Some("boss@x.edu"), body.clone()).await;
        assert_ne!(admin, 403, "{method} {path} should be allowed for an admin");
    }

    server.abort();
}

/// The bug the permission layer exists to fix, as a test: signing in to write a
/// note used to carry the power to delete the whole collection.
#[tokio::test]
async fn a_curator_cannot_delete_a_collection_out_from_under_its_notes() {
    let tmp = home_with_roster();
    let home = tmp.path().to_path_buf();
    let cfg = indice_lib::server::ManageConfig::forward_auth(USER_HEADER, SECRET);
    let (base, server) = serve(home.clone(), cfg).await;

    // An admin accessions a collection and adds a crawl to it.
    let status = request(
        "POST",
        format!("{base}/api/collections"),
        Some("boss@x.edu"),
        Some(("application/x-www-form-urlencoded", "name=Shared".into())),
    )
    .await;
    assert_eq!(status, 303);
    let path = fixture("simple.wacz").to_string_lossy().to_string();
    let status = request(
        "POST",
        format!("{base}/api/archives"),
        Some("boss@x.edu"),
        Some((
            "application/json",
            serde_json::json!({ "path": path, "collection": "shared" }).to_string(),
        )),
    )
    .await;
    assert_eq!(status, 202);

    // A curator may annotate it...
    let note = serde_json::json!({
        "collection": "shared",
        "url": "https://example.org/",
        "timestamp": "20260101000000",
        "note": "a contributor's note",
    })
    .to_string();
    let status = request(
        "POST",
        format!("{base}/api/annotations"),
        Some("curator@x.edu"),
        Some(("application/json", note)),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "curator may annotate: {status}"
    );

    // ...but must not be able to delete the collection those notes live in.
    let status = request(
        "POST",
        format!("{base}/api/collections/shared/delete"),
        Some("curator@x.edu"),
        Some(("application/x-www-form-urlencoded", "with_crawls=on".into())),
    )
    .await;
    assert_eq!(status, 403, "deaccession is an admin act");
    assert!(
        home.join("collections/shared/annotations.jsonl").exists(),
        "the notes must still be on disk"
    );

    server.abort();
}

/// An admin may moderate a note they did not write; a peer curator may not.
#[tokio::test]
async fn notes_are_author_gated_but_admins_moderate() {
    let tmp = home_with_roster();
    let cfg = indice_lib::server::ManageConfig::forward_auth(USER_HEADER, SECRET);
    let (base, server) = serve(tmp.path().to_path_buf(), cfg).await;

    request(
        "POST",
        format!("{base}/api/collections"),
        Some("boss@x.edu"),
        Some(("application/x-www-form-urlencoded", "name=Notes".into())),
    )
    .await;

    // The curator writes a note and reads back its id.
    let url = format!("{base}/api/annotations");
    let body = serde_json::json!({
        "collection": "notes",
        "url": "https://example.org/",
        "timestamp": "20260101000000",
        "note": "mine",
    })
    .to_string();
    let id = tokio::task::spawn_blocking(move || {
        let mut res = agent()
            .post(&url)
            .header("content-type", "application/json")
            .header("x-indice-auth-secret", SECRET)
            .header(USER_HEADER, "curator@x.edu")
            .send(body)
            .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&res.body_mut().read_to_string().unwrap()).unwrap();
        v["id"].as_str().unwrap().to_string()
    })
    .await
    .unwrap();

    let del = serde_json::json!({ "collection": "notes" }).to_string();
    // A peer curator is not the author and cannot moderate.
    let status = request(
        "POST",
        format!("{base}/api/annotations/{id}/delete"),
        Some("peer@x.edu"),
        Some(("application/json", del.clone())),
    )
    .await;
    assert_eq!(status, 403, "a stranger is a reader, and not the author");

    // The admin moderates it.
    let status = request(
        "POST",
        format!("{base}/api/annotations/{id}/delete"),
        Some("boss@x.edu"),
        Some(("application/json", del)),
    )
    .await;
    assert_eq!(status, 204, "an admin moderates anyone's note");

    server.abort();
}

/// No users.yaml must behave exactly as indice did before roles existed, or
/// upgrading silently locks an operator out of their own archive.
#[tokio::test]
async fn without_a_roster_every_authenticated_user_is_an_admin() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = indice_lib::server::ManageConfig::forward_auth(USER_HEADER, SECRET);
    let (base, server) = serve(tmp.path().to_path_buf(), cfg).await;

    for (method, path, _needs, body) in routes() {
        let status = request(method, format!("{base}{path}"), Some("anyone@x.edu"), body).await;
        assert_ne!(
            status, 403,
            "{method} {path} must stay open with no users.yaml"
        );
    }

    server.abort();
}

/// The display cookie renders chrome but must never authorize a write.
#[tokio::test]
async fn a_display_cookie_alone_cannot_write() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = indice_lib::server::ManageConfig::forward_auth(USER_HEADER, SECRET);
    let (base, server) = serve(tmp.path().to_path_buf(), cfg).await;

    // Log in through the proxy once to be issued the cookie.
    let login = format!("{base}/manage/add");
    let cookie = tokio::task::spawn_blocking(move || {
        let res = agent()
            .get(&login)
            .header("x-indice-auth-secret", SECRET)
            .header(USER_HEADER, "boss@x.edu")
            .call()
            .unwrap();
        res.headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|c| c.split(';').next())
            .map(str::to_string)
            .expect("a display cookie is issued at login")
    })
    .await
    .unwrap();

    // That cookie, with no proxy headers at all, must not carry a write.
    let url = format!("{base}/api/collections");
    let status = tokio::task::spawn_blocking(move || {
        agent()
            .post(&url)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", &cookie)
            .send("name=ViaCookie")
            .unwrap()
            .status()
            .as_u16()
    })
    .await
    .unwrap();
    assert_eq!(
        status, 403,
        "a cookie is evidence for rendering, not writing"
    );

    server.abort();
}
