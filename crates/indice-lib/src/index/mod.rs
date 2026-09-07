use anyhow::Result;

use crate::collections::Source;

// Functionally-aligned submodules. `mod.rs` retains the two public traits and
// re-exports every public item so external `index::<symbol>` paths keep resolving.
mod delete;
mod ingest;
mod metadata;
mod optimize;
mod paths;
mod provenance;
mod reindex;
mod search_sync;
mod stats;
mod swap;

pub use delete::*;
pub use ingest::*;
pub use metadata::*;
pub use optimize::*;
pub use paths::*;
pub use provenance::*;
pub use reindex::*;
pub use search_sync::*;
pub use stats::*;

/// Progress sink for indexing, implemented by the binary (e.g. with a progress
/// bar). The library stays UI- and dependency-free: it only reports counts.
/// Streaming a remote WACZ can be slow (each page record is a separate HTTP
/// range request, and reading the CDX up front takes a moment), so this makes
/// both the setup and the per-record work visible.
///
/// Lifecycle per WACZ: `begin` once → optionally `set_total` then `set_records*`
/// (streaming path, where a record count is known) → `finish` once.
pub trait IndexProgress: Sync {
    /// Work on a WACZ has begun. The record total isn't known yet (the ZIP
    /// directory and CDX must be read first), so this is the cue for an
    /// indeterminate spinner. `label` is the WACZ URL or path.
    fn begin(&self, label: &str);
    /// Describe the current setup activity (e.g. "downloading", "reading index"),
    /// so the spinner reflects what's actually happening before the record total
    /// is known.
    fn phase(&self, phase: &str);
    /// The CDX has been read: `total` page records will be streamed. Cue to
    /// switch the spinner to a determinate bar.
    fn set_total(&self, total: u64);
    /// `done` of the current WACZ's page records have been fetched.
    fn set_records(&self, done: u64);
    /// A WACZ was indexed with `pages` pages, and the index has been committed.
    /// Emits a persistent one-line summary (the bar itself is transient and, in
    /// bar mode, the INFO logs that would otherwise report this are hushed).
    fn wacz_indexed(&self, label: &str, pages: u64);
    /// Work on the current WACZ finished (clear the spinner/bar).
    fn finish(&self);
}

/// Resolves a refreshable remote [`Source`] — currently a
/// [`Source::Browsertrix`] resource — to a fresh, directly-fetchable URL
/// (Browsertrix presigned URLs expire, so they must be re-resolved each time we
/// index or replay). Implemented by the **binary**, which holds the credentials,
/// keeping auth/config out of the library. `Send + Sync` so it can be shared
/// while indexing and held in the server's shared state for replay.
pub trait SourceResolver: Send + Sync {
    fn resolve(&self, source: &Source) -> Result<String>;
}

#[cfg(test)]
pub(super) mod testsupport {
    use std::path::Path;

    use super::index_path;

    pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    pub fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(FIXTURES).join(name)
    }

    /// Copy a fixture WACZ into `<home>/archive` and index it from there, which
    /// the archive requirement demands for local files. Returns the copied path.
    pub fn index_fixture(name: &str, home: &Path, display: Option<&str>) -> std::path::PathBuf {
        let archive = home.join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        let staged = archive.join(name);
        std::fs::copy(fixture(name), &staged).unwrap();
        index_path(&staged, home, display, "test").unwrap();
        // index files the WACZ into the collection's folder (collection "test"),
        // so its resting place is archive/test/<name>.
        archive.join("test").join(name)
    }
}
