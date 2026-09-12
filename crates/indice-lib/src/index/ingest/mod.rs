//! Ingest: bringing a WACZ into the archive and the search index.
//!
//! The public entry points ([`index_path`] and
//! [`Ingest::index_location`]) validate the argument, open the index once, and
//! drive [`index_one`] per source. `index_one` is the pipeline, and reads as
//! one: **acquire** the WACZ ([`acquire`]), **index** its pages ([`pages`]), then
//! **record** it in the manifest ([`record`]).
//!
//! The join between those phases is [`WaczAccess`] — the answer to "how do I read
//! this WACZ?" (stream it over HTTP ranges, or read a local file). Deciding that
//! once, up front, is what keeps the rest of the pipeline free of
//! remote-vs-local branching.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use tracing::debug;

use crate::collections::{file_sha256, wacz_id, CollectionId, Manifest, Source};
use crate::search::SearchIndex;
use crate::wacz::read_datapackage;

use super::paths::index_dir;
use super::{IndexProgress, SourceResolver};

mod acquire;
mod pages;
mod record;

#[cfg(test)]
mod tests;

/// How a WACZ will be read, decided once by [`acquire::open`] and threaded
/// through the rest of the pipeline.
///
/// Everything downstream — reading the datapackage, indexing pages, computing
/// fixity — asks this handle instead of re-deciding remote-vs-local for itself.
pub(super) enum WaczAccess {
    /// Read over HTTP range requests, without downloading the whole file.
    Stream { url: String },
    /// Read from a local file: one the curator supplied, one `--download`
    /// fetched into the archive, or a temp copy of a WACZ that couldn't be
    /// streamed. `_tmp` owns that temp file, so it lives exactly as long as the
    /// handle does.
    Local {
        path: PathBuf,
        _tmp: Option<tempfile::NamedTempFile>,
    },
}

impl WaczAccess {
    /// Whether reads go over the network (which sets the default fetch
    /// concurrency, and means there's no whole-file hash).
    fn is_remote(&self) -> bool {
        matches!(self, WaczAccess::Stream { .. })
    }

    /// The WACZ's `datapackage.json` metadata (default when it can't be parsed).
    fn read_datapackage(&self) -> Result<crate::wacz::WaczMetadata> {
        Ok(match self {
            WaczAccess::Stream { url } => {
                crate::wacz::read_datapackage_from(crate::http_range::open_remote(url)?)
                    .unwrap_or_default()
            }
            WaczAccess::Local { path, .. } => read_datapackage(path).unwrap_or_default(),
        })
    }

    /// The `(sha256, size)` fixity pair recorded in the manifest.
    ///
    /// A streamed remote is never read whole, so it has no hash (empty; `verify`
    /// already skips remote sources) and its size comes from `Content-Length`. A
    /// local file is hashed — reading every byte, which dominates the tail for a
    /// large WACZ, so it gets its own "checksumming" phase and timing.
    fn fixity(&self, progress: &dyn IndexProgress) -> Result<(String, u64)> {
        match self {
            WaczAccess::Stream { url } => Ok((
                String::new(),
                crate::http_range::open_remote(url)?.total_len(),
            )),
            WaczAccess::Local { path, .. } => {
                progress.phase("checksumming");
                let sha_start = std::time::Instant::now();
                let sha = file_sha256(path)
                    .with_context(|| format!("computing sha256 of {}", path.display()))?;
                let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                debug!(
                    sha_ms = sha_start.elapsed().as_millis() as u64,
                    bytes = size,
                    "computed whole-file SHA-256 (fixity)"
                );
                Ok((sha, size))
            }
        }
    }
}

/// One ingest, configured: where it runs, what it is being asked to do, and
/// the collaborators it may call out to.
///
/// `Ingest::new(home)` is already a complete, valid ingest; each setter narrows
/// it. That is the `std::process::Command` shape, and it matters here for a
/// reason beyond tidiness: the pipeline threads `&Ingest` through every phase,
/// so a new collaborator is **one field and one setter**, reachable by the
/// phase that needs it without any signature in between changing.
///
/// Growing the collaborators used to mean editing four already-crowded
/// parameter lists. That is why crawl custody ended up being written out of
/// band (see [`set_added_by`](crate::index::set_added_by)) rather than threaded
/// through, and why the snapshot it then needed produced two data-loss bugs.
///
/// Every field is private: an `Ingest` can only come from `new` plus setters,
/// the same one-way-in discipline as
/// [`CollectionId`](crate::collections::CollectionId).
#[derive(Clone, Copy)]
pub struct Ingest<'a> {
    /// indice home: `archive/`, `collections/` and `index/` hang off it.
    home: &'a Path,
    /// Display-name override for each crawl (`--name`); `None` → the WACZ
    /// datapackage title, falling back to the filename/URL stem.
    name: Option<&'a str>,
    /// Fetch a remote WACZ into `<home>/archive` and index it as a local file
    /// (durable copy, whole-file fixity, offline replay) instead of streaming.
    download: bool,
    /// Re-index a source already registered in the collection; otherwise it is
    /// skipped, so a large interrupted ingest resumes.
    force: bool,
    /// Concurrent record fetches for CDX-guided streaming; `None` → a
    /// per-source default (see `pages::default_concurrency`).
    concurrency: Option<usize>,
    /// Resolves a refreshable remote source (Browsertrix) to a fresh presigned
    /// URL. `None` means no credentials are configured — real information that
    /// `acquire::open` turns into a specific error, so this one stays optional.
    resolver: Option<&'a dyn SourceResolver>,
    /// Where progress is reported. Never optional: see
    /// [`NoProgress`](crate::index::NoProgress).
    progress: &'a dyn IndexProgress,
}

impl<'a> Ingest<'a> {
    /// An ingest into `home` with everything at its default: no name override,
    /// no download, no force, per-source concurrency, no resolver, no progress.
    pub fn new(home: &'a Path) -> Self {
        Self {
            home,
            name: None,
            download: false,
            force: false,
            concurrency: None,
            resolver: None,
            progress: crate::index::no_progress(),
        }
    }

    /// Override the display name of each crawl this ingest records.
    pub fn name(mut self, name: Option<&'a str>) -> Self {
        self.name = name;
        self
    }
    /// Fetch a remote WACZ into the archive instead of streaming it in place.
    pub fn download(mut self, yes: bool) -> Self {
        self.download = yes;
        self
    }
    /// Re-index sources already registered in the collection.
    pub fn force(mut self, yes: bool) -> Self {
        self.force = yes;
        self
    }
    /// Concurrent record fetches for streaming; `None` for the default.
    pub fn concurrency(mut self, n: Option<usize>) -> Self {
        self.concurrency = n;
        self
    }
    /// Supply a resolver for refreshable remote sources (Browsertrix).
    pub fn resolver(mut self, r: Option<&'a dyn SourceResolver>) -> Self {
        self.resolver = r;
        self
    }
    /// Report progress to `p`.
    pub fn progress(mut self, p: &'a dyn IndexProgress) -> Self {
        self.progress = p;
        self
    }

    // Read side, for `reindex`. The phases (`acquire`, `pages`, `record`) are
    // descendants of this module and read the private fields directly; a
    // sibling module cannot, so it goes through these.
    pub(crate) fn home_dir(&self) -> &'a Path {
        self.home
    }
    pub(crate) fn progress_sink(&self) -> &'a dyn IndexProgress {
        self.progress
    }
}

impl Ingest<'_> {
    /// Index a WACZ from a location into the home's `index/`. The location is
    /// either a local `.wacz` file (from anywhere) or a remote `http(s)://` URL.
    ///
    /// A local WACZ is filed into `<home>/archive/<collection-slug>/` — moved if
    /// it already sits under `archive/`, copied otherwise — and its path stored
    /// relative to `home`, so the home folder (archive + collections + index) is
    /// portable. A directory or non-`.wacz` path is an error (see
    /// [`acquire::resolve_sources`]).
    ///
    /// Idempotent: re-indexing the same source upserts its manifest entry and
    /// replaces its documents in Tantivy.
    pub fn index_location(&self, location: &str, collection: &str) -> Result<()> {
        let cx = self;
        let Ingest {
            home,
            name,
            force,
            progress,
            ..
        } = *self;
        // Every crawl belongs to a collection (its id is the slug of the name).
        let group = (
            crate::collections::CollectionId::from_name(collection),
            collection.to_string(),
        );

        // Resolve config (frugality cap + writer-heap ceiling) up front, so a
        // malformed config.yaml aborts here — before we copy files into archive/ or
        // touch the index — rather than silently indexing with the wrong setting.
        let config = crate::config::Config::load(home)?;

        let index_dir = index_dir(home);
        std::fs::create_dir_all(&index_dir)
            .with_context(|| format!("creating index dir {}", index_dir.display()))?;

        let mut manifest = Manifest::open(&index_dir)?;

        // Validate the argument and file local WACZs into the collection's archive
        // folder (a bad path errors before we touch the index; the manifest lets us
        // refuse a silent re-collection of an already-registered crawl).
        let sources = acquire::resolve_sources(location, home, &group.0, &manifest)?;

        let mut search_index = SearchIndex::open_with_heap(
            index_dir.join("full_text").as_path(),
            config.writer_heap_bytes(),
        )
        .with_context(|| format!("opening search index at {}", index_dir.display()))?;
        search_index.set_stored_body_cap(config.stored_body_cap_bytes());
        let search = Mutex::new(search_index);

        for source in &sources {
            // Resume-friendly: skip a source already indexed *into this collection*
            // on an earlier run (unless --force). Commit + save happen per WACZ below,
            // so a source is "registered" only once its docs are durable. Scoping to
            // the collection means a genuine (re)assignment to a different collection
            // isn't silently swallowed — it falls through to index_one (a local file
            // is refused upstream by place_local_wacz; a URL is re-homed as before).
            if !force
                && manifest
                    .wacz_by_id(&wacz_id(source))
                    .is_some_and(|w| w.collection == group.0)
            {
                progress.phase(&format!("skipping already-indexed {}", source.location()));
                continue;
            }

            let (wacz_name, pages) = index_one(
                cx,
                source,
                &mut manifest,
                &search,
                name,
                (&group.0, group.1.as_str()),
            )?;

            // Commit + save per WACZ so an interrupted large ingest keeps every
            // completed crawl (and a re-run resumes past it), rather than losing the
            // whole run's uncommitted work.
            progress.phase("committing");
            let commit_start = std::time::Instant::now();
            search.lock().unwrap().commit()?;
            debug!(
                commit_ms = commit_start.elapsed().as_millis() as u64,
                wacz = %wacz_name,
                "committed index"
            );
            manifest.save()?;

            // Per-WACZ summary persists above the next WACZ's progress bar.
            progress.wacz_indexed(&wacz_name, pages);
        }

        progress.finish();

        Ok(())
    }
}

/// Index a local WACZ file into the given collection (it's filed into
/// `<home>/archive/<slug>/`).
///
/// The plain "just index this file" door, kept as a free function because that
/// is what most callers and tests want.
pub fn index_path(path: &Path, home: &Path, name: Option<&str>, collection: &str) -> Result<()> {
    Ingest::new(home)
        .name(name)
        .index_location(&path.to_string_lossy(), collection)
}

/// Index a single WACZ source, start to finish: acquire it, index its pages, and
/// record it in the manifest. Returns the WACZ's display name and page count,
/// for a post-commit summary.
pub(super) fn index_one(
    cx: &Ingest,
    source: &Source,
    manifest: &mut Manifest,
    search: &Mutex<SearchIndex>,
    // Display-name override for *this* crawl: `--name` for an ingest, the
    // preserved manifest name for a reindex. Per-call rather than a field on
    // `Ingest`, because reindex passes a different one per source.
    name: Option<&str>,
    // The collection (id, display name) this WACZ joins — always set; every
    // crawl belongs to a collection (no singletons).
    collection: (&CollectionId, &str),
) -> Result<(String, u64)> {
    let (home, concurrency, progress) = (cx.home, cx.concurrency, cx.progress);
    // Show an indeterminate spinner from the very start: the setup work (probing
    // the host, downloading, reading the ZIP directory and CDX) happens before
    // any record total is known, and can take many seconds on a large remote
    // WACZ. The streaming path later calls `set_total` to switch to a bar.
    progress.begin(&source.location());

    // 1. Acquire: where the WACZ lives, and how we'll read it.
    let (effective_source, access) = acquire::open(cx, source, collection.0)?;

    // Its datapackage metadata, read up front so the title can name the crawl.
    // Precedence: explicit --name, then the WACZ title, then the filename/URL stem.
    let meta = access.read_datapackage()?;
    let id = wacz_id(&effective_source);
    let crawl_name = pick_display_name(name, &meta, &effective_source);

    // Drop this WACZ's prior documents so re-indexing upserts, not appends.
    search.lock().unwrap().delete_crawl_docs(&id);

    // 2. Index its pages, tagging each with this WACZ and its collection. The
    //    pass also caches a thumbnail and collects the provenance and capture
    //    stats step 3 needs — no re-read of the WARCs.
    let thumbs_dir = index_dir(home).join("thumbs");
    let pinned_thumb = crate::collections::pinned_thumb_path(home, collection.0, &id);
    // The crawl's representative-image source: the declared main page, else the
    // first seed page.
    let main_page_url = meta
        .main_page_url
        .clone()
        .or_else(|| meta.seed_pages.first().map(|s| s.url.clone()));
    let stats = pages::index(
        &access,
        &pages::Ctx {
            docs: pages::Docs {
                crawl_id: &id,
                crawl_name: &crawl_name,
                collection: collection.0,
                search,
            },
            workers: resolve_workers(concurrency, access.is_remote()),
            thumbs_dir: &thumbs_dir,
            pinned_thumb: &pinned_thumb,
            main_page_url: main_page_url.as_deref(),
            progress,
        },
    )?;

    // Capture the outcome to report once the index is committed (see
    // `index_location`), not here - the commit could still fail.
    let outcome = (crawl_name.clone(), stats.pages);

    // Index the WACZ's metadata as a searchable document, tagged with its collection.
    let coll_body = record::collection_body(&meta);
    search
        .lock()
        .unwrap()
        .index_collection(&id, &crawl_name, collection.0, &coll_body)?;

    // 3. Record it: fixity + provenance into the manifest entry.
    let fixity = access.fixity(progress)?;
    record::upsert(
        manifest,
        record::Indexed {
            id: &id,
            collection,
            source: &effective_source,
            display_name: &crawl_name,
            meta,
            stats,
            fixity,
        },
    );

    // Note: the spinner/bar is *not* finished here - the Tantivy commit happens
    // once per `index_location` (after all sources), and that's where it's cleared
    // (via a final "committing" spinner) and the summary is emitted. See
    // `index_location`.
    Ok(outcome)
}

/// The crawl's display name: an explicit `--name` wins, then the WACZ
/// datapackage title, then the filename/URL stem.
fn pick_display_name(
    explicit: Option<&str>,
    meta: &crate::wacz::WaczMetadata,
    source: &Source,
) -> String {
    explicit
        .map(|n| n.to_string())
        .or_else(|| meta.title.clone().filter(|t| !t.trim().is_empty()))
        .unwrap_or_else(|| acquire::source_display_name(source))
}

/// Resolve fetch concurrency once we know whether this WACZ is remote, then clamp
/// it to a per-host ceiling so no `--concurrency` setting can flood a single host
/// with an unbounded number of in-flight range requests (polite by default, and a
/// guard against a mis-typed value like `--concurrency 500`).
fn resolve_workers(concurrency: Option<usize>, remote: bool) -> usize {
    let requested = concurrency.unwrap_or_else(|| pages::default_concurrency(remote));
    if requested > pages::MAX_CONCURRENCY {
        tracing::warn!(
            requested,
            cap = pages::MAX_CONCURRENCY,
            "capping fetch concurrency to the per-host ceiling"
        );
    }
    requested.clamp(1, pages::MAX_CONCURRENCY)
}
