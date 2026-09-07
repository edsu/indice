//! Atomic index-swap trio used by `reindex`: derive the sibling swap paths,
//! reconcile leftovers from an interrupted run, and promote a freshly-built
//! index into place. `pub(super)` — used by `reindex` and by co-located tests.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The live full-text index and the two sibling paths `reindex` uses to swap a
/// freshly-built index in atomically: `full_text.new` (the in-progress build)
/// and `full_text.old` (the previous index, parked briefly during the swap).
pub(super) fn index_swap_paths(index_dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        index_dir.join("full_text"),
        index_dir.join("full_text.new"),
        index_dir.join("full_text.old"),
    )
}

/// Clear leftover swap directories from a `reindex` that was interrupted (crash,
/// kill, disk-full) before it finished. Recovers the live index if the crash
/// landed in the small window between the two renames of a swap (`full_text`
/// gone, previous index still parked as `full_text.old`), then discards any
/// partial `full_text.new` build. A no-op when there's nothing to reconcile.
pub(super) fn reconcile_index_swap(index_dir: &Path) -> Result<()> {
    let (full_text, new_dir, old_dir) = index_swap_paths(index_dir);
    // Crash mid-swap: the old index was moved aside but the new one wasn't
    // promoted yet, so the live path is missing — restore the previous index.
    if !full_text.exists() && old_dir.exists() {
        std::fs::rename(&old_dir, &full_text)
            .with_context(|| format!("restoring index from {}", old_dir.display()))?;
    }
    // Otherwise the live index is authoritative; drop any stale parked copy.
    if old_dir.exists() {
        std::fs::remove_dir_all(&old_dir)
            .with_context(|| format!("removing stale {}", old_dir.display()))?;
    }
    // A leftover `.new` is always an incomplete build from an interrupted run.
    if new_dir.exists() {
        std::fs::remove_dir_all(&new_dir)
            .with_context(|| format!("removing partial {}", new_dir.display()))?;
    }
    Ok(())
}

/// Promote a fully-built `full_text.new` to the live `full_text`. Directories
/// can't be renamed onto a non-empty target, so this is two renames: park the
/// old index as `full_text.old`, move the new one into place, then delete the
/// old — each rename is atomic on a single filesystem, so the new index is
/// never half-written over the old. A crash between the two renames is
/// recovered by [`reconcile_index_swap`] on the next run.
pub(super) fn swap_in_new_index(index_dir: &Path) -> Result<()> {
    let (full_text, new_dir, old_dir) = index_swap_paths(index_dir);
    if full_text.exists() {
        std::fs::rename(&full_text, &old_dir)
            .with_context(|| format!("parking old index as {}", old_dir.display()))?;
    }
    std::fs::rename(&new_dir, &full_text)
        .with_context(|| format!("promoting {} to the live index", new_dir.display()))?;
    if old_dir.exists() {
        std::fs::remove_dir_all(&old_dir)
            .with_context(|| format!("removing replaced index {}", old_dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a directory standing in for a Tantivy index, with a marker file
    /// whose contents identify which build it is.
    fn fake_index(dir: &Path, tag: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("marker"), tag).unwrap();
    }
    fn marker(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("marker")).unwrap()
    }
    #[test]
    fn swap_promotes_new_index_and_clears_siblings() {
        let tmp = TempDir::new().unwrap();
        let (full_text, new_dir, old_dir) = index_swap_paths(tmp.path());
        fake_index(&full_text, "old");
        fake_index(&new_dir, "new");

        swap_in_new_index(tmp.path()).unwrap();

        assert_eq!(marker(&full_text), "new", "new build is now live");
        assert!(!new_dir.exists(), ".new consumed by the swap");
        assert!(!old_dir.exists(), ".old cleaned up after the swap");
    }
    #[test]
    fn swap_works_with_no_prior_live_index() {
        // First-ever reindex: no `full_text` yet, just the fresh build.
        let tmp = TempDir::new().unwrap();
        let (full_text, new_dir, old_dir) = index_swap_paths(tmp.path());
        fake_index(&new_dir, "new");

        swap_in_new_index(tmp.path()).unwrap();

        assert_eq!(marker(&full_text), "new");
        assert!(!new_dir.exists());
        assert!(!old_dir.exists());
    }
    #[test]
    fn reconcile_discards_partial_build_keeps_live_index() {
        // Crash before the swap: a partial `.new` lingers, live index intact.
        let tmp = TempDir::new().unwrap();
        let (full_text, new_dir, old_dir) = index_swap_paths(tmp.path());
        fake_index(&full_text, "live");
        fake_index(&new_dir, "partial");

        reconcile_index_swap(tmp.path()).unwrap();

        assert_eq!(marker(&full_text), "live", "live index untouched");
        assert!(!new_dir.exists(), "partial build discarded");
        assert!(!old_dir.exists());
    }
    #[test]
    fn reconcile_recovers_live_index_from_mid_swap_crash() {
        // Crash between the two renames: `full_text` gone, previous index parked
        // as `.old`. Recovery restores it (and drops any lingering `.new`).
        let tmp = TempDir::new().unwrap();
        let (full_text, new_dir, old_dir) = index_swap_paths(tmp.path());
        fake_index(&old_dir, "recovered");
        fake_index(&new_dir, "partial");

        reconcile_index_swap(tmp.path()).unwrap();

        assert_eq!(marker(&full_text), "recovered", "old index restored");
        assert!(!new_dir.exists());
        assert!(!old_dir.exists());
    }
    #[test]
    fn reconcile_drops_stale_old_when_live_index_present() {
        // A `.old` lingering next to a healthy live index (crash right after the
        // second rename, before cleanup) is simply discarded.
        let tmp = TempDir::new().unwrap();
        let (full_text, _new_dir, old_dir) = index_swap_paths(tmp.path());
        fake_index(&full_text, "live");
        fake_index(&old_dir, "stale");

        reconcile_index_swap(tmp.path()).unwrap();

        assert_eq!(marker(&full_text), "live");
        assert!(!old_dir.exists(), "stale parked copy removed");
    }
}
