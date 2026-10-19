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

/// A server whose manifest is unreadable, so the homepage and the edit form
/// both fail. Returns its base URL and its home, whose path must not escape.
async fn broken_server() -> (String, tempfile::TempDir) {
    log();
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("users.yaml"),
        "users:\n  - id: curator@x.edu\n    role: curator\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("index")).unwrap();
    std::fs::write(tmp.path().join("index/waczs.json"), "not json").unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let replay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "x-forwarded-email",
        SECRET,
    ));
    config.replay_origin = Some(indice_lib::server::ReplayOrigin::for_listener(
        replay.local_addr().unwrap(),
    ));
    let home = tmp.path().to_path_buf();
    tokio::spawn(async move {
        indice_lib::server::serve_on_listeners(
            listener,
            replay,
            &home,
            None,
            config,
            indice_lib::server::Providers::default(),
        )
        .await
    });
    (format!("http://127.0.0.1:{port}"), tmp)
}

/// GET `url`, optionally signed in as `who`, returning status and body.
async fn get(url: String, who: Option<&'static str>) -> (u16, String) {
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut req = agent.get(&url);
        if let Some(who) = who {
            req = req
                .header("x-forwarded-email", who)
                .header("x-indice-auth-secret", SECRET);
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
async fn a_public_failure_answers_with_a_reference_and_logs_the_cause() {
    let (base, home) = broken_server().await;
    let (status, body) = get(format!("{base}/"), None).await;

    assert_eq!(status, 500, "{body}");
    let home = home.path().to_string_lossy().to_string();
    assert!(!body.contains(&home), "the home path escaped: {body}");
    assert!(!body.contains("waczs.json"), "a file name escaped: {body}");

    let (_, rest) = body
        .split_once("reference ")
        .unwrap_or_else(|| panic!("no reference in {body:?}"));
    let id = &rest[..8];
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{body:?}");

    // The log has the cause, on a line carrying the same reference.
    let log = String::from_utf8(log().lock().unwrap().clone()).unwrap();
    let line = log
        .lines()
        .find(|l| l.contains(&format!("id={id}")) && l.contains("waczs.json"))
        .unwrap_or_else(|| panic!("no log line for {id} naming the file in:\n{log}"));
    assert!(
        line.contains(&home),
        "the log should keep the full path: {line}"
    );
}

#[tokio::test]
async fn a_curator_is_told_what_failed() {
    let (base, _home) = broken_server().await;
    let (status, body) = get(
        format!("{base}/manage/edit/anything"),
        Some("curator@x.edu"),
    )
    .await;

    assert_eq!(status, 500, "{body}");
    assert!(body.contains("waczs.json"), "{body}");
    assert!(!body.contains("reference"), "{body}");
}
