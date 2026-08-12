//! Integration tests for the home config (`<home>/config.yaml`) and the seam
//! that makes it take effect: the indexer loads it and applies the stored-body
//! cap. A missing config indexes fine; a malformed one aborts the run (rather
//! than silently indexing with the default).

use std::path::Path;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn wacz() -> String {
    Path::new(FIXTURES)
        .join("simple.wacz")
        .to_string_lossy()
        .into_owned()
}

fn crawl_count(home: &Path) -> usize {
    indice_lib::collections::Manifest::open(&indice_lib::index::index_dir(home))
        .map(|m| m.waczs.len())
        .unwrap_or(0)
}

#[test]
fn index_honors_a_valid_config() {
    // A valid config.yaml indexes normally — exercising Config::load in
    // index_location (the seam that makes the setting do anything).
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    std::fs::write(
        indice_lib::config::Config::path(home),
        "index:\n  stored_body_cap_kb: 1\n",
    )
    .unwrap();

    indice_lib::index::index_location(&wacz(), home, Some("S"), "c", false, None, None).unwrap();
    assert_eq!(crawl_count(home), 1, "a valid config indexes normally");
}

#[test]
fn index_aborts_on_a_malformed_config() {
    // A present-but-broken config must not be silently ignored: indexing errors
    // (and doesn't index anything) instead of falling back to the default cap.
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    std::fs::write(
        indice_lib::config::Config::path(home),
        "index:\n  stored_body_cap_kb: sixteen\n", // not a number
    )
    .unwrap();

    let err = indice_lib::index::index_location(&wacz(), home, Some("S"), "c", false, None, None)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("config.yaml"),
        "the error points at the config file; got: {err}"
    );
    assert_eq!(crawl_count(home), 0, "a broken config indexes nothing");
}
