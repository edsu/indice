//! Search-index compaction (segment merge) and fragmentation reporting.

use std::path::Path;

use anyhow::{Context, Result};

use super::paths::index_dir;
use super::IndexProgress;

/// Compact the search index in place by merging segments toward
/// `target_segments` — **without** re-streaming any sources, so it's far
/// cheaper than [`reindex`] when the index has fragmented into many small
/// segments (e.g. after ingest merges failed on a full disk). A search fans out
/// across every segment, so fewer segments = faster queries. `target_segments`
/// (≥ 1) trades compaction against peak transient disk (~index_size / target).
/// Returns `(segments_before, segments_after)`.
pub fn optimize(
    home: &Path,
    target_segments: usize,
    progress: &dyn IndexProgress,
) -> Result<(usize, usize)> {
    let full_text = index_dir(home).join("full_text");
    if !full_text.join("meta.json").exists() {
        anyhow::bail!(
            "no search index at {} — nothing to optimize (run `indice index` first)",
            full_text.display()
        );
    }
    let mut search = crate::search::SearchIndex::open(&full_text)
        .context("opening the search index to optimize")?;
    search.optimize(target_segments, progress)
}

/// Default segment target for compaction — the `optimize` CLI default, and the
/// target an automatic post-ingest compaction merges down to.
pub const DEFAULT_OPTIMIZE_TARGET: usize = 8;

/// Segment count above which the index counts as *fragmented*: a search fans out
/// across every segment, so a large count slows every query (notably the
/// homepage facet overview). Set well above a healthy, actively-merging index
/// (`optimize` targets 8, and Tantivy's background merges keep a busy index in
/// the low dozens) so it only trips on genuine fragmentation, not normal churn.
pub const FRAGMENTED_SEGMENT_THRESHOLD: usize = 48;

/// The full-text index's current segment count, or `None` when there's no index
/// yet. Read-only (no writer, no exclusive lock), so it's safe to call from a
/// running server or to cheaply gate an automatic compaction.
pub fn segment_count(home: &Path) -> Result<Option<usize>> {
    let full_text = index_dir(home).join("full_text");
    if !full_text.join("meta.json").exists() {
        return Ok(None);
    }
    let search = crate::search::SearchIndex::open_read_only(&full_text)
        .context("opening the search index to count segments")?;
    Ok(Some(search.segment_count()?))
}

/// Compact the index **only if** it has fragmented past
/// [`FRAGMENTED_SEGMENT_THRESHOLD`], leaving a healthy index — or a single
/// incremental add to a tidy one — untouched. Returns `Some((before, after))`
/// when it compacted, `None` when nothing needed doing. Intended as an automatic
/// post-ingest step so a big batch leaves a tidy index without the operator
/// having to remember `optimize`.
pub fn optimize_if_fragmented(
    home: &Path,
    progress: &dyn IndexProgress,
) -> Result<Option<(usize, usize)>> {
    match segment_count(home)? {
        Some(n) if n > FRAGMENTED_SEGMENT_THRESHOLD => {
            Ok(Some(optimize(home, DEFAULT_OPTIMIZE_TARGET, progress)?))
        }
        _ => Ok(None),
    }
}

/// The operator-facing nudge shown when the index has fragmented into `n`
/// segments — centralized so every caller (the CLI, the server) words it
/// identically.
pub fn fragmentation_warning(n: usize) -> String {
    format!(
        "the search index has {n} segments and may be slow to search; \
         run `indice optimize` to compact it"
    )
}

/// Warn (via `tracing`) if the index at `home` has fragmented past
/// [`FRAGMENTED_SEGMENT_THRESHOLD`]. Best-effort and read-only: for the paths
/// that detect fragmentation but don't auto-compact (an `index --no-optimize`).
/// A count error is logged at debug rather than surfaced — this is only a nudge.
pub fn warn_if_fragmented(home: &Path) {
    match segment_count(home) {
        Ok(Some(n)) if n > FRAGMENTED_SEGMENT_THRESHOLD => {
            tracing::warn!("{}", fragmentation_warning(n));
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("could not check index fragmentation: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn optimize_if_fragmented_compacts_only_when_over_threshold() {
        use crate::search::{Page, SearchIndex};
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let full_text = index_dir(home).join("full_text");

        // No index yet → nothing to count, nothing to compact.
        assert_eq!(segment_count(home).unwrap(), None);
        assert_eq!(
            optimize_if_fragmented(home, crate::index::no_progress()).unwrap(),
            None
        );

        // Build a deliberately fragmented index just past the threshold (one
        // segment per commit, background merges disabled).
        {
            let mut idx = SearchIndex::open(&full_text).unwrap();
            idx.disable_auto_merge();
            for i in 0..(FRAGMENTED_SEGMENT_THRESHOLD + 2) {
                let cid = format!("c{i}");
                idx.index_page(&Page {
                    url: "https://ex.com/x",
                    title: "T",
                    body: "body",
                    crawl_id: &cid,
                    crawl_name: "C",
                    ..Default::default()
                })
                .unwrap();
                idx.commit().unwrap();
            }
            assert!(idx.segment_count().unwrap() > FRAGMENTED_SEGMENT_THRESHOLD);
        } // drop the writer before optimize re-opens the index

        // Fragmented → compacts down toward the default target.
        let (before, after) = optimize_if_fragmented(home, crate::index::no_progress())
            .unwrap()
            .expect("a fragmented index is compacted");
        assert!(before > FRAGMENTED_SEGMENT_THRESHOLD);
        assert!(
            after <= DEFAULT_OPTIMIZE_TARGET,
            "compacted to {after} segments, expected ≤ {DEFAULT_OPTIMIZE_TARGET}"
        );

        // Now healthy → a second call is a no-op.
        assert_eq!(
            optimize_if_fragmented(home, crate::index::no_progress()).unwrap(),
            None
        );
    }
}
