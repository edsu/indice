//! Integration tests for deleting crawls and collections. Index a fixture WACZ,
//! delete it, and confirm it is gone from the search index, the manifest, and
//! disk — and that a non-empty collection is refused without `--with-crawls`.

use std::path::{Path, PathBuf};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn index_dir(home: &Path) -> PathBuf {
    indice_lib::index::index_dir(home)
}

/// Index a private copy of the fixture (never the repo file) into `collection`,
/// returning the new crawl's id.
fn index_fixture(home: &Path, collection: &str) -> String {
    let input = home.join(format!("input-{collection}.wacz"));
    std::fs::copy(Path::new(FIXTURES).join("simple.wacz"), &input).unwrap();
    indice_lib::index::index_location(
        &input.to_string_lossy(),
        home,
        Some("Simple"),
        collection,
        false,
        false,
        None,
        None,
    )
    .unwrap();
    let manifest = indice_lib::collections::Manifest::open(&index_dir(home)).unwrap();
    let id = manifest
        .members_of(&indice_lib::collections::slugify(collection))
        .next()
        .expect("a crawl was indexed")
        .id
        .clone();
    id
}

fn search_hits(home: &Path, q: &str) -> usize {
    let ft = index_dir(home).join("full_text");
    indice_lib::search::SearchIndex::open_read_only(&ft)
        .unwrap()
        .search(q, 50)
        .unwrap()
        .len()
}

#[test]
fn delete_crawl_removes_docs_manifest_and_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let id = index_fixture(home, "c");

    // Precondition: one crawl in the manifest, its WACZ on disk, search finds it.
    let manifest = indice_lib::collections::Manifest::open(&index_dir(home)).unwrap();
    assert_eq!(manifest.waczs.len(), 1);
    let file = manifest.waczs[0].source.resolve(home).unwrap();
    assert!(file.exists(), "indexed WACZ is filed on disk");
    assert!(
        search_hits(home, "example") > 0,
        "search finds it before delete"
    );

    // Delete.
    let plan = indice_lib::index::delete_crawl(home, &id).unwrap();
    assert_eq!(plan.id, id);
    assert!(
        plan.local_file.is_some(),
        "a local File source is scheduled for removal"
    );
    assert!(plan.last_in_collection, "it was the only member");

    // Gone from the manifest, disk, and search.
    let manifest = indice_lib::collections::Manifest::open(&index_dir(home)).unwrap();
    assert!(manifest.waczs.is_empty(), "manifest entry removed");
    assert!(!file.exists(), "local WACZ deleted");
    assert_eq!(search_hits(home, "example"), 0, "search no longer finds it");

    // Deleting a now-unknown id is a clear error (not a silent no-op).
    assert!(indice_lib::index::delete_crawl(home, &id).is_err());
}

#[test]
fn delete_crawl_keeps_a_file_referenced_by_another_entry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let id1 = index_fixture(home, "c");

    // Craft a second manifest entry that shares the *same* WACZ file (a different
    // id + collection, identical `source`). The public index API refuses
    // re-collecting one file, so edit waczs.json directly to set up the case.
    let waczs_path = index_dir(home).join("waczs.json");
    let mut entries: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&waczs_path).unwrap()).unwrap();
    let mut dup = entries[0].clone();
    dup["id"] = serde_json::json!("dup00001");
    dup["collection"] = serde_json::json!("d");
    entries.push(dup);
    std::fs::write(&waczs_path, serde_json::to_string_pretty(&entries).unwrap()).unwrap();

    let file = indice_lib::collections::Manifest::open(&index_dir(home))
        .unwrap()
        .wacz_by_id(&id1)
        .unwrap()
        .source
        .resolve(home)
        .unwrap();
    assert!(file.exists());

    // Deleting the first entry must NOT remove the shared file.
    let plan = indice_lib::index::delete_crawl(home, &id1).unwrap();
    assert!(
        plan.local_file.is_none(),
        "a file shared with another entry is not scheduled for removal"
    );
    assert!(file.exists(), "the shared WACZ file is preserved");

    // The other entry survives and still resolves to the (still-present) file.
    let manifest = indice_lib::collections::Manifest::open(&index_dir(home)).unwrap();
    assert!(manifest.wacz_by_id(&id1).is_none(), "deleted entry is gone");
    let other = manifest
        .wacz_by_id("dup00001")
        .expect("shared entry remains");
    assert!(other.source.resolve(home).unwrap().exists());
}

#[test]
fn delete_collection_refuses_nonempty_without_flag_then_deletes_with_it() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    index_fixture(home, "coll");
    let coll_id = indice_lib::collections::slugify("coll");

    // A non-empty collection is refused without --with-crawls, and left intact.
    let err = indice_lib::index::delete_collection(home, &coll_id, false)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("with-crawls"),
        "refusal names the flag; got: {err}"
    );
    assert_eq!(
        indice_lib::collections::Manifest::open(&index_dir(home))
            .unwrap()
            .waczs
            .len(),
        1,
        "the crawl is untouched by the refused delete"
    );

    // With the flag, members and the grouping are removed.
    let plan = indice_lib::index::delete_collection(home, &coll_id, true).unwrap();
    assert_eq!(plan.crawls_deleted.len(), 1, "the one member was deleted");
    let manifest = indice_lib::collections::Manifest::open(&index_dir(home)).unwrap();
    assert!(manifest.waczs.is_empty(), "member crawl deleted");
    assert!(
        manifest.collection_by_id(&coll_id).is_none(),
        "grouping removed"
    );
    assert_eq!(
        search_hits(home, "example"),
        0,
        "member gone from search too"
    );
}
