//! Integration tests for the collection multi-WACZ replay manifest
//! (`GET /collection/{id}/replay.json`), the primitive that lets wabac.js replay
//! a whole collection at once.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::path::Path;
use tower::ServiceExt; // for `oneshot`

/// A router rendering the **anonymous** view, which is what these tests check.
///
/// `Access::proxy` with credentials nobody sends: every request is an anonymous
/// visitor, exactly as one arriving at a server. `Access::local` would make each
/// request the operator and put workroom chrome on every page.
fn public_router(home: &std::path::Path) -> axum::Router {
    // A server refuses to start without a roster, so that everyone the identity
    // provider admits is not silently an admin. Empty is the right one here:
    // these tests are about what an anonymous reader sees, and `users: []`
    // means nobody may write.
    let roster = home.join("users.yaml");
    if !roster.exists() {
        std::fs::write(&roster, "users: []\n").unwrap();
    }
    let mut config = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "X-Forwarded-Email",
        "test-secret-not-sent",
    ));
    config.set_site_url("http://indice.test").unwrap();
    indice_lib::server::router(home, config).unwrap()
}

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(FIXTURES).join(name)
}

/// Index two local fixture WACZs into one collection and return `(home, id)`.
fn home_with_collection() -> (tempfile::TempDir, String) {
    let tmp = tempfile::TempDir::new().unwrap();
    let coll = "Test Collection";
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, coll).unwrap();
    indice_lib::index::index_path(
        &fixture("github-bitcoin-mining.wacz"),
        tmp.path(),
        None,
        coll,
    )
    .unwrap();
    (tmp, indice_lib::collections::slugify(coll))
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn get_text(app: axum::Router, uri: &str) -> (StatusCode, String) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn replay_json_lists_every_member_in_wabac_shape() {
    let (tmp, id) = home_with_collection();

    // The member ids the manifest recorded, to check the resource paths.
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let member_ids: Vec<String> = manifest.members_of(&id).map(|w| w.id.clone()).collect();
    assert_eq!(
        member_ids.len(),
        2,
        "both fixtures should be in the collection"
    );

    let app = public_router(tmp.path());
    let (status, json) = get_json(app, &format!("/collection/{id}/replay.json")).await;
    assert_eq!(status, StatusCode::OK);

    // Top-level shape wabac expects: { resources: [...], metadata: {...} }.
    let resources = json["resources"].as_array().expect("resources array");
    assert_eq!(resources.len(), 2);
    assert_eq!(json["metadata"]["title"], "Test Collection");

    for r in resources {
        let name = r["name"].as_str().unwrap();
        // name == crawlId == the WACZ id, and is a real member.
        assert_eq!(r["crawlId"].as_str().unwrap(), name);
        assert!(
            member_ids.iter().any(|m| m == name),
            "unknown member {name}"
        );
        // Local fixtures are File sources → served at /files/{id}.
        assert_eq!(r["path"].as_str().unwrap(), format!("/files/{name}"));
        // Hash carries the sha256: prefix wabac wants.
        assert!(
            r["hash"].as_str().unwrap().starts_with("sha256:"),
            "hash should be sha256:-prefixed, got {:?}",
            r["hash"]
        );
    }
}

#[tokio::test]
async fn collection_replay_button_opens_on_a_default_landing_page() {
    let (tmp, id) = home_with_collection();
    let app = public_router(tmp.path());
    let (status, html) = get_text(app, &format!("/collection/{id}")).await;
    assert_eq!(status, StatusCode::OK);

    // The "Replay collection" button replays the whole collection and, since
    // a whole-collection entry has no page in mind, opens on a default page
    // (the first member's first seed page) at its capture time. On the
    // read-only collection page the action row has just this one `.btn`.
    let btn = html
        .split("class=\"btn\" href=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("replay button href present");
    let tail = btn
        .strip_prefix(&format!("/collection/{id}/"))
        .expect("a whole-collection replay address");
    let (ts, url) = tail.split_once('/').expect("a time and a page");
    assert_eq!(ts.len(), 14, "the default page's capture time: {btn}");
    assert!(url.starts_with("http"), "the default page: {btn}");
}

#[tokio::test]
async fn replay_json_404s_for_unknown_collection() {
    let (tmp, _id) = home_with_collection();
    let app = public_router(tmp.path());
    let (status, _json) = get_json(app, "/collection/does-not-exist/replay.json").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn replay_json_metadata_has_no_pages_query_url() {
    let (tmp, id) = home_with_collection();
    let app = public_router(tmp.path());
    let (status, json) = get_json(app, &format!("/collection/{id}/replay.json")).await;
    assert_eq!(status, StatusCode::OK);
    // The manifest intentionally omits pagesQueryUrl: wabac replays natively,
    // loading each member's CDX and resolving URLs itself (reliable). The
    // pagesQueryUrl scale valve is deferred until it can be made reliable
    // (rustyweb-scale-footprint-qw5.10).
    assert!(
        json["metadata"].get("pagesQueryUrl").is_none(),
        "pagesQueryUrl should be omitted, got {:?}",
        json["metadata"]["pagesQueryUrl"]
    );
}

#[tokio::test]
async fn pages_endpoint_lists_pages_in_wabac_shape() {
    let (tmp, id) = home_with_collection();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let member_ids: Vec<String> = manifest.members_of(&id).map(|w| w.id.clone()).collect();

    let app = public_router(tmp.path());
    let (status, json) = get_json(app, &format!("/collection/{id}/pages?pageSize=100")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["total"].as_u64().unwrap() > 0, "collection has pages");
    let items = json["items"].as_array().expect("items array");
    assert!(!items.is_empty());
    for it in items {
        assert!(it["url"].is_string());
        assert!(it["title"].is_string());
        // filename == a member WACZ id (matches resources[].name in replay.json).
        let fname = it["filename"].as_str().unwrap();
        assert!(
            member_ids.iter().any(|m| m == fname),
            "unknown filename {fname}"
        );
        // ts is ISO 8601 so wabac's new Date(ts) parses it (never the raw 14-digit).
        let ts = it["ts"].as_str().unwrap();
        assert!(
            ts.is_empty() || (ts.contains('T') && ts.ends_with('Z')),
            "ts not ISO 8601: {ts}"
        );
    }
}

#[tokio::test]
async fn replay_json_uses_browsertrix_hash_for_streamed_members() {
    // A streamed member has no locally-computed sha256, but a Browsertrix import
    // kept the file hash from replay.json. The manifest must surface it
    // (sha256:-prefixed) so members keep distinct, verifiable hashes instead of a
    // shared empty "sha256:" that collapses them in wabac. See the multi-WACZ
    // collection replay fix.
    let (tmp, id) = home_with_collection();

    // Rewrite the first member: blank its sha256 and give it a Browsertrix
    // provenance carrying a (bare-hex) resource hash, as a streamed import does.
    let waczs = tmp.path().join("index").join("waczs.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&waczs).unwrap()).unwrap();
    let members = v.as_array_mut().unwrap();
    let target = members[0]["id"].as_str().unwrap().to_string();
    members[0]["sha256"] = serde_json::json!("");
    members[0]["browsertrix"] = serde_json::json!({
        "host": "https://example.browsertrix.com",
        "item_id": "item-1",
        "resource_hash": "deadbeef00",
    });
    std::fs::write(&waczs, serde_json::to_vec(&v).unwrap()).unwrap();

    let app = public_router(tmp.path());
    let (status, json) = get_json(app, &format!("/collection/{id}/replay.json")).await;
    assert_eq!(status, StatusCode::OK);
    let r = json["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == target)
        .expect("target member present");
    assert_eq!(
        r["hash"], "sha256:deadbeef00",
        "streamed member uses its Browsertrix hash, sha256:-prefixed"
    );
}

#[tokio::test]
async fn pages_endpoint_resolves_exact_url_to_its_member() {
    let (tmp, id) = home_with_collection();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    // The github-bitcoin-mining fixture is the member holding this URL.
    let gh = manifest
        .members_of(&id)
        .find(|w| w.name.contains("GitHub"))
        .expect("github member")
        .id
        .clone();

    let app = public_router(tmp.path());
    let target = "https://github.com/DocNow/hydrator/pull/78/files";
    let (status, json) = get_json(app, &format!("/collection/{id}/pages?url={target}")).await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().expect("items array");
    assert!(!items.is_empty(), "exact URL should resolve to a member");
    for it in items {
        assert_eq!(it["url"], target, "only the exact URL");
        assert_eq!(it["filename"], gh, "resolves to the member that holds it");
    }
}

// ── A crawl written into several WACZs ───────────────────────────────────────

/// The app and replay routers, with a replay origin so the viewer renders.
fn routers(home: &Path) -> indice_lib::server::Routers {
    let roster = home.join("users.yaml");
    if !roster.exists() {
        std::fs::write(&roster, "users: []\n").unwrap();
    }
    let mut config = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "X-Forwarded-Email",
        "test-secret-not-sent",
    ));
    config.set_site_url("http://indice.test").unwrap();
    config.replay_origin =
        Some(indice_lib::server::ReplayOrigin::parse("http://replay.indice.test").unwrap());
    indice_lib::server::routers(home, config).unwrap()
}

/// The ids of the collection's two members: `(github, other)`.
fn member_ids(home: &Path, id: &str) -> (String, String) {
    let manifest = indice_lib::collections::Manifest::open(&home.join("index")).unwrap();
    let (gh, other): (Vec<_>, Vec<_>) = manifest
        .members_of(id)
        .partition(|w| w.name.contains("GitHub"));
    (gh[0].id.clone(), other[0].id.clone())
}

/// Record both members as files of one Browsertrix crawl, as an import of a
/// crawl that Browsertrix wrote into two WACZs does.
fn make_one_browsertrix_crawl(home: &Path) {
    let waczs = home.join("index").join("waczs.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&waczs).unwrap()).unwrap();
    for w in v.as_array_mut().unwrap() {
        w["browsertrix"] = serde_json::json!({
            "host": "https://app.browsertrix.com",
            "item_id": "manual-20250125223202-f7a72d33-167",
        });
    }
    std::fs::write(&waczs, serde_json::to_vec(&v).unwrap()).unwrap();
}

const GH_PAGE: &str = "https://github.com/DocNow/hydrator/pull/78/files";

/// A page of a crawl Browsertrix wrote into several WACZs replays from all of
/// them, so a stylesheet fetched into another of the crawl's files is found.
#[tokio::test]
async fn a_crawl_in_several_files_replays_from_all_of_them() {
    let (tmp, id) = home_with_collection();
    let (gh, other) = member_ids(tmp.path(), &id);
    make_one_browsertrix_crawl(tmp.path());
    let indice_lib::server::Routers { app, replay } = routers(tmp.path());

    let (status, html) = get_text(app.clone(), &format!("/crawl/{other}/2025/{GH_PAGE}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains(&format!(r#""source":"/crawl/{other}/replay.json""#)),
        "the viewer replays the crawl's manifest"
    );

    // Served by both origins: the viewer's frame reads it from the replay one.
    for router in [app, replay] {
        let (status, json) = get_json(router.clone(), &format!("/crawl/{other}/replay.json")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["pagesQueryUrl"], format!("/crawl/{other}/pages"));
        // Every file of the crawl, named by its place in the crawl so wabac
        // sorts them in the crawl's order, and read from its own path.
        let resources = json["resources"].as_array().unwrap();
        let mut ids: Vec<&str> = Vec::new();
        for (i, r) in resources.iter().enumerate() {
            let name = r["name"].as_str().unwrap();
            let (place, id) = name.split_once('-').unwrap();
            assert_eq!(place, i.to_string(), "{name}");
            assert_eq!(r["path"].as_str().unwrap(), format!("/files/{id}"));
            ids.push(id);
        }
        let mut sorted = ids.clone();
        sorted.sort();
        let mut want = [gh.as_str(), other.as_str()];
        want.sort();
        assert_eq!(sorted, want, "every file of the crawl");

        // The crawl's lookup finds the page in the crawl's other file, by the
        // name the manifest gives it.
        let gh_name = resources
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .find(|n| n.ends_with(&format!("-{gh}")))
            .unwrap();
        let (status, json) = get_json(router, &format!("/crawl/{other}/pages?url={GH_PAGE}")).await;
        assert_eq!(status, StatusCode::OK);
        let items = json["items"].as_array().unwrap();
        assert!(!items.is_empty(), "the page is in the crawl's other file");
        assert!(items.iter().all(|it| it["filename"] == gh_name), "{json}");
    }
}

/// Any other crawl is one WACZ, and replays it, as it always has. Its lookup
/// answers from that file alone, not the rest of the collection.
#[tokio::test]
async fn a_crawl_in_one_file_replays_that_file() {
    let (tmp, id) = home_with_collection();
    let (_gh, other) = member_ids(tmp.path(), &id);
    let indice_lib::server::Routers { app, .. } = routers(tmp.path());

    let (status, html) = get_text(app.clone(), &format!("/crawl/{other}/2025/{GH_PAGE}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(&format!(r#""source":"/files/{other}""#)));

    let (status, json) = get_json(app.clone(), &format!("/crawl/{other}/replay.json")).await;
    assert_eq!(status, StatusCode::OK);
    let resources = json["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["name"], format!("0-{other}"));

    let (_, json) = get_json(app.clone(), &format!("/crawl/{other}/pages?url={GH_PAGE}")).await;
    assert_eq!(json["total"], 0, "the page is in another crawl");

    let (status, _) = get_text(app, "/crawl/nope/replay.json").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── Streamed Browsertrix crawls: read from presigned URLs ────────────────────

/// A resolver standing in for Browsertrix. A lookup returns a presigned URL,
/// valid for `valid`, for each of `files` that the lookup covers, as
/// Browsertrix's `replay.json` does: a whole public collection, or one
/// authenticated item. It counts how often it is asked.
struct FakeBrowsertrix {
    calls: std::sync::atomic::AtomicUsize,
    files: Vec<indice_lib::collections::Source>,
    valid: std::time::Duration,
    fail: bool,
}

impl FakeBrowsertrix {
    fn new(files: &[indice_lib::collections::Source]) -> Self {
        Self {
            calls: Default::default(),
            files: files.to_vec(),
            valid: std::time::Duration::from_secs(48 * 3600),
            fail: false,
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A source's location but for its file name: what one lookup covers.
fn lookup_of(source: &indice_lib::collections::Source) -> String {
    let location = source.location();
    location.rsplit_once('|').unwrap().0.to_string()
}

fn public_source(resource: &str) -> indice_lib::collections::Source {
    indice_lib::collections::Source::BrowsertrixPublic {
        host: "https://bt.example".into(),
        org: "o".into(),
        collection: "c".into(),
        resource: resource.into(),
    }
}

fn item_source(item: &str, resource: &str) -> indice_lib::collections::Source {
    indice_lib::collections::Source::Browsertrix {
        host: "https://bt.example".into(),
        org: "o".into(),
        item: item.into(),
        resource: resource.into(),
    }
}

fn presigned(source: &indice_lib::collections::Source) -> String {
    let file = source.location().rsplit_once('|').unwrap().1.to_string();
    format!("https://s3.example/{file}?X-Amz-Signature=sig")
}

impl indice_lib::index::SourceResolver for FakeBrowsertrix {
    fn resolve(&self, _: &indice_lib::collections::Source) -> anyhow::Result<String> {
        unreachable!("replay asks for every URL at once")
    }
    fn resolve_all(
        &self,
        source: &indice_lib::collections::Source,
    ) -> anyhow::Result<Vec<indice_lib::index::Resolved>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        anyhow::ensure!(!self.fail, "Browsertrix is down");
        let expires = Some(std::time::SystemTime::now() + self.valid);
        Ok(self
            .files
            .iter()
            .filter(|f| lookup_of(f) == lookup_of(source))
            .map(|f| indice_lib::index::Resolved {
                source: f.clone(),
                url: presigned(f),
                expires,
            })
            .collect())
    }
}

/// Make the collection's two members streamed Browsertrix files, from
/// `sources` in order, as `indice import browsertrix --stream` records them.
/// Returns each member's id with its source.
fn make_streamed(
    home: &Path,
    sources: [indice_lib::collections::Source; 2],
) -> Vec<(String, indice_lib::collections::Source)> {
    let waczs = home.join("index").join("waczs.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&waczs).unwrap()).unwrap();
    let mut out = Vec::new();
    for (w, source) in v.as_array_mut().unwrap().iter_mut().zip(sources) {
        w["source"] = serde_json::json!(source.location());
        out.push((w["id"].as_str().unwrap().to_string(), source));
    }
    std::fs::write(&waczs, serde_json::to_vec(&v).unwrap()).unwrap();
    out
}

/// Both members as files of one public Browsertrix crawl.
fn make_one_streamed_crawl(home: &Path) -> Vec<(String, indice_lib::collections::Source)> {
    make_one_browsertrix_crawl(home);
    make_streamed(
        home,
        [public_source("part-0.wacz"), public_source("part-1.wacz")],
    )
}

fn router_resolving(home: &Path, resolver: std::sync::Arc<FakeBrowsertrix>) -> axum::Router {
    let roster = home.join("users.yaml");
    if !roster.exists() {
        std::fs::write(&roster, "users: []\n").unwrap();
    }
    let mut config = indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "X-Forwarded-Email",
        "test-secret-not-sent",
    ));
    config.set_site_url("http://indice.test").unwrap();
    config.replay_origin =
        Some(indice_lib::server::ReplayOrigin::parse("http://replay.indice.test").unwrap());
    indice_lib::server::router_with_resolver(home, Some(resolver), config).unwrap()
}

/// The paths a manifest lists, by WACZ id.
fn paths_by_id(json: &serde_json::Value) -> std::collections::HashMap<String, String> {
    json["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let name = r["name"].as_str().unwrap();
            let id = name.split_once('-').map_or(name, |(_, id)| id);
            (id.to_string(), r["path"].as_str().unwrap().to_string())
        })
        .collect()
}

async fn get_response(app: axum::Router, uri: &str) -> axum::response::Response {
    app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// A streamed Browsertrix crawl's manifest sends wabac straight to each
/// file's presigned URL, sparing every range read a redirect through
/// `/files/{id}`, and one lookup gives the URLs of all the files. The
/// manifest isn't cached, since the URLs expire.
#[tokio::test]
async fn a_streamed_crawl_replays_straight_from_its_presigned_urls() {
    let (tmp, id) = home_with_collection();
    let files = make_one_streamed_crawl(tmp.path());
    let sources: Vec<_> = files.iter().map(|(_, s)| s.clone()).collect();
    let bt = std::sync::Arc::new(FakeBrowsertrix::new(&sources));
    let app = router_resolving(tmp.path(), bt.clone());

    let crawl = &files[0].0;
    let resp = get_response(app.clone(), &format!("/crawl/{crawl}/replay.json")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let paths = paths_by_id(&serde_json::from_slice(&bytes).unwrap());
    for (wacz, source) in &files {
        assert_eq!(paths[wacz], presigned(source));
    }
    assert_eq!(bt.calls(), 1, "one lookup for both files");

    // The collection's manifest, from the same cache.
    let (_, json) = get_json(app.clone(), &format!("/collection/{id}/replay.json")).await;
    let paths = paths_by_id(&json);
    for (wacz, source) in &files {
        assert_eq!(paths[wacz], presigned(source));
    }
    // `/files/{id}` still redirects, for anything that asks it.
    let resp = get_response(app, &format!("/files/{crawl}")).await;
    assert_eq!(resp.headers()["location"], presigned(&files[0].1));
    assert_eq!(bt.calls(), 1, "all from the cache");
}

/// A streamed crawl of one file replays through its manifest too, so it is
/// also read from its presigned URL.
#[tokio::test]
async fn a_streamed_crawl_of_one_file_replays_through_its_manifest() {
    let (tmp, _id) = home_with_collection();
    let files = make_streamed(
        tmp.path(),
        [public_source("part-0.wacz"), public_source("part-1.wacz")],
    );
    let sources: Vec<_> = files.iter().map(|(_, s)| s.clone()).collect();
    let app = router_resolving(
        tmp.path(),
        std::sync::Arc::new(FakeBrowsertrix::new(&sources)),
    );

    let (crawl, source) = &files[0];
    let (_, html) = get_text(app.clone(), &format!("/crawl/{crawl}/2025/{GH_PAGE}")).await;
    assert!(
        html.contains(&format!(r#""source":"/crawl/{crawl}/replay.json""#)),
        "{html}"
    );
    let (_, json) = get_json(app, &format!("/crawl/{crawl}/replay.json")).await;
    let paths = paths_by_id(&json);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[crawl], presigned(source));
}

/// Crawls imported with credentials are each a Browsertrix item of their own,
/// a lookup each. A crawl's manifest makes only its own. A collection's,
/// which may span hundreds, makes none: it lists the URLs already cached and
/// `/files/{id}` for the rest, which resolves a file, and its item, when wabac
/// opens it.
#[tokio::test]
async fn a_collection_of_items_makes_no_lookups_for_its_manifest() {
    let (tmp, id) = home_with_collection();
    let files = make_streamed(
        tmp.path(),
        [
            item_source("item-1", "a.wacz"),
            item_source("item-2", "b.wacz"),
        ],
    );
    let sources: Vec<_> = files.iter().map(|(_, s)| s.clone()).collect();
    let bt = std::sync::Arc::new(FakeBrowsertrix::new(&sources));
    let app = router_resolving(tmp.path(), bt.clone());
    let [(first, first_source), (second, second_source)] = &files[..] else {
        unreachable!()
    };

    let (_, json) = get_json(app.clone(), &format!("/collection/{id}/replay.json")).await;
    let paths = paths_by_id(&json);
    assert_eq!(paths[first], format!("/files/{first}"));
    assert_eq!(paths[second], format!("/files/{second}"));
    assert_eq!(bt.calls(), 0, "the collection's manifest looked nothing up");

    let (_, json) = get_json(app.clone(), &format!("/crawl/{first}/replay.json")).await;
    assert_eq!(paths_by_id(&json)[first], presigned(first_source));
    assert_eq!(bt.calls(), 1, "the crawl looked up its own item");

    // wabac opens the other crawl's file through `/files/{id}`.
    let resp = get_response(app.clone(), &format!("/files/{second}")).await;
    assert_eq!(resp.headers()["location"], presigned(second_source));
    let (_, json) = get_json(app, &format!("/collection/{id}/replay.json")).await;
    let paths = paths_by_id(&json);
    assert_eq!(paths[first], presigned(first_source));
    assert_eq!(paths[second], presigned(second_source));
    assert_eq!(bt.calls(), 2);
}

/// A URL past its time is not handed out again: the next manifest asks
/// Browsertrix afresh. And when Browsertrix can't be reached, the manifest
/// still answers, with `/files/{id}` for the crawl's files, after one call.
#[tokio::test]
async fn a_manifest_never_hands_out_an_expired_url() {
    let (tmp, id) = home_with_collection();
    let files = make_one_streamed_crawl(tmp.path());
    let sources: Vec<_> = files.iter().map(|(_, s)| s.clone()).collect();
    let crawl = &files[0].0;

    let expired = std::sync::Arc::new(FakeBrowsertrix {
        valid: std::time::Duration::ZERO,
        ..FakeBrowsertrix::new(&sources)
    });
    let app = router_resolving(tmp.path(), expired.clone());
    for round in 1..=2 {
        let (status, json) = get_json(app.clone(), &format!("/crawl/{crawl}/replay.json")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(paths_by_id(&json)[crawl], presigned(&files[0].1));
        assert_eq!(expired.calls(), round, "asked afresh each time");
    }

    let down = std::sync::Arc::new(FakeBrowsertrix {
        fail: true,
        ..FakeBrowsertrix::new(&sources)
    });
    let app = router_resolving(tmp.path(), down.clone());
    let (status, json) = get_json(app.clone(), &format!("/crawl/{crawl}/replay.json")).await;
    assert_eq!(status, StatusCode::OK);
    for (wacz, _) in &files {
        assert_eq!(paths_by_id(&json)[wacz], format!("/files/{wacz}"));
    }
    assert_eq!(down.calls(), 1, "not asked again for the other file");
    // A collection's manifest doesn't wait on Browsertrix at all.
    let (status, _) = get_json(app, &format!("/collection/{id}/replay.json")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(down.calls(), 1);
}
