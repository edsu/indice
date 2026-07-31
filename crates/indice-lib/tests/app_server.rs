//! Integration test for [`indice_lib::server::serve_on_listener`] — the seam the
//! desktop app shell (`crates/indice-app`) is built on. Unlike the `router`
//! oneshot tests, this exercises the full real-socket path: bind `127.0.0.1:0`,
//! read the OS-assigned port back *before* serving (exactly what the app does to
//! point its window at the right port), then serve over TCP with client-address
//! connect-info wired up.

use std::path::Path;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(FIXTURES).join(name)
}

#[tokio::test]
async fn serve_on_listener_serves_over_a_real_socket_with_range_support() {
    // A home with one indexed collection so both the homepage and the WACZ bytes
    // endpoint have something to serve.
    let tmp = tempfile::TempDir::new().unwrap();
    let coll = "Socket Test";
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, coll).unwrap();
    let id = indice_lib::collections::slugify(coll);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let member = manifest.members_of(&id).next().unwrap().id.clone();
    drop(manifest);

    // Bind :0 and read the assigned port back before serving.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let home = tmp.path().to_path_buf();
    let server =
        tokio::spawn(
            async move { indice_lib::server::serve_on_listener(listener, &home, None).await },
        );

    let base = format!("http://127.0.0.1:{port}");

    // Homepage renders over the socket. ureq is blocking, so run it off-executor
    // via spawn_blocking; awaiting the handle lets the server task make progress.
    let home_url = format!("{base}/");
    let home_status =
        tokio::task::spawn_blocking(move || ureq::get(&home_url).call().unwrap().status().as_u16())
            .await
            .unwrap();
    assert_eq!(home_status, 200, "homepage should render over the socket");

    // The WACZ bytes endpoint honors Range (206) — the request shape ReplayWeb.page
    // makes while replaying.
    let files_url = format!("{base}/files/{member}");
    let range_status = tokio::task::spawn_blocking(move || {
        ureq::get(&files_url)
            .header("Range", "bytes=0-15")
            .call()
            .unwrap()
            .status()
            .as_u16()
    })
    .await
    .unwrap();
    assert_eq!(range_status, 206, "bytes endpoint should honor Range");

    server.abort();
}
