//! Request limits, through a real socket.
//!
//! The limits key on the TCP peer, which only exists when the router is served
//! with connect info, so these run a real listener rather than `oneshot`. The
//! numbers they assert against are the module's own constants: a burst of 100
//! quick requests per client, and 64 open `/files` streams per client.

use std::path::Path;

const SECRET: &str = "limits-secret";
const BURST: usize = 100;
const STREAMS_PER_CLIENT: usize = 64;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(name)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// A server home with a curator on its roster and one indexed crawl.
fn server_home() -> (tempfile::TempDir, String) {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("users.yaml"),
        "users:\n  - id: curator@x.edu\n    role: curator\n",
    )
    .unwrap();
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, "test").unwrap();
    let id = indice_lib::collections::Manifest::open(&tmp.path().join("index"))
        .unwrap()
        .waczs[0]
        .id
        .clone();
    (tmp, id)
}

async fn serve(
    home: &Path,
    access: indice_lib::server::Access,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let replay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = indice_lib::server::ServerConfig::new(access);
    config.replay_origin = Some(indice_lib::server::ReplayOrigin::for_listener(
        replay.local_addr().unwrap(),
    ));
    let home = home.to_path_buf();
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

fn proxy() -> indice_lib::server::Access {
    indice_lib::server::Access::proxy("x-forwarded-email", SECRET)
}

/// GET `path` `n` times with these headers, returning each status and its
/// `Retry-After`.
async fn hammer_full(
    base: &str,
    path: &str,
    n: usize,
    headers: Vec<(&'static str, String)>,
) -> Vec<(u16, Option<String>)> {
    let url = format!("{base}{path}");
    tokio::task::spawn_blocking(move || {
        let agent = agent();
        (0..n)
            .map(|_| {
                let mut req = agent.get(&url);
                for (k, v) in &headers {
                    req = req.header(*k, v);
                }
                let mut res = req.call().unwrap();
                let retry = res
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let _ = res.body_mut().read_to_vec();
                (res.status().as_u16(), retry)
            })
            .collect()
    })
    .await
    .unwrap()
}

async fn hammer(
    base: &str,
    path: &str,
    n: usize,
    headers: Vec<(&'static str, String)>,
) -> Vec<u16> {
    hammer_full(base, path, n, headers)
        .await
        .into_iter()
        .map(|(s, _)| s)
        .collect()
}

/// Past its burst, a client is told to wait, with `Retry-After`; another client
/// behind the same proxy is not; and `/health` answers throughout.
///
/// The test client is on 127.0.0.1, which is where a reverse proxy sits, so
/// indice believes the `X-Forwarded-For` it sends. That is how two clients
/// behind one proxy get separate limits, and it is the case that matters: with
/// the header ignored, everyone behind a proxy would share one bucket.
#[tokio::test]
async fn a_client_past_its_burst_is_told_to_wait_and_others_are_not() {
    let (tmp, _) = server_home();
    let (base, server) = serve(tmp.path(), proxy()).await;
    let alice = vec![("x-forwarded-for", "9.9.9.9".to_string())];

    // Three bursts' worth, because the bucket refills at 20 a second while the
    // requests run: the first hundred cannot be refused, and somewhere after
    // them consumption outpaces the refill. Three times the burst keeps that
    // true even if each round trip takes tens of milliseconds on a slow runner.
    let results = hammer_full(&base, "/assets/app.css", 3 * BURST, alice.clone()).await;
    assert!(
        results[..BURST].iter().all(|(s, _)| *s == 200),
        "the burst is allowed"
    );
    let refused = results
        .iter()
        .find(|(s, _)| *s == 429)
        .expect("past the burst, a client is told to wait");
    assert!(refused.1.is_some(), "a 429 says how long to wait");

    // Someone else behind the same proxy has their own allowance.
    let bob = vec![("x-forwarded-for", "8.8.8.8".to_string())];
    assert_eq!(hammer(&base, "/assets/app.css", 1, bob).await, vec![200]);

    // And the orchestrator's probe is never limited.
    assert_eq!(hammer(&base, "/health", 1, alice).await, vec![200]);
    server.abort();
}

/// A curator vouched for by the proxy is not rate-limited, so an institution's
/// own script needs a roster entry rather than a setting.
///
/// Three bursts' worth, as in the test above, so that a client who was *not*
/// exempt would certainly be refused; fewer could pass here with the exemption
/// broken, on a machine slow enough for the bucket to keep up.
#[tokio::test]
async fn a_curator_is_not_rate_limited() {
    let (tmp, _) = server_home();
    let (base, server) = serve(tmp.path(), proxy()).await;
    let curator = vec![
        ("x-forwarded-for", "9.9.9.9".to_string()),
        ("x-indice-auth-secret", SECRET.to_string()),
        ("x-forwarded-email", "curator@x.edu".to_string()),
    ];
    let statuses = hammer(&base, "/assets/app.css", 3 * BURST, curator).await;
    assert!(
        statuses.iter().all(|&s| s == 200),
        "a curator must not be told to wait"
    );
    server.abort();
}

/// A workstation is not limited at all: one user, nothing to protect them from.
#[tokio::test]
async fn a_workstation_is_not_limited() {
    let (tmp, _) = server_home();
    let local = indice_lib::server::Access::local("127.0.0.1:0".parse().unwrap()).unwrap();
    let (base, server) = serve(tmp.path(), local).await;
    let statuses = hammer(&base, "/assets/app.css", 3 * BURST, vec![]).await;
    assert!(
        statuses.iter().all(|&s| s == 200),
        "no limit on a workstation"
    );
    server.abort();
}

/// A client may hold only so many `/files` streams open, and a stream counts
/// until its body is finished with, not until the handler returns.
///
/// The responses here are never read. The WACZ is larger than a socket buffer,
/// so the server cannot finish writing any of them, and each body stays alive
/// holding its slot. That is the slow-reader case the limit is for: counting
/// only until the handler returned would see none of them, since a streaming
/// handler returns at once.
#[tokio::test]
async fn a_client_may_hold_only_so_many_archive_streams_open() {
    let (tmp, id) = server_home();
    let (base, server) = serve(tmp.path(), proxy()).await;
    let url = format!("{base}/files/{id}");

    let (held, last) = tokio::task::spawn_blocking(move || {
        let agent = agent();
        let open = |who: &str| {
            agent
                .get(&url)
                .header("x-forwarded-for", who)
                .call()
                .unwrap()
        };
        // Kept, unread: each holds a stream open on the server.
        let held: Vec<_> = (0..STREAMS_PER_CLIENT).map(|_| open("9.9.9.9")).collect();
        let statuses: Vec<u16> = held.iter().map(|r| r.status().as_u16()).collect();
        let over = open("9.9.9.9").status().as_u16();
        let other = open("8.8.8.8").status().as_u16();
        (statuses, (over, other, held))
    })
    .await
    .unwrap();
    assert!(held.iter().all(|&s| s == 200), "within the cap: {held:?}");
    let (over, other, still_open) = last;
    assert_eq!(over, 429, "past the cap, a client is refused");
    assert_eq!(other, 200, "another client has its own streams");

    // Finishing with them frees the slots.
    drop(still_open);
    let base2 = base.clone();
    let mut freed = false;
    for _ in 0..50 {
        let status = tokio::task::spawn_blocking({
            let url = format!("{base2}/files/{id}");
            move || {
                agent()
                    .get(&url)
                    .header("x-forwarded-for", "9.9.9.9")
                    .header("range", "bytes=0-0")
                    .call()
                    .unwrap()
                    .status()
                    .as_u16()
            }
        })
        .await
        .unwrap();
        if status == 206 {
            freed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(freed, "dropped streams should give their slots back");
    server.abort();
}
