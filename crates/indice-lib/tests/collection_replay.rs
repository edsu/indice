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
        let mut names: Vec<&str> = json["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let name = r["name"].as_str().unwrap();
                assert_eq!(r["path"].as_str().unwrap(), format!("/files/{name}"));
                name
            })
            .collect();
        names.sort();
        let mut want = [gh.as_str(), other.as_str()];
        want.sort();
        assert_eq!(names, want, "every file of the crawl");
        assert_eq!(json["pagesQueryUrl"], format!("/crawl/{other}/pages"));

        // The crawl's lookup finds the page in the crawl's other file.
        let (status, json) = get_json(router, &format!("/crawl/{other}/pages?url={GH_PAGE}")).await;
        assert_eq!(status, StatusCode::OK);
        let items = json["items"].as_array().unwrap();
        assert!(!items.is_empty(), "the page is in the crawl's other file");
        assert!(items.iter().all(|it| it["filename"] == gh.as_str()));
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
    assert_eq!(resources[0]["name"], other.as_str());

    let (_, json) = get_json(app.clone(), &format!("/crawl/{other}/pages?url={GH_PAGE}")).await;
    assert_eq!(json["total"], 0, "the page is in another crawl");

    let (status, _) = get_text(app, "/crawl/nope/replay.json").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
