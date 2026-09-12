//! Ingest: bringing a WACZ into the archive and the search index.
//!
//! The public entry points ([`index_path`] / [`index_location`] /
//! [`index_location_with_resolver`]) validate the argument, open the index once,
//! and drive [`index_one`] per source. `index_one` is the pipeline, and reads as
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

/// Index a local WACZ file into the given collection (it's filed into
/// `<home>/archive/<slug>/`). Thin wrapper over [`index_location`].
pub fn index_path(path: &Path, home: &Path, name: Option<&str>, collection: &str) -> Result<()> {
    index_location(
        &path.to_string_lossy(),
        home,
        name,
        collection,
        false, // download
        false, // force
        None,  // concurrency: per-source default
        crate::index::no_progress(),
    )
}

/// Index a WACZ from a location into the home directory's `index/`. The location
/// is either a local `.wacz` file (from anywhere) or a remote `http(s)://` URL.
///
/// A local WACZ is filed into `<home>/archive/<collection-slug>/` — moved if it
/// already sits under `archive/`, copied otherwise — and its path stored relative
/// to `home`, so the home folder (archive + collections + index) is portable. A
/// directory or non-`.wacz` path is an error (see [`acquire::resolve_sources`]).
///
/// Idempotent: re-indexing the same source upserts its manifest entry and
/// replaces its documents in Tantivy.
/// `name` overrides the collection display name; otherwise it comes from the
/// WACZ metadata, falling back to the filename/URL stem.
#[allow(clippy::too_many_arguments)]
pub fn index_location(
    location: &str,
    home: &Path,
    name: Option<&str>,
    collection: &str,
    download: bool,
    // Re-index a source even if it's already registered in the collection;
    // otherwise an already-indexed source is skipped (so a large ingest resumes).
    force: bool,
    concurrency: Option<usize>,
    progress: &dyn IndexProgress,
) -> Result<()> {
    index_location_with_resolver(
        location,
        home,
        name,
        collection,
        download,
        force,
        concurrency,
        None,
        progress,
    )
}

/// Like [`index_location`], but with a [`SourceResolver`] for refreshable remote
/// sources (Browsertrix). The importer's streaming mode passes one; plain
/// `index` doesn't need it.
#[allow(clippy::too_many_arguments)]
pub fn index_location_with_resolver(
    location: &str,
    home: &Path,
    name: Option<&str>,
    collection: &str,
    // Download a remote WACZ into <home>/archive and index it as a local file
    // instead of streaming it in place.
    download: bool,
    // Re-index a source even if already registered in the collection; otherwise
    // an already-indexed source is skipped so a large ingest can resume.
    force: bool,
    // Concurrent record fetches for CDX-guided streaming; `None` = per-source
    // default (4 remote, CPU count local).
    concurrency: Option<usize>,
    // Resolves a Browsertrix source to a fresh presigned URL (binary-provided).
    resolver: Option<&dyn SourceResolver>,
    // Optional progress sink for indexing (the binary renders a bar).
    progress: &dyn IndexProgress,
) -> Result<()> {
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
            source,
            home,
            &mut manifest,
            &search,
            name,
            (&group.0, group.1.as_str()),
            download,
            concurrency,
            resolver,
            progress,
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

/// Index a single WACZ source, start to finish: acquire it, index its pages, and
/// record it in the manifest. Returns the WACZ's display name and page count,
/// for a post-commit summary.
#[allow(clippy::too_many_arguments)]
pub(super) fn index_one(
    source: &Source,
    home: &Path,
    manifest: &mut Manifest,
    search: &Mutex<SearchIndex>,
    name: Option<&str>,
    // The collection (id, display name) this WACZ joins — always set; every
    // crawl belongs to a collection (no singletons).
    collection: (&CollectionId, &str),
    // Download a remote WACZ into <home>/archive and index it as a local file
    // (durable copy, whole-file fixity, offline replay) instead of streaming.
    download: bool,
    // Concurrent record fetches for CDX-guided streaming; `None` picks a
    // per-source default (see `pages::default_concurrency`).
    concurrency: Option<usize>,
    // Resolves a Browsertrix source to a fresh presigned URL (binary-provided).
    resolver: Option<&dyn SourceResolver>,
    // Optional progress sink for indexing.
    progress: &dyn IndexProgress,
) -> Result<(String, u64)> {
    // Show an indeterminate spinner from the very start: the setup work (probing
    // the host, downloading, reading the ZIP directory and CDX) happens before
    // any record total is known, and can take many seconds on a large remote
    // WACZ. The streaming path later calls `set_total` to switch to a bar.
    progress.begin(&source.location());

    // 1. Acquire: where the WACZ lives, and how we'll read it.
    let (effective_source, access) =
        acquire::open(source, home, collection.0, download, resolver, progress)?;

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
        &id,
        collection,
        &effective_source,
        &crawl_name,
        meta,
        stats,
        fixity,
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
