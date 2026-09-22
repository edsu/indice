use std::path::Path;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use tempfile::TempDir;
use tower::ServiceExt;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(FIXTURES).join(name)
}

fn make_index(paths: &[&str]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    for path in paths {
        index_into(tmp.path(), path);
    }
    tmp
}

/// Copy a fixture WACZ into `<home>/archive` and index it from there. Local
/// WACZs must live under the archive folder, so tests stage them there first.
fn index_into(home: &Path, name: &str) {
    let archive = home.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let dest = archive.join(name);
    std::fs::copy(fixture(name), &dest).unwrap();
    indice_lib::index::index_path(&dest, home, None, "test").unwrap();
}

// ── Indexing ──────────────────────────────────────────────────────────────────

#[test]
fn index_wacz_html_response_indexed() {
    let tmp = make_index(&["simple.wacz"]);
    let idx =
        indice_lib::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    let results = idx.search("example", 10).unwrap();
    assert!(
        !results.is_empty(),
        "HTML content from WACZ should be in fulltext index"
    );
}

#[test]
fn index_wacz_collection_document_indexed() {
    let tmp = make_index(&["simple.wacz"]);
    let idx =
        indice_lib::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    // The seed page URL ends with "example.com" so searching example.com finds the collection doc.
    let results = idx.search("example.com", 10).unwrap();
    assert!(
        results.iter().any(|r| r.doc_type == "collection"),
        "collection document should be searchable"
    );
}

#[test]
fn index_wacz_result_has_crawl_fields() {
    let tmp = make_index(&["simple.wacz"]);
    let idx =
        indice_lib::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    let results = idx.search("example", 10).unwrap();
    let page = results.iter().find(|r| r.doc_type == "page").unwrap();
    assert!(!page.crawl_id.is_empty(), "page should have crawl_id");
    assert_eq!(page.crawl_name, "simple");
}

/// A page document must carry the *right* three tags, not merely non-empty
/// ones.
///
/// These three travel together through every page-indexing function as adjacent
/// `&str` arguments in a fixed order, so transposing two of them is an easy
/// slip — and a slip that mis-tags every document in the index while leaving
/// the manifest perfectly correct, so every manifest-focused test here would
/// still pass. This asserts the search document instead, which is the only
/// place the mistake would show.
#[test]
fn a_page_document_is_tagged_with_its_own_crawl_and_collection() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let archive = home.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let dest = archive.join("simple.wacz");
    std::fs::copy(fixture("simple.wacz"), &dest).unwrap();
    indice_lib::index::index_path(&dest, home, Some("A Readable Name"), "Tagged Coll").unwrap();

    // What the manifest says this crawl is.
    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    let wacz = &manifest.waczs[0];

    let idx = indice_lib::search::SearchIndex::open(home.join("index").join("full_text").as_path())
        .unwrap();
    let results = idx.search("example", 10).unwrap();
    let page = results
        .iter()
        .find(|r| r.doc_type == "page")
        .expect("a page document");

    assert_eq!(page.crawl_id, wacz.id, "crawl_id is the crawl's id");
    assert_eq!(
        page.crawl_name, "A Readable Name",
        "crawl_name is the display name, not the id"
    );
    assert_eq!(
        page.collection, "tagged-coll",
        "collection is the slug, not the display name or the crawl"
    );
    // The three are pairwise distinct here on purpose: if any two were equal a
    // transposition would pass unnoticed.
    assert_ne!(page.crawl_id, page.crawl_name);
    assert_ne!(page.crawl_name, page.collection);
    assert_ne!(page.crawl_id, page.collection);
}

#[test]
fn index_wacz_writes_manifest_with_metadata() {
    let tmp = make_index(&["simple.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(manifest.waczs.len(), 1);
    let col = &manifest.waczs[0];
    assert_eq!(col.name, "simple");
    assert!(!col.id.is_empty());
    assert!(!col.sha256.is_empty());
    // simple.wacz has a pages/pages.jsonl with one page
    assert!(
        !col.seed_pages.is_empty(),
        "should have seed pages from pages.jsonl"
    );
}

#[test]
fn optimize_compacts_in_place_and_keeps_search_working() {
    let tmp = make_index(&["simple.wacz"]);
    // Compact the existing index — no sources are re-read.
    let (before, after) =
        indice_lib::index::optimize(tmp.path(), 8, indice_lib::index::no_progress()).unwrap();
    assert!(
        after >= 1 && after <= before,
        "before={before} after={after}"
    );
    // The manifest is untouched (no source re-read) and the index still answers.
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(manifest.waczs.len(), 1);
    let idx = indice_lib::search::SearchIndex::open_read_only(
        tmp.path().join("index").join("full_text").as_path(),
    )
    .unwrap();
    assert!(!idx.search("example", 10).unwrap().is_empty());
}

#[test]
fn optimize_errors_clearly_when_there_is_no_index() {
    let tmp = TempDir::new().unwrap();
    let err = indice_lib::index::optimize(tmp.path(), 8, indice_lib::index::no_progress())
        .unwrap_err()
        .to_string();
    assert!(err.contains("no search index"), "unexpected error: {err}");
}

// ── Search API ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn search_api_returns_results() {
    let tmp = make_index(&["simple.wacz"]);
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/api/search?q=example")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json["results"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "search should return results: {json}"
    );
}

#[tokio::test]
async fn search_api_result_includes_crawl_fields() {
    let tmp = make_index(&["simple.wacz"]);
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/api/search?q=example")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let results = json["results"].as_array().unwrap();
    let first = &results[0];
    assert!(first
        .get("crawl_id")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false));
    assert!(first.get("crawl_name").is_some());
    assert!(first.get("doc_type").is_some());
}

#[tokio::test]
async fn search_api_no_results() {
    let tmp = make_index(&["simple.wacz"]);
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/api/search?q=zzz_nonexistent_zzz")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let results = json["results"].as_array().unwrap();
    assert!(
        results.is_empty(),
        "nonexistent query should return empty results: {json}"
    );
}

// ── File serving ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn files_route_serves_registered_wacz() {
    let tmp = make_index(&["simple.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = &manifest.waczs[0].id;
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let req = Request::get(format!("/files/{id}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn files_route_range_request() {
    let tmp = make_index(&["simple.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = &manifest.waczs[0].id;
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let req = Request::get(format!("/files/{id}"))
        .header("range", "bytes=0-99")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        body.len(),
        100,
        "byte range should return exactly 100 bytes"
    );
}

#[tokio::test]
async fn files_route_unknown_id_404() {
    let tmp = TempDir::new().unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/files/deadbeef").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Replay contract ─────────────────────────────────────────────────────────
//
// wabac.js replays a WACZ by reading it over HTTP from /files/{id} with range
// requests. Actual rendering needs a browser, but these tests assert the
// server-side contract wabac depends on: the bytes we serve are exactly the
// WACZ on disk, ranges return the correct slice, the served archive is
// replayable content (its internal CDX resolves a page to a 200), and the
// viewer wires up <replay-web-page> so the service worker loads.

#[tokio::test]
async fn served_wacz_is_byte_identical_to_disk() {
    let tmp = make_index(&["a.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let req = Request::get(format!("/files/{id}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let served = to_bytes(resp.into_body(), usize::MAX).await.unwrap();

    let on_disk = std::fs::read(fixture("a.wacz")).unwrap();
    assert_eq!(
        served.len(),
        on_disk.len(),
        "served length should match file"
    );
    assert_eq!(
        served.as_ref(),
        on_disk.as_slice(),
        "served bytes must equal the WACZ on disk"
    );
}

#[tokio::test]
async fn served_range_matches_the_file_slice() {
    let tmp = make_index(&["a.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();

    // Request an interior slice and verify the exact bytes, not just the length.
    let req = Request::get(format!("/files/{id}"))
        .header("range", "bytes=100-199")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        resp.headers().get("content-range").unwrap(),
        &format!(
            "bytes 100-199/{}",
            std::fs::metadata(fixture("a.wacz")).unwrap().len()
        ),
    );
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();

    let on_disk = std::fs::read(fixture("a.wacz")).unwrap();
    assert_eq!(
        body.as_ref(),
        &on_disk[100..=199],
        "range must return the exact file slice"
    );
}

#[tokio::test]
async fn served_wacz_cdx_resolves_a_replayable_page() {
    let tmp = make_index(&["a.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();

    // Pull the whole WACZ through the HTTP endpoint the browser would use...
    let req = Request::get(format!("/files/{id}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let served = to_bytes(resp.into_body(), usize::MAX).await.unwrap();

    // ...write it out and confirm its embedded CDX (what wabac reads) resolves
    // a real page. a.wacz's seed is a 301; the storymaps URL is the 200 target.
    let served_path = tmp.path().join("served.wacz");
    std::fs::write(&served_path, &served).unwrap();

    let records = indice_lib::wacz::search_cdx(&served_path, REAL_URL).unwrap();
    let page = records
        .iter()
        .find(|r| r.status == 200 && r.mime.contains("html"));
    assert!(
        page.is_some(),
        "served WACZ should contain a replayable 200 HTML page for {REAL_URL}"
    );
}

#[tokio::test]
async fn viewer_wires_up_replay_web_page() {
    let tmp = TempDir::new().unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/replay/viewer").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        html.contains("replay-web-page"),
        "viewer must mount the component"
    );
    // Absolute replaybase is what makes the service worker resolve to
    // /replay/sw.js rather than /replay/replay/sw.js - the bug we hit.
    assert!(html.contains("replaybase"), "viewer must set replaybase");
    assert!(
        html.contains("/replay/"),
        "replaybase should be the absolute /replay/ path"
    );
    assert!(
        html.contains("rwp-url-change"),
        "viewer should track navigation for the banner"
    );
}

// ── Static assets ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn replay_viewer_served() {
    let tmp = TempDir::new().unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/replay/viewer").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn replay_asset_has_etag_and_no_cache() {
    let tmp = TempDir::new().unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/replay/viewer").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("etag").is_some(),
        "asset should carry an ETag"
    );
    assert_eq!(
        resp.headers().get("cache-control").unwrap(),
        "no-cache",
        "asset should be revalidated so new versions propagate"
    );
}

#[tokio::test]
async fn replay_asset_returns_304_when_etag_matches() {
    let tmp = TempDir::new().unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();

    // First request to learn the ETag.
    let req = Request::get("/replay/viewer").body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let etag = resp
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // Second request with matching If-None-Match should be 304.
    let req = Request::get("/replay/viewer")
        .header("if-none-match", &etag)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty(), "304 should have no body");
}

#[tokio::test]
async fn replay_root_redirects() {
    let tmp = TempDir::new().unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/replay/").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // /replay/ now redirects to homepage
    assert!(
        resp.status().is_redirection(),
        "expected redirect, got {}",
        resp.status()
    );
}

// ── Homepage ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn homepage_shows_collection_name() {
    let tmp = make_index(&["simple.wacz"]);
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let cname = manifest.collections[0].name.clone();
    assert!(
        text.contains(&cname),
        "homepage should show the collection name {cname:?}: {text}"
    );
    assert!(
        text.contains("aria-label=\"Local\""),
        "a local collection card should show the Local pill"
    );
}

#[tokio::test]
async fn homepage_card_links_to_collection_page() {
    let tmp = make_index(&["simple.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let cid = manifest.collections[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let resp = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains(&format!("href=\"/collection/{cid}\"")),
        "homepage card title should link to the collection page (/collection/{cid})"
    );
}

#[tokio::test]
async fn crawl_page_shows_metadata_and_pages() {
    let tmp = make_index(&["a.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let resp = app
        .oneshot(
            Request::get(format!("/crawl/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();

    assert!(html.contains("SHA-256"), "should show fixity metadata");
    assert!(html.contains("Replay"), "should have a replay button");
    assert!(html.contains("Pages"), "should have a pages section");
    assert!(
        html.contains("aria-label=\"Local\""),
        "a local crawl should show the Local source badge"
    );
    assert!(
        html.contains(&format!("crawl={id}")),
        "replay links should carry the crawl id so the viewer crumb can link back"
    );
    // a.wacz's seed page (title "2Tone: The Sound of Britain").
    assert!(html.contains("2Tone"), "should list the crawl's pages");
}

#[tokio::test]
async fn crawl_page_shows_browsertrix_provenance() {
    let tmp = make_index(&["a.wacz"]);
    // Mark the crawl as imported from Browsertrix, as `import browsertrix` does.
    indice_lib::index::set_browsertrix_provenance(
        tmp.path(),
        &tmp.path().join("archive/test/a.wacz"),
        "https://app.browsertrix.com",
        "item-xyz",
        "sha256:aa",
        Some(5),
    )
    .unwrap();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let resp = app
        .oneshot(
            Request::get(format!("/crawl/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        html.contains("Browsertrix (app.browsertrix.com)"),
        "crawl page should attribute the Browsertrix source"
    );
    assert!(
        html.contains("item-xyz"),
        "crawl page should show the Browsertrix item id"
    );
}

#[tokio::test]
async fn crawl_page_shows_multi_wacz_provenance() {
    let tmp = make_index(&["a.wacz"]);
    // Mark the entry as a multi-WACZ, as index_nested does for a nested file
    // (set on the manifest directly so the test needn't build a nested WACZ).
    let index_dir = tmp.path().join("index");
    let mut m = indice_lib::collections::Manifest::open(&index_dir).unwrap();
    m.waczs[0].nested_waczs = Some(3);
    m.save().unwrap();
    let id = m.waczs[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let resp = app
        .oneshot(
            Request::get(format!("/crawl/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        html.contains("Multi-WACZ"),
        "crawl page should flag a multi-WACZ"
    );
    assert!(
        html.contains("3 crawls bundled"),
        "and show the bundled-crawl count"
    );
}

/// A stand-in resolver that always returns a canned presigned URL.
struct FakeResolver(String);
impl indice_lib::index::SourceResolver for FakeResolver {
    fn resolve(&self, _source: &indice_lib::collections::Source) -> anyhow::Result<String> {
        Ok(self.0.clone())
    }
}

/// Rewrite the single crawl's source to a Browsertrix source (as `import
/// --stream` would record it) and return its id.
fn make_browsertrix_source(tmp: &TempDir) -> String {
    let index_dir = tmp.path().join("index");
    let mut m = indice_lib::collections::Manifest::open(&index_dir).unwrap();
    m.waczs[0].source = indice_lib::collections::Source::Browsertrix {
        host: "https://app.browsertrix.com".into(),
        org: "o1".into(),
        item: "item-1".into(),
        resource: "a.wacz".into(),
    };
    let id = m.waczs[0].id.clone();
    m.save().unwrap();
    id
}

#[tokio::test]
async fn browsertrix_replay_redirects_to_a_freshly_resolved_url() {
    let tmp = make_index(&["a.wacz"]);
    let id = make_browsertrix_source(&tmp);
    let resolver: std::sync::Arc<dyn indice_lib::index::SourceResolver> = std::sync::Arc::new(
        FakeResolver("https://files.example/a.wacz?sig=fresh".into()),
    );
    let app = indice_lib::server::router_with_resolver(tmp.path(), Some(resolver)).unwrap();

    let resp = app
        .oneshot(
            Request::get(format!("/files/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "https://files.example/a.wacz?sig=fresh"
    );
}

#[tokio::test]
async fn public_browsertrix_replay_redirects_via_resolver() {
    // A public Browsertrix source (as `import --public --stream` records) is
    // remote too: /files/{id} re-resolves a fresh presigned URL and redirects.
    let tmp = make_index(&["a.wacz"]);
    let id = {
        let index_dir = tmp.path().join("index");
        let mut m = indice_lib::collections::Manifest::open(&index_dir).unwrap();
        m.waczs[0].source = indice_lib::collections::Source::BrowsertrixPublic {
            host: "https://app.browsertrix.com".into(),
            org: "o1".into(),
            collection: "col-uuid".into(),
            resource: "a.wacz".into(),
        };
        let id = m.waczs[0].id.clone();
        m.save().unwrap();
        id
    };
    let resolver: std::sync::Arc<dyn indice_lib::index::SourceResolver> = std::sync::Arc::new(
        FakeResolver("https://files.example/pub.wacz?sig=fresh".into()),
    );
    let app = indice_lib::server::router_with_resolver(tmp.path(), Some(resolver)).unwrap();

    let resp = app
        .oneshot(
            Request::get(format!("/files/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "https://files.example/pub.wacz?sig=fresh"
    );
}

#[tokio::test]
async fn browsertrix_crawl_page_flags_remote_hosting() {
    let tmp = make_index(&["a.wacz"]);
    let id = make_browsertrix_source(&tmp);
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let resp = app
        .oneshot(
            Request::get(format!("/crawl/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("source-badge") && html.contains("aria-label=\"Remote\""),
        "a remotely-hosted crawl should show the remote badge"
    );
}

#[tokio::test]
async fn browsertrix_replay_without_credentials_is_unavailable() {
    let tmp = make_index(&["a.wacz"]);
    let id = make_browsertrix_source(&tmp);
    // Default router has no resolver (no credentials).
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let resp = app
        .oneshot(
            Request::get(format!("/files/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn collection_page_lists_members() {
    let tmp = make_index(&["a.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let coll_id = manifest.collections[0].id.clone();
    let wacz_id = manifest.waczs[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let resp = app
        .oneshot(
            Request::get(format!("/collection/{coll_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        html.contains("Crawls"),
        "collection page should have a members section"
    );
    assert!(
        html.contains(&format!("/crawl/{wacz_id}")),
        "collection page should link to its member crawl"
    );
    assert!(
        html.contains("aria-label=\"Local\""),
        "member cards should show a source pill"
    );
}

#[tokio::test]
async fn collection_page_flags_missing_minimum_fields() {
    // a.wacz's datapackage seeds only `dates` (no description/keywords/
    // contributors/licenses), so the About block renders but the DACS minimum —
    // Scope & Content, Creator, Access & Use — is still missing and prompted,
    // even though the collection isn't wholly empty.
    let tmp = make_index(&["a.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let coll_id = manifest.collections[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let resp = app
        .oneshot(
            Request::get(format!("/collection/{coll_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("Still needed:"),
        "should prompt for the gaps: {html}"
    );
    for f in ["Scope", "Creator", "Access"] {
        assert!(
            html.contains(f),
            "missing minimum field {f:?} should be listed"
        );
    }
}

#[tokio::test]
async fn collection_page_nudge_lists_only_still_missing_minimum() {
    // With a creator supplied, the "still needed" prompt names only the remaining
    // DACS-minimum gaps (Scope & Content, Access & Use) — not Creator.
    let tmp = make_index(&["a.wacz"]); // collection "test"
    indice_lib::index::set_collection(
        tmp.path(),
        "test",
        &indice_lib::collections::CollectionFields {
            creator: Some("Someone".into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let resp = app
        .oneshot(
            Request::get("/collection/test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    // `&` renders escaped; the exact joined list proves Creator isn't included.
    assert!(
        html.contains("Still needed: Scope &amp; Content, Access &amp; Use"),
        "nudge should list only the missing minimum fields: {html}"
    );
}

#[tokio::test]
async fn collection_page_shows_scoped_facets() {
    // The collection page carries a scoped facet overview: each value links into
    // a search restricted to this collection (`collection:<id>`), turning the page
    // into a faceted entry point rather than just a member list.
    let tmp = make_index(&["a.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let coll_id = manifest.collections[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let resp = app
        .oneshot(
            Request::get(format!("/collection/{coll_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();

    // At least one facet dimension renders (a.wacz has real captures → sites/years).
    assert!(
        html.contains("Top sites") || html.contains("By year"),
        "collection page should show a scoped facet overview"
    );
    // Its facet links scope the search to this collection (url-encoded `collection:`).
    assert!(
        html.contains(&format!("collection%3A{coll_id}")),
        "facet links should scope the search to this collection"
    );
}

#[tokio::test]
async fn crawl_page_shows_scoped_facets() {
    // The crawl detail page carries the same scoped facet overview as a
    // collection, scoped to the single crawl (`crawl:<id>`).
    let tmp = make_index(&["a.wacz"]);
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();
    let app = indice_lib::server::router(tmp.path()).unwrap();

    let resp = app
        .oneshot(
            Request::get(format!("/crawl/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        html.contains("Top sites") || html.contains("By year"),
        "crawl page should show a scoped facet overview"
    );
    assert!(
        html.contains(&format!("crawl%3A{id}")),
        "crawl facet links should scope the search to this crawl (via the crawl: alias)"
    );
}

#[tokio::test]
async fn collection_page_unknown_id_404() {
    let tmp = TempDir::new().unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let resp = app
        .oneshot(
            Request::get("/collection/deadbeef")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn thumb_route_unknown_id_404() {
    let tmp = TempDir::new().unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let resp = app
        .oneshot(Request::get("/thumb/deadbeef").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn collection_thumbnail_is_set_served_and_preferred() {
    let tmp = make_index(&["simple.wacz"]); // collection "test"
                                            // A small valid image the curator pins for the whole collection.
    let pic = tmp.path().join("pic.png");
    image::RgbImage::from_pixel(8, 8, image::Rgb([10, 20, 30]))
        .save(&pic)
        .unwrap();
    indice_lib::index::set_collection_thumbnail(tmp.path(), "test", &pic).unwrap();

    // Committed under the collection dir (git-trackable).
    assert!(tmp.path().join("collections/test/thumbnail.jpg").is_file());

    let app = indice_lib::server::router(tmp.path()).unwrap();
    // Served at /collection-thumb/<slug>.
    let resp = app
        .clone()
        .oneshot(
            Request::get("/collection-thumb/test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap(),
        "image/jpeg"
    );

    // The homepage card prefers it (links to /collection-thumb/test).
    let home = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = to_bytes(home.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("/collection-thumb/test"),
        "homepage card should use the collection thumbnail: {html}"
    );
}

#[tokio::test]
async fn collection_thumb_rejects_traversal_ids() {
    let tmp = TempDir::new().unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    for bad in ["..", "a%2Fb", "a.b"] {
        let resp = app
            .clone()
            .oneshot(
                Request::get(format!("/collection-thumb/{bad}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "id {bad:?} must not resolve a file"
        );
    }
}

#[tokio::test]
async fn collection_card_shows_placeholder_without_image() {
    // simple.wacz deflates its WARCs (scan path), so no thumbnail is generated —
    // the card should render the image area as a CSS placeholder, not an <img>.
    let tmp = make_index(&["simple.wacz"]);
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let resp = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("card-thumb"),
        "card should have an image area"
    );
    assert!(
        html.contains("thumb placeholder"),
        "no thumbnail should fall back to a CSS placeholder"
    );
}

#[tokio::test]
async fn home_directory_is_portable() {
    use indice_lib::collections::{Manifest, Source};

    // Build a home dir with the WACZ under <home>/archive, then index it.
    let base = TempDir::new().unwrap();
    let home_a = base.path().join("home-a");
    let archive = home_a.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::copy(fixture("simple.wacz"), archive.join("simple.wacz")).unwrap();
    indice_lib::index::index_path(&archive.join("simple.wacz"), &home_a, None, "test").unwrap();

    // The source is stored relative to home (portable), filed under its
    // collection folder, not absolute.
    let manifest = Manifest::open(&home_a.join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();
    assert_eq!(
        manifest.waczs[0].source,
        Source::File(Path::new("archive/test/simple.wacz").to_path_buf()),
        "local WACZ should be stored relative to home, under its collection"
    );

    // Move the whole home dir to a new path, then serve from there.
    let home_b = base.path().join("home-b");
    std::fs::rename(&home_a, &home_b).unwrap();

    let app = indice_lib::server::router(&home_b).unwrap();
    let resp = app
        .oneshot(
            Request::get(format!("/files/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "moved home should still resolve the WACZ"
    );
}

#[tokio::test]
async fn can_index_while_server_holds_the_index() {
    // A running server opens the index read-only (no write lock), so indexing
    // must be able to proceed concurrently.
    let tmp = make_index(&["simple.wacz"]);
    let _app = indice_lib::server::router(tmp.path()).unwrap(); // held, like a live server

    // This previously failed with a Tantivy LockBusy error.
    index_into(tmp.path(), "pdf-doc.wacz");

    // The newly indexed content is searchable.
    let idx = indice_lib::search::SearchIndex::open_read_only(
        tmp.path().join("index").join("full_text").as_path(),
    )
    .unwrap();
    assert!(!idx.search("\"flux capacitor\"", 10).unwrap().is_empty());
}

#[tokio::test]
async fn homepage_empty_collections() {
    let tmp = TempDir::new().unwrap();
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("No collections"),
        "empty index should show placeholder: {text}"
    );
}

// ── Remote (HTTP) source ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn index_from_http_url_and_link_directly() {
    use axum::routing::get;

    // Serve the simple.wacz fixture bytes over a local HTTP server.
    let wacz = std::fs::read(fixture("simple.wacz")).unwrap();
    let app = axum::Router::new().route(
        "/simple.wacz",
        get(move || {
            let bytes = wacz.clone();
            async move { bytes }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });

    let url = format!("http://{addr}/simple.wacz");
    let tmp = TempDir::new().unwrap();

    // index_location uses a blocking HTTP client; run it off the async runtime.
    let (url_c, dir_c) = (url.clone(), tmp.path().to_path_buf());
    tokio::task::spawn_blocking(move || {
        indice_lib::index::Ingest::new(&dir_c)
            .index_location(&url_c, "test")
            .unwrap();
    })
    .await
    .unwrap();
    server.abort();

    // The manifest records the URL as the source (not a local path).
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(manifest.waczs.len(), 1);
    let col = &manifest.waczs[0];
    assert_eq!(
        col.source,
        indice_lib::collections::Source::Url(url.clone())
    );

    // The downloaded WACZ was indexed and is searchable. Scope the index so its
    // writer lock is released before the router opens its own SearchIndex.
    {
        let idx = indice_lib::search::SearchIndex::open(
            tmp.path().join("index").join("full_text").as_path(),
        )
        .unwrap();
        assert!(!idx.search("example", 10).unwrap().is_empty());
    }

    // The crawl page links wabac directly at the remote URL, not through /files/{id}.
    let app2 = indice_lib::server::router(tmp.path()).unwrap();
    let resp = app2
        .oneshot(
            Request::get(format!("/crawl/{}", col.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("source=http%3A%2F%2F127.0.0.1"),
        "remote source should be used directly in viewer links"
    );
    assert!(
        !html.contains(&format!("/files/{}", col.id)),
        "remote source should not be routed through /files/{{id}}"
    );
}

/// A rebuild must not download a remote source, however the `Ingest` it is
/// called on was configured. Downloading rewrites the source from `Url` to
/// `File`, and since the crawl id is derived from the effective source, the
/// rebuild would upsert a *second* entry under a new id rather than updating the
/// first — leaving the original entry in place and its provenance
/// (`browsertrix`, `archive_it`, `added_by`, all looked up by id) unfound.
///
/// `reindex` forces `download` off for exactly this reason. Before `Ingest`
/// existed it passed `false` positionally, so this was unreachable; the builder
/// made it expressible, and this test is the guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebuild_does_not_download_a_remote_source() {
    use axum::routing::get;

    let wacz = std::fs::read(fixture("simple.wacz")).unwrap();
    let app = axum::Router::new().route(
        "/simple.wacz",
        get(move || {
            let bytes = wacz.clone();
            async move { bytes }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });

    let url = format!("http://{addr}/simple.wacz");
    let tmp = TempDir::new().unwrap();

    // Register it as a streamed remote source: no download.
    let (url_c, dir_c) = (url.clone(), tmp.path().to_path_buf());
    tokio::task::spawn_blocking(move || {
        indice_lib::index::Ingest::new(&dir_c)
            .index_location(&url_c, "remote-coll")
            .unwrap();
    })
    .await
    .unwrap();

    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(manifest.waczs.len(), 1);
    let original_id = manifest.waczs[0].id.clone();

    // Now rebuild with download explicitly on. The server is still up, so a
    // fetch would succeed — nothing but reindex's own choice prevents it.
    let dir_c = tmp.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        indice_lib::index::Ingest::new(&dir_c)
            .download(true)
            .reindex()
            .unwrap();
    })
    .await
    .unwrap();
    server.abort();

    let after = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(
        after.waczs.len(),
        1,
        "a rebuild must update the entry, not add one under a downloaded id"
    );
    assert_eq!(
        after.waczs[0].id, original_id,
        "the crawl id must be stable"
    );
    assert_eq!(
        after.waczs[0].source,
        indice_lib::collections::Source::Url(url),
        "the source must still be the remote URL"
    );
    assert!(
        std::fs::read_dir(tmp.path().join("archive"))
            .map(|d| d.count() == 0)
            .unwrap_or(true),
        "nothing should have been written into the archive"
    );
}

/// The `name` twin of the test above: a rebuild must not apply a `--name`
/// override, because `index_one` is handed each crawl's *recorded* name from the
/// manifest. No phase reads `cx.name` today, so this asserts the contract rather
/// than catching a live bug — `reindex` sets `name(None)` so that stays true if
/// one ever does.
#[test]
fn a_rebuild_does_not_rename_crawls() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let archive = home.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let dest = archive.join("simple.wacz");
    std::fs::copy(fixture("simple.wacz"), &dest).unwrap();
    indice_lib::index::index_path(&dest, home, Some("Recorded Name"), "coll").unwrap();

    indice_lib::index::Ingest::new(home)
        .name(Some("Should Not Apply"))
        .reindex()
        .unwrap();

    let after = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    assert_eq!(after.waczs.len(), 1);
    assert_eq!(
        after.waczs[0].name, "Recorded Name",
        "a rebuild keeps each crawl's recorded name"
    );
}

/// Re-adding an *existing* unattributed crawl must not claim it.
///
/// The already-indexed skip guard is scoped to the collection, so adding the
/// same URL into a different collection deliberately falls through and re-homes
/// the entry. That path must not also hand over custody: DESIGN's rule is that
/// an unattributed crawl is nobody's and only an admin may remove it, so
/// claiming one is a privilege change. `may_delete_crawl` is owner-or-admin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn re_homing_an_unattributed_crawl_does_not_claim_it() {
    use axum::routing::get;

    let wacz = std::fs::read(fixture("simple.wacz")).unwrap();
    let app = axum::Router::new().route(
        "/simple.wacz",
        get(move || {
            let bytes = wacz.clone();
            async move { bytes }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });
    let url = format!("http://{addr}/simple.wacz");
    let tmp = TempDir::new().unwrap();

    // An admin indexes it from the CLI: no request identity, so unattributed.
    let (url_c, dir_c) = (url.clone(), tmp.path().to_path_buf());
    tokio::task::spawn_blocking(move || {
        indice_lib::index::Ingest::new(&dir_c)
            .index_location(&url_c, "coll-a")
            .unwrap();
    })
    .await
    .unwrap();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(manifest.waczs[0].added_by, None, "CLI attributes nobody");

    // A curator adds the same URL into a different collection.
    let (url_c, dir_c) = (url.clone(), tmp.path().to_path_buf());
    tokio::task::spawn_blocking(move || {
        let curator = indice_lib::identity::SubjectId::parse("curator@x.edu").unwrap();
        indice_lib::index::Ingest::new(&dir_c)
            .actor(Some(&curator))
            .index_location(&url_c, "coll-b")
            .unwrap();
    })
    .await
    .unwrap();
    server.abort();

    let after = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(
        after.waczs.len(),
        1,
        "one crawl, re-homed rather than added"
    );
    assert_eq!(
        after.waczs[0].collection.as_str(),
        "coll-b",
        "the re-home itself is intended and still happens"
    );
    assert_eq!(
        after.waczs[0].added_by, None,
        "but custody is set once, at accession, and an existing entry keeps it"
    );
}

// ── Real-fixture smoke tests ───────────────────────────────────────────────────

const REAL_URL: &str = "https://storymaps.arcgis.com/stories/278e1b5c18a3474082e583e889705179";

#[test]
fn index_real_wacz_searchable() {
    let tmp = make_index(&["a.wacz"]);
    let idx =
        indice_lib::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    let results = idx.search("Britain", 10).unwrap();
    assert!(
        !results.is_empty(),
        "real wacz should be searchable for a term in its title/text"
    );
    // The storymaps page (title "2Tone: The Sound of Britain") should be among the hits.
    assert!(
        results.iter().any(|r| r.url == REAL_URL),
        "the storymaps page should be a result"
    );
}

#[test]
fn index_real_wacz_has_correct_url() {
    let tmp = make_index(&["a.wacz"]);
    let idx =
        indice_lib::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    let results = idx.search("Britain", 10).unwrap();
    assert!(
        results
            .iter()
            .any(|r| r.doc_type == "page" && r.url == REAL_URL),
        "a page document for the storymaps URL should exist"
    );
}

#[test]
fn index_pdf_text_is_searchable() {
    // pdf-doc.wacz wraps a real PDF (generated from text) as an
    // application/pdf response. Its body text ("flux capacitor ...") exists
    // only inside the PDF, so a hit proves PDF extraction ran during indexing.
    let tmp = make_index(&["pdf-doc.wacz"]);
    let idx =
        indice_lib::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    let results = idx.search("\"flux capacitor\"", 10).unwrap();
    assert!(!results.is_empty(), "PDF text should be searchable");
    let hit = &results[0];
    assert_eq!(hit.doc_type, "page");
    assert_eq!(hit.url, "http://example.com/report.pdf");
    assert!(
        hit.snippet.to_lowercase().contains("flux"),
        "snippet should highlight matched PDF text: {}",
        hit.snippet
    );
}

#[test]
fn index_real_wacz_indexes_rendered_text() {
    // The storymaps page is a Next.js SPA: its raw HTML body is nearly empty, so
    // before urn:text indexing only the title was searchable. Browsertrix's
    // urn:text record carries the fully rendered text (author name, body prose),
    // which we now index. "Scout Butler" (the author) appears only there.
    let tmp = make_index(&["a.wacz"]);
    let idx =
        indice_lib::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    let results = idx.search("\"Scout Butler\"", 10).unwrap();
    assert!(
        !results.is_empty(),
        "rendered-text-only phrase should be searchable via the urn:text record"
    );
    let hit = &results[0];
    assert_eq!(hit.doc_type, "page");
    assert!(
        hit.snippet.contains("Scout") || hit.snippet.contains("Butler"),
        "snippet should highlight the matched rendered text: {}",
        hit.snippet
    );
}

// ── Remote-fetch resilience ──────────────────────────────────────────────────

/// A transient HTTP failure (503 + Retry-After) is retried, then succeeds. This
/// exercises the retry *wiring* end to end: the agent built with
/// `http_status_as_error(false)` (so 4xx/5xx come back as responses), the
/// transient-status classification, `Retry-After` parsing, and `with_retry`.
#[tokio::test]
async fn get_reader_retries_a_transient_status() {
    use axum::response::IntoResponse;
    use axum::routing::get;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let app = axum::Router::new().route(
        "/f",
        get(move || {
            let h = h.clone();
            async move {
                // First request: transient failure, retry immediately (Retry-After: 0).
                if h.fetch_add(1, Ordering::SeqCst) == 0 {
                    (StatusCode::SERVICE_UNAVAILABLE, [("retry-after", "0")], "").into_response()
                } else {
                    (StatusCode::OK, "hello world").into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });

    let url = format!("http://{addr}/f");
    // get_reader is blocking (ureq); run it off the async runtime.
    let body = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut r = indice_lib::http_range::get_reader(&url).unwrap();
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        s
    })
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        body, "hello world",
        "should have retried past the 503 and read the 200 body"
    );
    assert!(
        hits.load(Ordering::SeqCst) >= 2,
        "the 503 should have triggered a retry (got {} requests)",
        hits.load(Ordering::SeqCst)
    );
}

/// The header search box on a collection page carries the collection scope (a
/// hidden `scope=collection:<id>` field) and a scoped placeholder, so searching
/// from there stays within the collection.
#[tokio::test]
async fn collection_page_header_search_is_scoped() {
    let tmp = make_index(&["simple.wacz"]); // indexed into collection "test"
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/collection/test")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains(r#"name="scope" value="collection:test""#),
        "header search carries the collection scope"
    );
    assert!(
        html.contains("Search test…"),
        "placeholder names the collection"
    );
}

/// A `scope` param folds into the query so it rides the normal filter machinery:
/// the results page shows a removable active-filter chip for the collection.
#[tokio::test]
async fn search_scope_param_folds_into_query() {
    let tmp = make_index(&["simple.wacz"]);
    let app = indice_lib::server::router(tmp.path()).unwrap();
    let req = Request::get("/search?scope=collection:test&q=example")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    // The folded-in collection scope surfaces as a removable active-filter chip
    // (broaden-to-everything is one click), which a bare `q=example` would lack.
    assert!(
        html.contains("filter-chip"),
        "scoped search renders the collection filter chip"
    );
}

// ── Concurrent writers ──────────────────────────────────────────────────────

/// A rebuild running concurrently with an ingest must not destroy it.
///
/// This is the race `index::lock` exists for, and the loss was total, not
/// partial. `reindex` builds into the sibling `index/full_text.new` while an
/// ingest writes `index/full_text`, and Tantivy's writer lock is named relative
/// to the index directory, so the two took different locks and neither saw the
/// other. The rebuild then renamed `full_text` aside, promoted its own, and
/// `remove_dir_all`'d the old one — taking the freshly-indexed crawl's
/// documents with it — and saved the manifest copy it had read before the
/// ingest started, erasing the entry too.
///
/// Both halves, silently, with no error on either side. The two threads here
/// stand in for `indice reindex` against a serving `serve --manage`; they get
/// separate open file descriptions, so they contend on the flock for real.
#[test]
fn a_rebuild_and_an_ingest_do_not_destroy_each_other() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();

    // Seed one crawl, so the rebuild has something to do and a manifest to read.
    let archive = home.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let first = archive.join("simple.wacz");
    std::fs::copy(fixture("simple.wacz"), &first).unwrap();
    indice_lib::index::index_path(&first, &home, Some("seeded"), "seeded-coll").unwrap();

    // The crawl the ingest will add, staged but not yet indexed.
    let second = archive.join("a.wacz");
    std::fs::copy(fixture("a.wacz"), &second).unwrap();

    // Both threads wait on the barrier, so they are released together rather
    // than merely spawned together. Without it the rebuild of a single small
    // fixture can finish before the other thread even reaches its acquisition
    // — on a faster machine, with warm caches, or if the fixtures shrink — and
    // the test then passes without the lock ever being contended, which is the
    // one way a regression could slip past it.
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let rebuild = {
        let (home, gate) = (home.clone(), gate.clone());
        std::thread::spawn(move || {
            gate.wait();
            indice_lib::index::Ingest::new(&home).reindex()
        })
    };
    let ingest = {
        let (home, gate) = (home.clone(), gate.clone());
        std::thread::spawn(move || {
            gate.wait();
            indice_lib::index::Ingest::new(&home)
                .index_location(&second.to_string_lossy(), "added-coll")
        })
    };
    rebuild.join().unwrap().expect("the rebuild succeeds");
    ingest.join().unwrap().expect("the ingest succeeds");

    // Whichever order they ran in, both crawls must be in the manifest...
    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    let names: Vec<&str> = manifest.waczs.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(
        manifest.waczs.len(),
        2,
        "both crawls must survive; got {names:?}"
    );

    // ...and both must still have documents, which is the half the swap
    // destroyed. Asked per collection rather than by searching for a word, so
    // this does not depend on what the fixtures happen to say.
    let idx = indice_lib::search::SearchIndex::open_read_only(
        home.join("index").join("full_text").as_path(),
    )
    .unwrap();
    for slug in ["seeded-coll", "added-coll"] {
        let (total, _) = idx.collection_pages(slug, None, None, 0, 1).unwrap();
        assert!(
            total > 0,
            "{slug} has a manifest entry but no documents: the rebuild's swap \
             deleted them (manifest holds {names:?})"
        );
    }
}

/// A deletion must survive a concurrent rebuild.
///
/// Without the index lock this half-undid itself. The rebuild snapshots the
/// manifest while the crawl is still registered; the delete then drops its
/// documents, removes `archive/<slug>/<file>.wacz`, and removes the manifest
/// entry; the rebuild re-indexes from its snapshot, finds the file missing,
/// warns "skipping missing local WACZ" but *preserves* the entry, and saves the
/// snapshot. The crawl is back in `waczs.json` pointing at nothing, and the
/// rebuild exits non-zero into the bargain.
#[test]
fn a_deletion_survives_a_concurrent_rebuild() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let archive = home.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    for (src, name) in [("simple.wacz", "keep.wacz"), ("a.wacz", "doomed.wacz")] {
        std::fs::copy(fixture(src), archive.join(name)).unwrap();
    }
    indice_lib::index::index_path(&archive.join("keep.wacz"), &home, Some("keep"), "keep-coll")
        .unwrap();
    indice_lib::index::index_path(
        &archive.join("doomed.wacz"),
        &home,
        Some("doomed"),
        "doomed-coll",
    )
    .unwrap();
    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    let doomed = manifest
        .waczs
        .iter()
        .find(|w| w.name == "doomed")
        .expect("the crawl to delete")
        .id
        .clone();

    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let rebuild = {
        let (home, gate) = (home.clone(), gate.clone());
        std::thread::spawn(move || {
            gate.wait();
            indice_lib::index::Ingest::new(&home).reindex()
        })
    };
    let delete = {
        let (home, gate, id) = (home.clone(), gate.clone(), doomed.clone());
        std::thread::spawn(move || {
            gate.wait();
            indice_lib::index::delete_crawl(&home, &id)
        })
    };
    // The rebuild may legitimately fail: if the delete wins the lock, the
    // rebuild's own snapshot is taken afterwards and is consistent, but if the
    // rebuild wins, it rebuilds a crawl that is then deleted. Either way the
    // delete must stick.
    let _ = rebuild.join().unwrap();
    delete.join().unwrap().expect("the delete succeeds");

    let after = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    assert!(
        after.waczs.iter().all(|w| w.id != doomed),
        "the deleted crawl must not be resurrected; manifest holds {:?}",
        after.waczs.iter().map(|w| &w.name).collect::<Vec<_>>()
    );
    assert!(
        after.waczs.iter().any(|w| w.name == "keep"),
        "and the untouched crawl must still be there"
    );
}

/// An annotation write is serialized against a rebuild.
///
/// A rebuild re-indexes annotations from the JSONL and then swaps the whole
/// index directory, so a note indexed between those two steps had its document
/// deleted along with the old index. `annotations.jsonl` survives, so the note
/// itself was never lost — but it was silently unsearchable until someone
/// rebuilt again, and nothing reported it.
///
/// Asserted as "the write waits" rather than by racing a rebuild, because the
/// losing interleaving is a narrow window and a test that only sometimes
/// exercises it is worse than one that always does.
#[test]
fn an_annotation_write_waits_for_the_index_lock() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let archive = home.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let staged = archive.join("simple.wacz");
    std::fs::copy(fixture("simple.wacz"), &staged).unwrap();
    indice_lib::index::index_path(&staged, &home, Some("noted"), "noted-coll").unwrap();

    // Hold the lock from another thread, which is what another process looks
    // like: the re-entrancy count is per thread, so this contends for real.
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let home = home.clone();
        std::thread::spawn(move || {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(home.join("index").join(".index.lock"))
                .unwrap();
            f.lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv();
        })
    };
    held_rx.recv().unwrap();

    let ann = indice_lib::annotations::Annotation::page(
        "https://example.com/",
        "20240101000000",
        "a note",
        indice_lib::annotations::Creator {
            kind: Some("Person".into()),
            id: Some("mailto:a@x.edu".into()),
            name: None,
        },
    );
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let writer = {
        let home = home.clone();
        std::thread::spawn(move || {
            indice_lib::index::index_annotation_upsert(&home, "noted-coll", &ann).unwrap();
            done_tx.send(()).unwrap();
        })
    };

    assert!(
        done_rx
            .recv_timeout(std::time::Duration::from_millis(400))
            .is_err(),
        "the annotation write must not proceed while the index is locked"
    );
    release_tx.send(()).unwrap();
    done_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("and it completes once the lock is free");
    writer.join().unwrap();
    holder.join().unwrap();
}

/// A description saved during an ingest must survive it.
///
/// This is what the manifest tier is for. `Manifest::save` rewrites
/// `waczs.json` wholesale from an in-memory vec, and an ingest used to open the
/// manifest before its loop and save after each crawl — so a curator editing a
/// finding aid in that window had their edit read, overwritten and lost, with
/// no error on either side. The window was the length of the ingest.
///
/// Both sides now re-open the manifest inside their own brief hold, so neither
/// can be writing from a stale copy.
#[test]
fn a_description_saved_during_an_ingest_is_not_erased() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let archive = home.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    // Seed the collection so there is something to describe, and so the ingest
    // below is adding a second crawl to an existing manifest.
    let seed = archive.join("simple.wacz");
    std::fs::copy(fixture("simple.wacz"), &seed).unwrap();
    indice_lib::index::index_path(&seed, &home, Some("seed"), "Notes").unwrap();

    let big = archive.join("a.wacz");
    std::fs::copy(fixture("a.wacz"), &big).unwrap();

    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let ingest = {
        let (home, gate) = (home.clone(), gate.clone());
        std::thread::spawn(move || {
            gate.wait();
            indice_lib::index::Ingest::new(&home).index_location(&big.to_string_lossy(), "Notes")
        })
    };
    let describe = {
        let (home, gate) = (home.clone(), gate.clone());
        std::thread::spawn(move || {
            gate.wait();
            // Land inside the ingest, which is where the old code lost it.
            std::thread::sleep(std::time::Duration::from_millis(40));
            indice_lib::index::set_collection(
                &home,
                "Notes",
                &indice_lib::collections::CollectionFields {
                    narrative: Some("A description a curator typed mid-ingest.".into()),
                    ..Default::default()
                },
                None,
            )
        })
    };
    ingest.join().unwrap().expect("the ingest succeeds");
    describe.join().unwrap().expect("the description saves");

    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    let coll = manifest
        .collections
        .iter()
        .find(|c| c.id.as_str() == "notes")
        .expect("the collection");
    assert_eq!(
        coll.narrative.as_deref(),
        Some("A description a curator typed mid-ingest."),
        "the curator's description was erased by the ingest"
    );
    assert_eq!(
        manifest.waczs.len(),
        2,
        "and both crawls are registered: {:?}",
        manifest.waczs.iter().map(|w| &w.name).collect::<Vec<_>>()
    );
}
