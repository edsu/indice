//! CLI-surface tests that spawn the built `indice` binary (Cargo exposes its
//! path as `CARGO_BIN_EXE_indice`).

use std::process::Command;

/// `index` requires `--collection`: every crawl belongs to a collection, so a
/// bare `index <wacz>` must fail with guidance (exit 2), not invent a singleton.
#[test]
fn index_requires_a_collection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_indice"))
        .args(["index", "some.wacz"])
        .arg("--home")
        .arg(tmp.path())
        .output()
        .unwrap();

    assert!(!out.status.success(), "bare index should fail");
    assert_eq!(out.status.code(), Some(2), "exit code 2 for a usage error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--collection"),
        "stderr should tell the user to pass --collection: {stderr}"
    );
}

/// `search-url` reads the CDX from each crawl's *local* WACZ; a remote-sourced
/// crawl (Browsertrix / URL) has no local file, so it must be skipped — not
/// `unwrap()`ed into a panic (regression for the `Source::resolve()` None case).
#[test]
fn search_url_skips_remote_sources_without_panicking() {
    let tmp = tempfile::TempDir::new().unwrap();
    let index_dir = tmp.path().join("index");
    std::fs::create_dir_all(&index_dir).unwrap();
    // A manifest whose only crawl is a remote Browsertrix source (no local file).
    std::fs::write(
        index_dir.join("waczs.json"),
        r#"[{"id":"x","collection":"c","source":"browsertrix|https://app.browsertrix.com|org|item|res.wacz","name":"Remote crawl","date_indexed":"2026-01-01","file_size":0,"sha256":""}]"#,
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_indice"))
        .args(["search-url", "https://example.com/"])
        .arg("--home")
        .arg(tmp.path())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "search-url must not panic on a remote source; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("skipping remote"),
        "should note the skipped remote crawl; got: {combined}"
    );
}
