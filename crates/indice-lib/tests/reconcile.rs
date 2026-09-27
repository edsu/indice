//! Integration tests for reconciling the Tantivy index against the manifest.
//!
//! The state under test is the one no lock can prevent: an ingest commits its
//! documents, and the manifest save never happens because the process died in
//! between. Each test reproduces that end state directly, by indexing a fixture
//! and then removing the manifest entry while leaving the documents alone,
//! which is what a crash leaves behind.

use std::path::{Path, PathBuf};

use indice_lib::collections::Manifest;
use indice_lib::index::{missing_remedy, orphan_remedy, reconcile, unrecoverable_remedy, Outcome};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn index_dir(home: &Path) -> PathBuf {
    indice_lib::index::index_dir(home)
}

fn no_progress() -> &'static dyn indice_lib::index::IndexProgress {
    indice_lib::index::no_progress()
}

/// `home` as the report prints it: canonical, so a pasted command works from
/// any directory. On macOS this is the difference between `/var/...` and
/// `/private/var/...`.
fn canonical(path: &Path) -> String {
    path.canonicalize().unwrap().display().to_string()
}

/// Reconcile a home that is expected to have an index.
fn checked(home: &Path) -> indice_lib::index::Reconciliation {
    match reconcile(home, no_progress()).unwrap() {
        Outcome::Checked(report) => report,
        Outcome::NoIndex => panic!("expected an initialized index at {}", home.display()),
    }
}

/// Index a private copy of the fixture into `collection`, returning its id.
fn index_fixture(home: &Path, collection: &str) -> String {
    let input = home.join(format!("input-{collection}.wacz"));
    std::fs::copy(Path::new(FIXTURES).join("simple.wacz"), &input).unwrap();
    indice_lib::index::Ingest::new(home)
        .name(Some("Simple"))
        .index_location(&input.to_string_lossy(), collection)
        .unwrap();
    Manifest::open(&index_dir(home))
        .unwrap()
        .members_of(&indice_lib::collections::slugify(collection))
        .next()
        .expect("a crawl was indexed")
        .id
        .clone()
}

/// Drop `crawl_id` from the manifest and save, leaving its documents in the
/// index. This is precisely the state a crash between the Tantivy commit and
/// `manifest.save()` leaves behind.
fn simulate_crash_before_manifest_save(home: &Path, crawl_id: &str) {
    let mut manifest = Manifest::open(&index_dir(home)).unwrap();
    manifest.waczs.retain(|w| w.id != crawl_id);
    manifest.save().unwrap();
}

#[test]
fn a_healthy_archive_reconciles_clean() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    index_fixture(home, "c");

    let report = checked(home);
    assert!(
        report.is_consistent(),
        "nothing to report on a healthy archive: {report:?}"
    );
    assert_eq!(report.in_manifest, 1);
    assert_eq!(report.in_index, 1);
}

#[test]
fn an_orphaned_crawl_is_found_and_its_file_located() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let id = index_fixture(home, "c");
    simulate_crash_before_manifest_save(home, &id);

    let report = checked(home);
    assert!(!report.is_consistent());
    assert_eq!(
        report.missing.len(),
        0,
        "nothing is missing, one is orphaned"
    );
    assert_eq!(report.orphans.len(), 1, "{report:?}");

    let orphan = &report.orphans[0];
    assert_eq!(orphan.crawl_id, id);
    assert!(
        orphan.live_pages > 0,
        "the documents a search would still turn up"
    );

    // The payoff of ids being derived from the location string rather than the
    // contents: the file is still under archive/, so the crawl is recoverable
    // and the remedy is a re-index rather than discarding the documents.
    let source = orphan
        .source
        .as_ref()
        .expect("the WACZ is still under archive/, so it must be located");
    let on_disk = source.resolve(home).expect("a local file");
    assert!(on_disk.exists(), "{} should exist", on_disk.display());
    // The command has to be runnable as printed. The collection comes off the
    // orphan's own documents, because the manifest entry that would otherwise
    // say which collection it belonged to is the thing that went missing.
    let remedy = orphan_remedy(home, orphan).expect("a file on disk means a command");
    assert_eq!(orphan.collection.as_deref(), Some("c"));
    assert!(remedy.contains("--force"), "{remedy}");
    assert!(remedy.contains("--collection 'c'"), "{remedy}");
    assert!(
        !remedy.contains("<collection>"),
        "no placeholder left for the curator to guess at: {remedy}"
    );
    // Without --home the command only works from inside the archive, and the
    // path it names is home-relative, so pasting it anywhere else fails.
    assert!(
        remedy.contains(&format!("--home '{}'", canonical(home))),
        "the command has to name the archive: {remedy}"
    );
    // `--home` alone is not enough: `index` resolves its location argument
    // against the current directory, not against home, so a home-relative
    // path would find the right archive and the wrong file.
    assert!(
        remedy.contains(&canonical(&on_disk)),
        "the location has to be absolute: {remedy}"
    );
}

#[test]
fn an_orphan_whose_file_is_gone_reports_no_source() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let id = index_fixture(home, "c");

    // Take the file away as well, which is the case where the documents are
    // the last copy of the crawl and re-indexing cannot help.
    let source = Manifest::open(&index_dir(home))
        .unwrap()
        .wacz_by_id(&id)
        .expect("indexed")
        .source
        .clone();
    std::fs::remove_file(source.resolve(home).unwrap()).unwrap();
    simulate_crash_before_manifest_save(home, &id);

    let report = checked(home);
    assert_eq!(report.orphans.len(), 1);
    assert!(
        report.orphans[0].source.is_none(),
        "no file hashes to this id any more"
    );
    assert!(
        orphan_remedy(home, &report.orphans[0]).is_none(),
        "there is no re-index command when nothing on disk hashes to the id"
    );
    // `indice crawl delete` cannot clear it either: that plans from the
    // manifest entry, which is the thing that went missing. A rebuild can,
    // but only if the manifest still lists something to rebuild from, and
    // here it does not.
    assert!(
        unrecoverable_remedy(home, &report).is_none(),
        "a rebuild with nothing registered exits early and would leave the \
         orphan where it is, so offering it would be bad advice"
    );
}

#[test]
fn a_deleted_crawl_is_not_an_orphan() {
    // The false positive that would make this pass useless. A proper delete
    // removes the manifest entry *and* the documents, but Tantivy keeps the
    // crawl's term in the segment dictionary until a merge, so a pass that
    // counted terms rather than live documents would report every deleted
    // crawl as damage.
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let id = index_fixture(home, "c");
    indice_lib::index::delete_crawl(home, &id).unwrap();

    let report = checked(home);
    assert!(
        report.is_consistent(),
        "a deleted crawl left nothing to reconcile: {report:?}"
    );
}

#[test]
fn a_manifest_entry_without_documents_is_reported_as_confirmed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let id = index_fixture(home, "c");

    // Lose the documents while keeping the entry: the reverse direction, from
    // a commit that never landed.
    let ft = index_dir(home).join("full_text");
    let mut search = indice_lib::search::SearchIndex::open(&ft).unwrap();
    search.delete_crawl_docs(&id);
    search.commit().unwrap();
    drop(search);

    let report = checked(home);
    assert_eq!(report.orphans.len(), 0);
    assert_eq!(report.missing.len(), 1, "{report:?}");

    let missing = &report.missing[0];
    assert_eq!(missing.crawl_id, id);
    assert!(
        missing.is_confirmed(),
        "the manifest recorded {:?} pages, so their absence is real damage \
         rather than a crawl that never had any",
        missing.recorded_pages
    );
    let remedy = missing_remedy(home, missing);
    assert!(remedy.contains("--force"), "{remedy}");
    // The manifest knows the collection for certain in this direction, so a
    // placeholder here would be worse than in the orphan case.
    assert!(remedy.contains("--collection 'c'"), "{remedy}");
    assert!(!remedy.contains("<collection>"), "{remedy}");
}

#[test]
fn an_entry_that_never_had_pages_is_reported_but_not_confirmed() {
    // Zero documents is not damage on its own, and calling it damage would
    // send a curator looking for a crawl that was always empty.
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let id = index_fixture(home, "c");

    let ft = index_dir(home).join("full_text");
    let mut search = indice_lib::search::SearchIndex::open(&ft).unwrap();
    search.delete_crawl_docs(&id);
    search.commit().unwrap();
    drop(search);

    let mut manifest = Manifest::open(&index_dir(home)).unwrap();
    manifest
        .waczs
        .iter_mut()
        .find(|w| w.id == id)
        .unwrap()
        .page_count = None;
    manifest.save().unwrap();

    let report = checked(home);
    assert_eq!(report.missing.len(), 1);
    assert!(
        !report.missing[0].is_confirmed(),
        "without a recorded page count there is nothing to contradict"
    );
    assert_eq!(
        report.confirmed_findings(),
        0,
        "so an exit code keyed off confirmed findings stays quiet"
    );
}

#[test]
fn an_unrecoverable_orphan_beside_a_live_crawl_is_cleared_by_a_rebuild() {
    // The other half of the case above. With something still registered, a
    // rebuild writes a fresh index from the manifest and leaves unreferenced
    // documents behind, so there is a command worth printing.
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let doomed = index_fixture(home, "c");
    index_fixture(home, "keep");

    let source = Manifest::open(&index_dir(home))
        .unwrap()
        .wacz_by_id(&doomed)
        .expect("indexed")
        .source
        .clone();
    std::fs::remove_file(source.resolve(home).unwrap()).unwrap();
    simulate_crash_before_manifest_save(home, &doomed);

    let report = checked(home);
    assert_eq!(report.orphans.len(), 1);
    assert!(report.orphans[0].source.is_none());
    let clear = unrecoverable_remedy(home, &report).expect("one crawl is still registered");
    assert!(clear.contains("reindex"), "{clear}");
    assert!(
        clear.contains(&format!("--home '{}'", canonical(home))),
        "{clear}"
    );
}

#[test]
fn a_home_with_no_index_is_not_reported_as_agreeing() {
    // Running this from the wrong directory must not claim the stores agree,
    // and must not bring an archive into being. Both the lock and opening the
    // index would create `index/` if reached.
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();

    match reconcile(home, no_progress()).unwrap() {
        Outcome::NoIndex => {}
        Outcome::Checked(report) => panic!("claimed a verdict on a non-archive: {report:?}"),
    }
    assert!(
        !index_dir(home).exists(),
        "a read-only pass must not create {}",
        index_dir(home).display()
    );
    assert_eq!(
        std::fs::read_dir(home).unwrap().count(),
        0,
        "nothing at all should have been written"
    );
}

#[test]
fn a_path_holding_a_quote_still_produces_one_shell_argument() {
    // A single-quoted path breaks on an apostrophe, which turns a paste into a
    // command targeting something else entirely.
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let input = home.join("o'brien-2024.wacz");
    std::fs::copy(Path::new(FIXTURES).join("simple.wacz"), &input).unwrap();
    indice_lib::index::Ingest::new(home)
        .name(Some("Quoted"))
        .index_location(&input.to_string_lossy(), "c")
        .unwrap();
    let id = Manifest::open(&index_dir(home))
        .unwrap()
        .waczs
        .first()
        .unwrap()
        .id
        .clone();
    simulate_crash_before_manifest_save(home, &id);

    let report = checked(home);
    let remedy = orphan_remedy(home, &report.orphans[0]).expect("the file is on disk");
    assert!(remedy.contains("brien-2024.wacz"), "{remedy}");
    assert!(
        remedy.contains(r"o'\''brien"),
        "the apostrophe has to be escaped for the shell, not left to split the \
         argument: {remedy}"
    );
}
