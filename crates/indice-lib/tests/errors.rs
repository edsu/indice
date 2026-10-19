//! What a failed request tells the client.
//!
//! Errors carry context naming files (`parsing /srv/indice/index/waczs.json`),
//! so a public route answers with a reference instead, and the log keeps the
//! whole chain under that reference. Someone who may change the archive is told
//! what went wrong.
//!
//! Its own test binary, because it installs a global `tracing` subscriber to
//! read the log back, which would capture every other test's lines too.

use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

use indice_lib::server;

const SECRET: &str = "s3cret";

/// Everything logged by this test binary, for the tests to search.
fn log() -> &'static Arc<Mutex<Vec<u8>>> {
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || Sink(sink.clone()))
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        buf
    })
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A running server, and the home whose path must not escape.
struct Server {
    app: String,
    replay: String,
    home: tempfile::TempDir,
}

/// Serve `home` with a curator and an admin on the roster.
async fn serve(
    home: tempfile::TempDir,
    resolver: Option<Arc<dyn indice_lib::index::SourceResolver>>,
) -> Server {
    log();
    std::fs::write(
        home.path().join("users.yaml"),
        "users:\n  \
         - id: curator@x.edu\n    role: curator\n  \
         - id: boss@x.edu\n    role: admin\n",
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let app = format!("http://{}", listener.local_addr().unwrap());
    let replay_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let replay = format!("http://{}", replay_listener.local_addr().unwrap());
    let mut config = server::ServerConfig::new(server::Access::proxy("x-forwarded-email", SECRET));
    config.replay_origin = Some(server::ReplayOrigin::for_listener(
        replay_listener.local_addr().unwrap(),
    ));
    let path = home.path().to_path_buf();
    tokio::spawn(async move {
        server::serve_on_listeners(
            listener,
            replay_listener,
            &path,
            resolver,
            config,
            server::Providers::default(),
        )
        .await
    });
    Server { app, replay, home }
}

/// A home whose manifest is unreadable, so anything that opens it fails.
fn broken_home() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("index")).unwrap();
    std::fs::write(tmp.path().join("index/waczs.json"), "not json").unwrap();
    tmp
}

/// Send `method` to `url`, signed in as `who` if given; status and body back.
async fn call(method: &'static str, url: String, who: Option<&'static str>) -> (u16, String) {
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        let mut res = match method {
            "GET" => sign_in(agent.get(&url), who).call(),
            _ => sign_in(agent.post(&url), who)
                .content_type("application/x-www-form-urlencoded")
                .send("with_crawls=false"),
        }
        .unwrap();
        (
            res.status().as_u16(),
            res.body_mut().read_to_string().unwrap(),
        )
    })
    .await
    .unwrap()
}

/// Add the proxy's headers for `who`, if anyone is signed in.
fn sign_in<B>(req: ureq::RequestBuilder<B>, who: Option<&str>) -> ureq::RequestBuilder<B> {
    match who {
        Some(who) => req
            .header("x-forwarded-email", who)
            .header("x-indice-auth-secret", SECRET),
        None => req,
    }
}

/// The reference an answer quotes, checked to be the 16 hex digits of an id.
fn reference(body: &str) -> String {
    let (_, rest) = body
        .split_once("reference ")
        .unwrap_or_else(|| panic!("no reference in {body:?}"));
    let id: String = rest.chars().take(16).collect();
    assert!(
        id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit()),
        "{body:?}"
    );
    id
}

/// The log line for request `id` that contains `needle`.
fn logged(id: &str, needle: &str) -> String {
    let log = String::from_utf8(log().lock().unwrap().clone()).unwrap();
    log.lines()
        .find(|l| l.contains(&format!("id={id}")) && l.contains(needle))
        .unwrap_or_else(|| panic!("no log line for {id} containing {needle:?} in:\n{log}"))
        .to_string()
}

#[tokio::test]
async fn a_public_failure_answers_with_a_reference_and_logs_the_cause() {
    let s = serve(broken_home(), None).await;
    let (status, body) = call("GET", format!("{}/", s.app), None).await;

    assert_eq!(status, 500, "{body}");
    let home = s.home.path().to_string_lossy().to_string();
    assert!(!body.contains(&home), "the home path escaped: {body}");
    assert!(!body.contains("waczs.json"), "a file name escaped: {body}");

    // The log has the cause, with its full path, under the same reference.
    let line = logged(&reference(&body), "waczs.json");
    assert!(
        line.contains(&home),
        "the log should keep the full path: {line}"
    );
}

#[tokio::test]
async fn a_failure_on_the_replay_origin_has_a_reference_too() {
    let s = serve(broken_home(), None).await;
    let url = format!("{}/collection/anything/replay.json", s.replay);
    let (status, body) = call("GET", url, None).await;

    assert_eq!(status, 500, "{body}");
    assert!(!body.contains("waczs.json"), "{body}");
    logged(&reference(&body), "waczs.json");
}

#[tokio::test]
async fn a_curator_is_told_what_failed() {
    let s = serve(broken_home(), None).await;
    let url = format!("{}/manage/edit/anything", s.app);
    let (status, body) = call("GET", url, Some("curator@x.edu")).await;

    assert_eq!(status, 500, "{body}");
    assert!(body.contains("waczs.json"), "{body}");
    logged(&reference(&body), "waczs.json");
}

#[tokio::test]
async fn an_admin_is_told_what_failed() {
    let s = serve(broken_home(), None).await;
    let url = format!("{}/api/collections/anything/delete", s.app);
    let (status, body) = call("POST", url, Some("boss@x.edu")).await;

    assert_eq!(status, 500, "{body}");
    assert!(body.contains("waczs.json"), "{body}");
    logged(&reference(&body), "waczs.json");
}

/// A resolver standing in for a Browsertrix that refuses the server.
struct Unreachable;

impl indice_lib::index::SourceResolver for Unreachable {
    fn resolve(&self, _: &indice_lib::collections::Source) -> anyhow::Result<String> {
        anyhow::bail!("login refused for org 75f05fd9 at https://bx.internal.example")
    }
}

#[tokio::test]
async fn an_unreachable_browsertrix_is_named_but_not_described() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("index")).unwrap();
    let crawl = serde_json::json!([{
        "id": "bx1",
        "collection": "c",
        "source": "browsertrix|https://bx.internal.example|org|item|a.wacz",
        "name": "n",
        "date_indexed": "2026-01-01T00:00:00Z",
        "file_size": 1,
        "sha256": "0",
    }]);
    std::fs::write(tmp.path().join("index/waczs.json"), crawl.to_string()).unwrap();
    let s = serve(tmp, Some(Arc::new(Unreachable))).await;

    let (status, body) = call("GET", format!("{}/files/bx1", s.replay), None).await;

    assert_eq!(status, 502, "{body}");
    assert!(body.contains("Browsertrix"), "{body}");
    assert!(
        !body.contains("bx.internal"),
        "the upstream's error escaped: {body}"
    );
    logged(&reference(&body), "login refused");
}
