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
    fn begin(&self, _label: &str) {}
    /// Describe the current setup activity (e.g. "downloading", "reading index"),
    /// so the spinner reflects what's actually happening before the record total
    /// is known.
    fn phase(&self, _phase: &str) {}
    /// The CDX has been read: `total` page records will be streamed. Cue to
    /// switch the spinner to a determinate bar.
    fn set_total(&self, _total: u64) {}
    /// `done` of the current WACZ's page records have been fetched.
    fn set_records(&self, _done: u64) {}
    /// A WACZ was indexed with `pages` pages, and the index has been committed.
    /// Emits a persistent one-line summary (the bar itself is transient and, in
    /// bar mode, the INFO logs that would otherwise report this are hushed).
    fn wacz_indexed(&self, _label: &str, _pages: u64) {}
    /// Work on the current WACZ finished (clear the spinner/bar).
    fn finish(&self) {}
}

/// A progress sink that drops every report.
///
/// Reporting progress is not conditional in the pipeline: "no UI" is this
/// value, not `None`. That is a deliberate distinction — an `Option` in a
/// signature should assert something about the *domain*, and "there may or may
/// not be a progress bar" is a fact about the caller's terminal, not about
/// indexing. Deciding whether to draw one is the caller's business, so the
/// `Option` stops there and the library always receives a real sink.
///
/// (Compare [`SourceResolver`]: `None` there means "no credentials are
/// configured", which ingest genuinely acts on, so that one stays optional.)
///
/// Every trait method has a default no-op body, so this is an empty impl. The
/// cost of those defaults is that a *forgotten* method is silent rather than a
/// compile error; acceptable here because all six return `()` and exist purely
/// to report.
pub struct NoProgress;

impl IndexProgress for NoProgress {}

/// A shared no-op progress sink, for a default that needs a reference.
///
/// `&NoProgress` is `&'static` by static promotion (a unit struct with no
/// `Drop` and no interior mutability), so this hands out a reference without
/// allocating or requiring the caller to own one.
pub fn no_progress() -> &'static dyn IndexProgress {
    &NoProgress
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
