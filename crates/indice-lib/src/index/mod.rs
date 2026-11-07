use anyhow::Result;

use crate::collections::Source;
use crate::i18n::{FluentArg, L10n};

// Functionally-aligned submodules. `mod.rs` retains the two public traits and
// re-exports every public item so external `index::<symbol>` paths keep resolving.
mod delete;
mod ingest;
pub(crate) mod lock;
pub(crate) mod manifest;
mod metadata;
mod optimize;
mod paths;
mod provenance;
mod reconcile;
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
pub use reconcile::*;
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
    /// Describe the current setup activity (e.g. downloading, reading the
    /// index), so the spinner reflects what's actually happening before the
    /// record total is known. A [`Phase`] is data, not text: each sink renders it
    /// for its own reader (the CLI in English, the web page in the language of
    /// whoever started the job).
    fn phase(&self, _phase: &Phase) {}
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

/// What indexing is doing right now, as data. The library names the activity
/// and its facts (a count, a file name); the sink that shows it chooses the
/// words, so one phase can read in English on the command line and in Italian
/// on a curator's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Downloading,
    Resolving,
    Checksumming,
    Scanning,
    ReadingIndex,
    BuildingIndex,
    Committing,
    SkippingAlreadyIndexed {
        location: String,
    },
    NestedWacz {
        n: usize,
        of: usize,
    },
    /// `holder` describes another process or operation, as the lock layer
    /// recorded it. It is a fact about someone else, so it is shown as given.
    WaitingFor {
        holder: String,
    },
    WaitingForImport,
    BuildingWacz,
    /// `bytes` is `None` when the source does not say how big the file is.
    DownloadingFile {
        name: String,
        bytes: Option<u64>,
        n: usize,
        of: usize,
    },
    Segments {
        count: usize,
    },
    ExpungingDeletes {
        segments: usize,
    },
    RemovedOrphans {
        files: usize,
    },
    ReadingManifest,
    ListingCrawls,
    LookingForOrphanFiles,
}

impl Phase {
    /// The catalog message that words this phase.
    pub fn key(&self) -> &'static str {
        match self {
            Phase::Downloading => "phase-downloading",
            Phase::Resolving => "phase-resolving",
            Phase::Checksumming => "phase-checksumming",
            Phase::Scanning => "phase-scanning",
            Phase::ReadingIndex => "phase-reading-index",
            Phase::BuildingIndex => "phase-building-index",
            Phase::Committing => "phase-committing",
            Phase::SkippingAlreadyIndexed { .. } => "phase-skipping-already-indexed",
            Phase::NestedWacz { .. } => "phase-nested-wacz",
            Phase::WaitingFor { .. } => "phase-waiting-for",
            Phase::WaitingForImport => "phase-waiting-for-import",
            Phase::BuildingWacz => "phase-building-wacz",
            Phase::DownloadingFile { bytes: Some(_), .. } => "phase-downloading-file",
            Phase::DownloadingFile { bytes: None, .. } => "phase-downloading-file-unsized",
            Phase::Segments { .. } => "phase-segments",
            Phase::ExpungingDeletes { .. } => "phase-expunging-deletes",
            Phase::RemovedOrphans { .. } => "phase-removed-orphans",
            Phase::ReadingManifest => "phase-reading-manifest",
            Phase::ListingCrawls => "phase-listing-crawls",
            Phase::LookingForOrphanFiles => "phase-looking-for-orphan-files",
        }
    }

    /// The phase in `l10n`'s language.
    pub fn render(&self, l10n: &L10n) -> String {
        let key = self.key();
        let num = |n: usize| FluentArg::Num(n as i64);
        match self {
            Phase::SkippingAlreadyIndexed { location } => {
                l10n.tr_args(key, &[("location", FluentArg::Str(location))])
            }
            Phase::NestedWacz { n, of } => l10n.tr_args(key, &[("n", num(*n)), ("of", num(*of))]),
            Phase::WaitingFor { holder } => {
                l10n.tr_args(key, &[("holder", FluentArg::Str(holder))])
            }
            Phase::DownloadingFile { name, bytes, n, of } => {
                let size = bytes.map(|b| l10n.size(b)).unwrap_or_default();
                l10n.tr_args(
                    key,
                    &[
                        ("name", FluentArg::Str(name)),
                        ("size", FluentArg::Str(&size)),
                        ("n", num(*n)),
                        ("of", num(*of)),
                    ],
                )
            }
            Phase::Segments { count } => l10n.tr_args(key, &[("count", num(*count))]),
            Phase::ExpungingDeletes { segments } => {
                l10n.tr_args(key, &[("segments", num(*segments))])
            }
            Phase::RemovedOrphans { files } => l10n.tr_args(key, &[("files", num(*files))]),
            _ => l10n.tr(key),
        }
    }
}

/// The English text, which is what the command line shows and the log records.
impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render(&L10n::default()))
    }
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

    /// `source`'s URL, with every other the same lookup turned up. One
    /// Browsertrix lookup returns the URLs of a whole collection or item, so
    /// replay can cache them all from one call rather than make one per file.
    /// The default knows only `source`'s, with no expiry.
    fn resolve_all(&self, source: &Source) -> Result<Vec<Resolved>> {
        Ok(vec![Resolved {
            source: source.clone(),
            url: self.resolve(source)?,
            expires: None,
        }])
    }
}

/// A URL a [`SourceResolver`] found for a source, and when it stops working,
/// if the service said.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub source: Source,
    pub url: String,
    pub expires: Option<std::time::SystemTime>,
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

#[cfg(test)]
mod phase_tests {
    use super::Phase;
    use crate::i18n::{negotiate, parse_language, L10n};

    /// Every variant, with the text the command line showed before phases were
    /// data. The CLI prints `Display`, so these strings are its output.
    fn samples() -> Vec<(Phase, &'static str)> {
        vec![
            (Phase::Downloading, "downloading"),
            (Phase::Resolving, "resolving"),
            (Phase::Checksumming, "checksumming"),
            (Phase::Scanning, "scanning"),
            (Phase::ReadingIndex, "reading index"),
            (Phase::BuildingIndex, "building index"),
            (Phase::Committing, "committing"),
            (
                Phase::SkippingAlreadyIndexed {
                    location: "https://x.test/a.wacz".into(),
                },
                "skipping already-indexed https://x.test/a.wacz",
            ),
            (Phase::NestedWacz { n: 2, of: 5 }, "nested WACZ 2/5"),
            (
                Phase::WaitingFor { holder: "x".into() },
                "waiting for x to finish…",
            ),
            (
                Phase::WaitingForImport,
                "waiting for another import to finish…",
            ),
            (Phase::BuildingWacz, "building WACZ"),
            (
                Phase::DownloadingFile {
                    name: "a.warc.gz".into(),
                    bytes: Some(2_621_440),
                    n: 1,
                    of: 3,
                },
                "downloading a.warc.gz (2.5 MB) [1/3]",
            ),
            (
                Phase::DownloadingFile {
                    name: "a.warc.gz".into(),
                    bytes: None,
                    n: 2,
                    of: 3,
                },
                "downloading a.warc.gz [2/3]",
            ),
            (Phase::Segments { count: 4 }, "4 segments"),
            // Counts of a thousand or more print ungrouped, as the CLI always has.
            (Phase::Segments { count: 12_345 }, "12345 segments"),
            (
                Phase::NestedWacz { n: 1000, of: 1500 },
                "nested WACZ 1000/1500",
            ),
            (
                Phase::ExpungingDeletes { segments: 2 },
                "expunging deletes (2 segment(s))",
            ),
            (
                Phase::RemovedOrphans { files: 3 },
                "removed 3 orphaned file(s)",
            ),
            (Phase::ReadingManifest, "reading the manifest"),
            (Phase::ListingCrawls, "listing crawls in the index"),
            (
                Phase::LookingForOrphanFiles,
                "looking for the orphans' files on disk",
            ),
        ]
    }

    #[test]
    fn phase_display_is_the_english_text() {
        for (phase, text) in samples() {
            assert_eq!(phase.to_string(), text, "{phase:?}");
        }
    }

    #[test]
    fn every_phase_has_a_message_in_every_language() {
        let italian = negotiate(Some("it"), &parse_language("en").unwrap());
        for (phase, english) in samples() {
            let key = phase.key();
            assert!(
                crate::i18n::CATALOGS
                    .iter()
                    .all(|(_, src)| src.contains(&format!("\n{key} ="))),
                "{key} missing from a catalog"
            );
            // A key a catalog lacks renders as the bare key.
            assert_ne!(phase.render(&italian), key);
            assert_ne!(phase.render(&italian), english, "{key} not translated");
            assert_ne!(phase.render(&L10n::default()), key);
        }
    }
}
