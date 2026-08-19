//! Resumable-ingest behavior: re-running `index` skips sources already indexed
//! into the collection (so a large, interrupted ingest can be safely re-run to
//! pick up where it left off), while `--force` re-indexes them.

use std::path::Path;
use std::sync::Mutex;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// An `IndexProgress` that records the `phase(...)` messages, so a test can
/// observe whether a source was skipped.
#[derive(Default)]
struct PhaseRecorder {
    phases: Mutex<Vec<String>>,
}

impl indice_lib::index::IndexProgress for PhaseRecorder {
    fn begin(&self, _: &str) {}
    fn phase(&self, p: &str) {
        self.phases.lock().unwrap().push(p.to_string());
    }
    fn set_total(&self, _: u64) {}
    fn set_records(&self, _: u64) {}
    fn wacz_indexed(&self, _: &str, _: u64) {}
    fn finish(&self) {}
}

impl PhaseRecorder {
    fn skipped(&self) -> bool {
        self.phases
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("skipping already-indexed"))
    }
}

fn crawl_count(home: &Path) -> usize {
    indice_lib::collections::Manifest::open(&indice_lib::index::index_dir(home))
        .unwrap()
        .waczs
        .len()
}

fn index(home: &Path, src: &str, force: bool, prog: Option<&dyn indice_lib::index::IndexProgress>) {
    indice_lib::index::index_location(src, home, Some("S"), "c", false, force, None, prog).unwrap();
}

#[test]
fn reindex_swaps_cleanly_and_leaves_no_swap_dirs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let input = home.join("in.wacz");
    std::fs::copy(Path::new(FIXTURES).join("simple.wacz"), &input).unwrap();
    let src = input.to_string_lossy().into_owned();

    index(home, &src, false, None);
    let before = crawl_count(home);
    assert!(before > 0, "fixture registers a crawl");

    // Pre-seed leftover swap dirs as if a previous reindex was interrupted;
    // reindex must reconcile them away rather than trip over them.
    let idx = indice_lib::index::index_dir(home);
    std::fs::create_dir_all(idx.join("full_text.new")).unwrap();
    std::fs::create_dir_all(idx.join("full_text.old")).unwrap();

    indice_lib::index::reindex(home, None, None, None).unwrap();

    assert_eq!(crawl_count(home), before, "collection membership preserved");
    assert!(
        idx.join("full_text").exists(),
        "live index present after swap"
    );
    assert!(
        !idx.join("full_text.new").exists(),
        "no leftover .new after a clean reindex"
    );
    assert!(
        !idx.join("full_text.old").exists(),
        "no leftover .old after a clean reindex"
    );

    // The swapped-in index is real and searchable.
    let si = indice_lib::search::SearchIndex::open(&idx.join("full_text")).unwrap();
    assert!(si.num_docs().unwrap() > 0, "reindexed index has documents");
}

#[test]
fn rerun_skips_already_indexed_unless_forced() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let input = home.join("in.wacz");
    std::fs::copy(Path::new(FIXTURES).join("simple.wacz"), &input).unwrap();
    let src = input.to_string_lossy().into_owned();

    // First ingest registers the crawl.
    index(home, &src, false, None);
    assert_eq!(crawl_count(home), 1);

    // Re-running the same ingest skips the already-indexed source (this is what
    // makes an interrupted large ingest resumable — the finished crawls persist,
    // committed per WACZ, and a re-run steps over them).
    let rec = PhaseRecorder::default();
    index(home, &src, false, Some(&rec));
    assert!(rec.skipped(), "re-run skips the already-indexed source");
    assert_eq!(crawl_count(home), 1, "no duplicate");

    // --force re-indexes it (no skip).
    let rec2 = PhaseRecorder::default();
    index(home, &src, true, Some(&rec2));
    assert!(!rec2.skipped(), "--force re-indexes rather than skipping");
    assert_eq!(crawl_count(home), 1, "still one crawl (upsert)");
}

// The resume-skip is scoped to the target collection (`w.collection == group.0`)
// so a genuine reassignment to a different collection isn't silently swallowed.
// Its only *behavioral* difference from an unscoped skip is for a URL source
// re-indexed into another collection — a local file is refused upstream, or gets
// its own per-collection copy + id, so its skip never collides across
// collections. Exercising the URL case needs a network fetch, so it isn't
// unit-tested here; `rerun_skips_already_indexed_unless_forced` above guards
// that the same-collection resume path still works after the scoping change.
