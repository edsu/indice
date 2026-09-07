//! The ingest engine and its public entry points. `index_path` / `index_location`
//! / `index_location_with_resolver` drive `index_one` and its private helpers
//! (source placement, probing, extraction, nested-WACZ flattening, per-URL merge,
//! CDX-guided streaming, thumbnailing). Only `index_one` is `pub(super)` (it's
//! reused by `reindex`); everything else is private to this module.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rayon::prelude::*;
use tracing::{debug, info};

use crate::collections::{file_sha256, wacz_id, Manifest, Source, Wacz};
use crate::http_range::{RangeFetch, RangeReader};
use crate::search::{extract_html_text, SearchIndex};
use crate::wacz::{extract_warc_from_wacz, iter_warc_paths, read_datapackage};
use crate::warc::{iter_records, WarcRecord, Warcinfo};

use super::paths::{archive_dir, index_dir, year_prefix};
use super::{IndexProgress, SourceResolver};

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
        None,
        None,
    )
}

/// Index a WACZ from a location into the home directory's `index/`. The location
/// is either a local `.wacz` file (from anywhere) or a remote `http(s)://` URL.
///
/// A local WACZ is filed into `<home>/archive/<collection-slug>/` — moved if it
/// already sits under `archive/`, copied otherwise — and its path stored relative
/// to `home`, so the home folder (archive + collections + index) is portable. A
/// directory or non-`.wacz` path is an error (see [`resolve_sources`] /
/// [`place_local_wacz`]).
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
    progress: Option<&dyn IndexProgress>,
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
    progress: Option<&dyn IndexProgress>,
) -> Result<()> {
    // Every crawl belongs to a collection (its id is the slug of the name).
    let group = (
        crate::collections::slugify(collection),
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
    let sources = resolve_sources(location, home, &group.0, &manifest)?;

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
            if let Some(p) = progress {
                p.phase(&format!("skipping already-indexed {}", source.location()));
            }
            continue;
        }

        let (wacz_name, pages) = index_one(
            source,
            home,
            &mut manifest,
            &search,
            name,
            (group.0.as_str(), group.1.as_str()),
            download,
            concurrency,
            resolver,
            progress,
        )?;

        // Commit + save per WACZ so an interrupted large ingest keeps every
        // completed crawl (and a re-run resumes past it), rather than losing the
        // whole run's uncommitted work.
        if let Some(p) = progress {
            p.phase("committing");
        }
        let commit_start = std::time::Instant::now();
        search.lock().unwrap().commit()?;
        debug!(
            commit_ms = commit_start.elapsed().as_millis() as u64,
            wacz = %wacz_name,
            "committed index"
        );
        manifest.save()?;

        // Per-WACZ summary persists above the next WACZ's progress bar.
        if let Some(p) = progress {
            p.wacz_indexed(&wacz_name, pages);
        }
    }

    if let Some(p) = progress {
        p.finish();
    }

    Ok(())
}

/// Turn one `index` argument into a source to index, filing local WACZs into the
/// collection's archive folder. An `http(s)://` URL yields a URL source. A local
/// `.wacz` file may live anywhere: it's brought into `<home>/archive/<slug>/` —
/// **moved** if it already sits under `archive/` (reorganized within indice's
/// own space), **copied** otherwise (the original is left intact) — so the home
/// directory stays self-contained and portable and the archive is browsable by
/// collection. Directories and non-`.wacz` paths are errors with guidance.
fn resolve_sources(
    location: &str,
    home: &Path,
    collection_slug: &str,
    manifest: &Manifest,
) -> Result<Vec<Source>> {
    match Source::parse(location) {
        url @ Source::Url(_) => Ok(vec![url]),
        bt @ (Source::Browsertrix { .. } | Source::BrowsertrixPublic { .. }) => Ok(vec![bt]),
        Source::File(p) => Ok(vec![place_local_wacz(&p, home, collection_slug, manifest)?]),
    }
}

/// The archive folder for a collection: `<home>/archive/<slug>/`, where local
/// WACZs for that collection are filed.
fn collection_archive_dir(home: &Path, slug: &str) -> PathBuf {
    archive_dir(home).join(slug)
}

/// Pick a destination for `source` inside `dir` that won't clobber a *different*
/// WACZ already filed there. A byte-identical file already present is reused (so
/// re-indexing the same WACZ is idempotent); a name clash with different content
/// gets a `-2`, `-3`, … suffix. Returns an existing path only when it's
/// byte-identical to `source`.
fn pick_archive_dest(
    dir: &Path,
    filename: &std::ffi::OsStr,
    source: &Path,
) -> Result<std::path::PathBuf> {
    let first = dir.join(filename);
    if !first.exists() {
        return Ok(first);
    }
    let source_sha = file_sha256(source)?;
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("wacz");
    let mut path = first;
    let mut n = 1u32;
    loop {
        if !path.exists() || file_sha256(&path)? == source_sha {
            return Ok(path);
        }
        n += 1;
        path = dir.join(format!("{stem}-{n}.wacz"));
    }
}

/// Bring a local WACZ into `<home>/archive/<slug>/` and return it as a
/// home-relative File [`Source`]. Moves it if it's already under `archive/`
/// (reorganizing within the managed space); copies it otherwise. A file already
/// in the collection folder is used in place; a byte-identical file already
/// filed there is reused (idempotent re-index).
fn place_local_wacz(
    path: &Path,
    home: &Path,
    collection_slug: &str,
    manifest: &Manifest,
) -> Result<Source> {
    if path.is_dir() {
        anyhow::bail!(
            "{} is a directory; pass individual .wacz files instead \
             (e.g. `indice index archive/*.wacz`)",
            path.display()
        );
    }
    if path.extension().and_then(|e| e.to_str()) != Some("wacz") {
        anyhow::bail!("{} is not a .wacz file or an http(s) URL", path.display());
    }
    let abs = path
        .canonicalize()
        .with_context(|| format!("{} does not exist", path.display()))?;

    let dest_dir = collection_archive_dir(home, collection_slug);

    // Already inside the collection's archive folder (at any depth — e.g. the
    // importer's archive/<slug>/<item-id>/ subdir)? Use it in place, no move.
    if dest_dir
        .canonicalize()
        .map(|d| abs.starts_with(&d))
        .unwrap_or(false)
    {
        return Ok(Source::for_file(&abs, home));
    }

    // Guard against silent re-collection: if this exact file is already a
    // registered member of a *different* collection, moving it would change its
    // id and orphan that membership (and its committed note/thumbnail). Re-homing
    // a crawl isn't supported yet, so refuse rather than corrupt.
    if let Some(existing) = manifest.waczs.iter().find(|w| {
        w.source
            .resolve(home)
            .and_then(|p| p.canonicalize().ok())
            .is_some_and(|p| p == abs)
    }) {
        if existing.collection != collection_slug {
            anyhow::bail!(
                "{} is already in collection \"{}\"; re-collecting a crawl isn't supported yet \
                 — remove it from that collection first, or index a separate copy.",
                abs.display(),
                existing.collection
            );
        }
    }

    std::fs::create_dir_all(&dest_dir)
        .with_context(|| format!("creating archive dir {}", dest_dir.display()))?;
    let filename = abs
        .file_name()
        .with_context(|| format!("{} has no file name", abs.display()))?;
    let dest = pick_archive_dest(&dest_dir, filename, &abs)?;

    if dest.exists() {
        // pick_archive_dest returned a byte-identical file already filed here —
        // reuse it (idempotent re-index), nothing to write.
    } else {
        // A file already under archive/ is on the same filesystem as its
        // destination, so a rename (move) is cheap and non-duplicating; a file
        // from elsewhere is copied so the curator's original is left untouched.
        let under_archive = archive_dir(home)
            .canonicalize()
            .map(|a| abs.starts_with(&a))
            .unwrap_or(false);
        if under_archive {
            std::fs::rename(&abs, &dest)
                .with_context(|| format!("moving {} to {}", abs.display(), dest.display()))?;
            info!(from = %abs.display(), to = %dest.display(), "moved WACZ into collection archive");
        } else {
            std::fs::copy(&abs, &dest)
                .with_context(|| format!("copying {} to {}", abs.display(), dest.display()))?;
            info!(from = %abs.display(), to = %dest.display(), "copied WACZ into collection archive");
        }
    }
    let dest_abs = dest.canonicalize().unwrap_or(dest);
    Ok(Source::for_file(&dest_abs, home))
}

/// Index a single WACZ source: obtain a local readable copy (downloading a URL
/// to a temp file), index its pages and metadata, and upsert its manifest entry.
/// Returns the WACZ's display name and page count, for a post-commit summary.
#[allow(clippy::too_many_arguments)]
pub(super) fn index_one(
    source: &Source,
    home: &Path,
    manifest: &mut Manifest,
    search: &Mutex<SearchIndex>,
    name: Option<&str>,
    // The collection (id, display name) this WACZ joins — always set; every
    // crawl belongs to a collection (no singletons).
    collection: (&str, &str),
    // Download a remote WACZ into <home>/archive and index it as a local file
    // (durable copy, whole-file fixity, offline replay) instead of streaming.
    download: bool,
    // Concurrent record fetches for CDX-guided streaming; `None` picks a
    // per-source default (see `default_concurrency`).
    concurrency: Option<usize>,
    // Resolves a Browsertrix source to a fresh presigned URL (binary-provided).
    resolver: Option<&dyn SourceResolver>,
    // Optional progress sink for indexing.
    progress: Option<&dyn IndexProgress>,
) -> Result<(String, u64)> {
    // Show an indeterminate spinner from the very start: the setup work (probing
    // the host, downloading, reading the ZIP directory and CDX) happens before
    // any record total is known, and can take many seconds on a large remote
    // WACZ. The streaming path later calls `set_total` to switch to a bar.
    if let Some(p) = progress {
        p.begin(&source.location());
    }

    // Resolve how the WACZ is read (`.3` mode logic):
    //  - A local File source: read in place.
    //  - A remote URL + --download: fetch into <home>/archive and adopt as a
    //    local File source (durable/offline; the recorded source becomes local).
    //  - A remote URL (default): stream over HTTP range requests, no download —
    //    but only if its WARCs are Stored; if they're deflated, fall back to a
    //    temp download + scan (keeping the URL as the source).
    let effective_source: Source = match source {
        Source::Url(u) if download => {
            info!(url = %u, "downloading remote WACZ into archive");
            if let Some(p) = progress {
                p.phase("downloading");
            }
            Source::File(download_into_archive(u, home, collection.0)?)
        }
        _ => source.clone(),
    };

    let mut _tmp: Option<tempfile::NamedTempFile> = None;
    let (remote_url, local): (Option<String>, Option<PathBuf>) = match &effective_source {
        Source::File(_p) => (None, Some(effective_source.resolve(home).unwrap())),
        Source::Url(u) => {
            // Stream by default, but only if the host supports range requests and
            // the WARCs are Stored. Otherwise (no range, deflated WARCs, or a
            // probe error) fall back to downloading a temp copy and scanning it,
            // keeping the URL as the source.
            if remote_warcs_streamable(u).unwrap_or(false) {
                (Some(u.clone()), None)
            } else {
                info!(url = %u, "remote WACZ can't be streamed (no range support or compressed WARCs); downloading to index");
                if let Some(p) = progress {
                    p.phase("downloading");
                }
                let tmp = download_to_temp(u).with_context(|| format!("downloading {u}"))?;
                let p = tmp.path().to_path_buf();
                _tmp = Some(tmp);
                (None, Some(p))
            }
        }
        // A Browsertrix resource (private or public): resolve a fresh presigned
        // URL (they expire) and stream it. The recorded source stays the stable
        // Browsertrix identity, so the id is stable across re-imports and replay
        // re-resolves later.
        bt @ (Source::Browsertrix { .. } | Source::BrowsertrixPublic { .. }) => {
            if let Some(p) = progress {
                p.phase("resolving");
            }
            let resolver = resolver.ok_or_else(|| {
                anyhow::anyhow!(
                    "indexing a Browsertrix source needs a resolver to fetch a fresh URL \
                     (a private source needs BROWSERTRIX_USER + BROWSERTRIX_PASSWORD or \
                     BROWSERTRIX_TOKEN; a public source is resolved without credentials)"
                )
            })?;
            let url = resolver
                .resolve(bt)
                .with_context(|| format!("resolving {}", bt.location()))?;
            if !remote_warcs_streamable(&url).unwrap_or(false) {
                anyhow::bail!(
                    "this Browsertrix WACZ can't be stream-indexed (compressed WARCs or \
                     no range support); import it in download mode instead"
                );
            }
            (Some(url), None)
        }
    };
    let id = wacz_id(&effective_source);

    // Read metadata from the WACZ datapackage.json up front so its title can
    // name the collection. Precedence: explicit --name, then the WACZ title,
    // then the filename/URL stem.
    let meta = match &remote_url {
        Some(u) => crate::wacz::read_datapackage_from(crate::http_range::open_remote(u)?)
            .unwrap_or_default(),
        None => read_datapackage(local.as_ref().unwrap()).unwrap_or_default(),
    };
    let display_name = name
        .map(|n| n.to_string())
        .or_else(|| meta.title.clone().filter(|t| !t.trim().is_empty()))
        .unwrap_or_else(|| source_display_name(&effective_source));

    // The curated collection this WACZ joins (always supplied by the caller).
    let (collection_id, collection_name) = (collection.0.to_string(), collection.1.to_string());

    // Drop this WACZ's prior documents so re-indexing upserts, not appends.
    search.lock().unwrap().delete_crawl_docs(&id);

    // Index pages, tagging each with this WACZ (id/name) and its collection. The
    // pass also collects provenance and capture stats (no re-read of the WARCs).
    // CDX-guided extraction is the default everywhere (replay already resolves
    // records through the CDX, so indexing trusts it too); a full scan is only
    // the fallback when a WACZ can't be CDX-guided (deflated WARCs / no CDX).
    // Resolve fetch concurrency once we know whether this WACZ is remote, then
    // clamp it to a per-host ceiling so no `--concurrency` setting can flood a
    // single host with an unbounded number of in-flight range requests (polite
    // by default, and a guard against a mis-typed value like `--concurrency 500`).
    let requested = concurrency.unwrap_or_else(|| default_concurrency(remote_url.is_some()));
    let workers = requested.clamp(1, MAX_CONCURRENCY);
    if requested > MAX_CONCURRENCY {
        tracing::warn!(
            requested,
            cap = MAX_CONCURRENCY,
            "capping fetch concurrency to the per-host ceiling"
        );
    }
    // The crawl's representative-image source: the declared main page, else the
    // first seed page. Used after indexing to cache a thumbnail (best-effort).
    let main_page_url = meta
        .main_page_url
        .clone()
        .or_else(|| meta.seed_pages.first().map(|s| s.url.clone()));
    let thumbs_dir = index_dir(home).join("thumbs");
    // A nested multi-WACZ (a WACZ of WACZs, e.g. Browsertrix's combined
    // collection download) has no top-level WARCs, so the normal paths would
    // index it as empty. Detect and index it up front, flattening all inner
    // WACZs into this one crawl; otherwise fall through to normal indexing.
    // (This costs one extra WACZ open on the common flat path — cheap: reading
    // the ZIP directory, dwarfed by the record streaming that follows — and it's
    // the deliberate price of detecting nesting structurally rather than trusting
    // the non-standard multi-wacz-package profile.)
    let stats = if let Some(nested) = index_nested(
        local.as_deref(),
        remote_url.as_deref(),
        &id,
        &display_name,
        &collection_id,
        search,
        workers,
        progress,
    )? {
        nested
    } else {
        match &remote_url {
            Some(u) => {
                info!(url = %u, "streaming remote WACZ index (no download)");
                let fetch = crate::http_range::HttpFetch::open(u)?;
                let stats = index_wacz_streaming(
                    fetch.clone(),
                    &id,
                    &display_name,
                    &collection_id,
                    search,
                    u,
                    workers,
                    progress,
                )?;
                cache_thumbnail(
                    fetch,
                    &thumbs_dir,
                    &id,
                    main_page_url.as_deref(),
                    &crate::collections::pinned_thumb_path(home, &collection_id, &id),
                );
                stats
            }
            None => {
                let p = local.as_ref().unwrap();
                // CDX-guided when the WARCs are Stored (the WACZ spec's SHOULD, always
                // true for Browsertrix output) so a CDX offset maps to a byte
                // position; otherwise fall back to a full scan of every WARC record.
                if local_warcs_streamable(p).unwrap_or(false) {
                    let fetch = crate::http_range::FileFetch::open(p)
                        .with_context(|| format!("opening {} for CDX-guided index", p.display()))?;
                    let stats = index_wacz_streaming(
                        fetch.clone(),
                        &id,
                        &display_name,
                        &collection_id,
                        search,
                        &p.display().to_string(),
                        workers,
                        progress,
                    )?;
                    cache_thumbnail(
                        fetch,
                        &thumbs_dir,
                        &id,
                        main_page_url.as_deref(),
                        &crate::collections::pinned_thumb_path(home, &collection_id, &id),
                    );
                    stats
                } else {
                    // The scan path has no cheap up-front record total, so it stays on
                    // the spinner (no determinate bar). Label it "scanning" - it reads
                    // every WARC record, unlike the CDX-guided path.
                    if let Some(pr) = progress {
                        pr.phase("scanning");
                    }
                    index_wacz(p, &id, &display_name, &collection_id, search)?
                }
            }
        }
    };

    // Capture the outcome to report once the index is committed (see
    // `index_location`), not here - the commit could still fail.
    let outcome = (display_name.clone(), stats.pages);

    // Index the WACZ's metadata as a searchable document, tagged with its collection.
    let coll_body = build_collection_body(&meta);
    search
        .lock()
        .unwrap()
        .index_collection(&id, &display_name, &collection_id, &coll_body)?;

    // Fixity: a streamed remote is never fully read, so there's no whole-file
    // SHA-256 (empty; `verify` already skips remote sources). Its size comes
    // from the HTTP Content-Length. A local/downloaded file is hashed as before -
    // reading the whole file, which dominates the tail for a large local WACZ (so
    // it gets its own "checksumming" phase and timing, not lumped into indexing).
    let (sha, file_size) = match &remote_url {
        Some(u) => (
            String::new(),
            crate::http_range::open_remote(u)?.total_len(),
        ),
        None => {
            let p = local.as_ref().unwrap();
            if let Some(pr) = progress {
                pr.phase("checksumming");
            }
            let sha_start = std::time::Instant::now();
            let sha =
                file_sha256(p).with_context(|| format!("computing sha256 of {}", p.display()))?;
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            debug!(
                sha_ms = sha_start.elapsed().as_millis() as u64,
                bytes = size,
                "computed whole-file SHA-256 (fixity)"
            );
            (sha, size)
        }
    };
    let date_indexed = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    // Provenance: collect the software reported by the datapackage and by the
    // warcinfo record (deduped) - we don't label which crawled vs packaged.
    // operator/user-agent/robots come from warcinfo when present.
    let warcinfo = stats.warcinfo.unwrap_or_default();
    let mut software: Vec<String> = Vec::new();
    for s in meta.software.into_iter().chain(warcinfo.software) {
        if !software.contains(&s) {
            software.push(s);
        }
    }
    manifest.ensure_collection(&collection_id, &collection_name, &date_indexed);

    // Seed the collection's finding aid from this WACZ's datapackage — fill-gaps,
    // so only empty fields are set: the first indexed WACZ with a value wins and
    // a curator's edits are never overwritten. A single crawl's `description`
    // isn't really the whole collection's scope, but a draft beats a blank and
    // invites the curator to refine it.
    let year = meta.created.as_deref().and_then(year_prefix);
    let seed = crate::collections::CollectionFields {
        narrative: meta.description.clone(),
        subjects: (!meta.keywords.is_empty()).then(|| meta.keywords.clone()),
        dates: year,
        creator: meta.creator.clone(),
        rights: (!meta.licenses.is_empty()).then(|| meta.licenses.join(", ")),
        ..Default::default()
    };
    if !seed.is_empty() {
        manifest.seed_fields(&collection_id, &collection_name, &seed, &date_indexed);
    }

    // Preserve import provenance (set out-of-band by the importers) across a
    // reindex, which otherwise rebuilds the entry from scratch.
    let browsertrix = manifest.wacz_by_id(&id).and_then(|w| w.browsertrix.clone());
    let archive_it = manifest.wacz_by_id(&id).and_then(|w| w.archive_it.clone());

    manifest.upsert_wacz(Wacz {
        id,
        collection: collection_id,
        source: effective_source.clone(),
        name: display_name,
        date_indexed,
        file_size,
        sha256: sha,
        description: meta.description,
        crawl_date: meta.created,
        seed_pages: meta.seed_pages,
        software,
        operator: warcinfo.operator,
        user_agent: warcinfo.user_agent,
        robots: warcinfo.robots,
        page_count: Some(stats.pages),
        capture_start: stats.earliest_capture,
        capture_end: stats.latest_capture,
        browsertrix,
        archive_it,
        nested_waczs: stats.nested_waczs,
        // Provenance previously parsed-but-dropped / newly read.
        modified: meta.modified,
        is_part_of: warcinfo.is_part_of,
        hostname: warcinfo.hostname,
        conforms_to: warcinfo.conforms_to,
        keywords: meta.keywords,
        licenses: meta.licenses,
        status_counts: stats.status_counts,
    });

    // Note: the spinner/bar is *not* finished here - the Tantivy commit happens
    // once per `index_location` (after all sources), and that's where it's cleared
    // (via a final "committing" spinner) and the summary is emitted. See
    // `index_location`.
    Ok(outcome)
}

/// Download a remote WACZ to a temp file for indexing.
fn download_to_temp(url: &str) -> Result<tempfile::NamedTempFile> {
    use std::io::{copy, Write};

    let mut tmp = tempfile::Builder::new().suffix(".wacz").tempfile()?;
    let mut reader = crate::http_range::get_reader(url)?;
    copy(&mut reader, &mut tmp).with_context(|| format!("writing {url} to temp file"))?;
    tmp.flush()?;
    Ok(tmp)
}

/// Download a remote WACZ into `<home>/archive/<collection-slug>/<name>.wacz` and
/// return its path relative to `home` (so the manifest stores a portable local
/// source, and the archive is browsable by collection). The name comes from the
/// URL's last path segment. Downloads to a temp file first, then files it with
/// [`pick_archive_dest`] so a different WACZ that happens to share the name isn't
/// clobbered (and a re-download of identical bytes is reused). Used by `--download`.
fn download_into_archive(url: &str, home: &Path, collection_slug: &str) -> Result<PathBuf> {
    use std::io::{copy, Write};

    let stem = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("download");
    let name = if stem.ends_with(".wacz") {
        stem.to_string()
    } else {
        format!("{stem}.wacz")
    };

    let dir = collection_archive_dir(home, collection_slug);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating archive dir {}", dir.display()))?;

    // Download into a temp file in the same dir (same filesystem → cheap rename),
    // then choose a non-clobbering final name.
    let mut tmp = tempfile::Builder::new()
        .prefix(".download-")
        .suffix(".wacz")
        .tempfile_in(&dir)
        .with_context(|| format!("temp file in {}", dir.display()))?;
    copy(&mut crate::http_range::get_reader(url)?, &mut tmp)
        .with_context(|| format!("writing {url} to a temp file"))?;
    tmp.flush()?;

    let dest = pick_archive_dest(&dir, std::ffi::OsStr::new(&name), tmp.path())?;
    if !dest.exists() {
        // `persist` renames the temp file into place (and drops the temp guard).
        tmp.persist(&dest)
            .with_context(|| format!("saving download to {}", dest.display()))?;
    } // else: byte-identical file already downloaded here; the temp is discarded.

    // Home-relative path (portable), using the possibly-disambiguated file name.
    let final_name = dest.file_name().unwrap_or(std::ffi::OsStr::new(&name));
    Ok(PathBuf::from("archive")
        .join(collection_slug)
        .join(final_name))
}

/// Whether a remote WACZ can be stream-indexed: reachable, range-capable, and
/// its WARC entries Stored (uncompressed). Reads only the ZIP central directory.
fn remote_warcs_streamable(url: &str) -> Result<bool> {
    let reader = crate::http_range::open_remote(url)?;
    let mut zip = zip::ZipArchive::new(reader)
        .with_context(|| format!("reading remote ZIP central directory of {url}"))?;
    crate::wacz::warcs_stored(&mut zip)
}

/// Whether a local WACZ can be CDX-guided: its `archive/` WARC entries are Stored
/// (uncompressed), so a CDX byte offset maps to an absolute position. The file
/// counterpart of [`remote_warcs_streamable`]; reads only the central directory.
fn local_warcs_streamable(path: &Path) -> Result<bool> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("reading ZIP central directory of {}", path.display()))?;
    crate::wacz::warcs_stored(&mut zip)
}

/// Display name for a source: the WACZ filename stem, for a file or URL.
fn source_display_name(source: &Source) -> String {
    match source {
        Source::File(p) => file_display_name(p),
        Source::Url(u) => {
            let path = u.split(['?', '#']).next().unwrap_or(u);
            let base = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(u);
            base.strip_suffix(".wacz").unwrap_or(base).to_string()
        }
        Source::Browsertrix { resource, .. } | Source::BrowsertrixPublic { resource, .. } => {
            resource
                .strip_suffix(".wacz")
                .unwrap_or(resource)
                .to_string()
        }
    }
}

/// Build the body text for a collection-level Tantivy document from its metadata.
fn build_collection_body(meta: &crate::wacz::WaczMetadata) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(desc) = &meta.description {
        parts.push(desc.clone());
    }
    for page in &meta.seed_pages {
        if let Some(title) = &page.title {
            parts.push(title.clone());
        }
        parts.push(page.url.clone());
    }
    parts.join(" ")
}

/// A raw contribution to a page's search document, parsed from one WARC record.
enum RawRecord {
    /// An HTML response: source of the page title, description, headings, and a
    /// scraped-text fallback body. (PDF responses reuse this variant with just a
    /// title and body.)
    Html {
        url: String,
        timestamp: String,
        title: String,
        body: String,
        description: String,
        headings: String,
        keywords: String,
        author: String,
        /// `"html"` or `"pdf"`.
        media_type: String,
        /// `<html lang>` value (empty for PDFs).
        lang: String,
        /// HTTP response status code, if known.
        status: Option<u16>,
        /// Year from the HTTP `Last-Modified` header, if present.
        modified_year: Option<u64>,
    },
    /// Fully rendered (post-JS) page text - Browsertrix's `urn:text:` resource
    /// record, or the `text` field from `pages/*.jsonl`. Richer than scraped
    /// HTML, especially for SPAs. `title` is set only for the pages.jsonl source
    /// (used as a fallback when the HTML capture has no title).
    Text {
        url: String,
        timestamp: String,
        text: String,
        title: Option<String>,
    },
}

/// Accumulated per-URL data merged from all WARC records for that page.
#[derive(Default)]
struct MergedPage {
    timestamp: String,
    title: Option<String>,
    html_body: Option<String>,
    rendered_text: Option<String>,
    description: Option<String>,
    headings: Option<String>,
    keywords: Option<String>,
    author: Option<String>,
    media_type: Option<String>,
    lang: Option<String>,
    status: Option<u16>,
    modified_year: Option<u64>,
}

/// Provenance and capture stats gathered during one WACZ indexing pass, so the
/// manifest can record them without re-reading the WARCs.
#[derive(Default, Debug)]
struct CrawlStats {
    pages: u64,
    earliest_capture: Option<String>,
    latest_capture: Option<String>,
    warcinfo: Option<Warcinfo>,
    /// For a nested multi-WACZ: how many inner WACZs were flattened into this
    /// crawl. `None` for an ordinary (flat) WACZ.
    nested_waczs: Option<u64>,
    /// HTTP status-code histogram tallied from the CDX (every capture, including
    /// the bodyless 4xx/5xx that never become search documents) — the derived
    /// "capture quality" / Appraisal signal.
    status_counts: BTreeMap<u16, u64>,
}

/// Tally HTTP status codes across every CDX record — the capture-quality signal.
fn tally_status(cdx: &[crate::wacz::CdxjRecord]) -> BTreeMap<u16, u64> {
    let mut counts = BTreeMap::new();
    for rec in cdx {
        if rec.status != 0 {
            *counts.entry(rec.status).or_insert(0) += 1;
        }
    }
    counts
}

/// Detect + index a **nested multi-WACZ** (a WACZ whose payload is other WACZ
/// files; see [`crate::wacz::nested_wacz_locations`]) — e.g. Browsertrix's
/// combined collection `/download`. Each inner `.wacz` is indexed under this
/// (outer) crawl's id, so the whole thing stays one manifest entry (flatten).
/// Returns `None` when the WACZ isn't nested, so the caller falls through to
/// ordinary indexing. Replay is unaffected — wabac.js already resolves nesting.
///
/// A Stored inner WACZ is a contiguous byte window of the outer file, so it's
/// streamed **in place** via a [`SubRangeFetch`] — no extraction, and a remote
/// outer fetches only the ranges it needs. (Only a compressed inner entry, or an
/// inner whose own WARCs aren't Stored, has to be materialized to a temp file —
/// not the Browsertrix case.)
#[allow(clippy::too_many_arguments)]
fn index_nested(
    local: Option<&Path>,
    remote_url: Option<&str>,
    crawl_id: &str,
    crawl_name: &str,
    collection: &str,
    search: &Mutex<SearchIndex>,
    workers: usize,
    progress: Option<&dyn IndexProgress>,
) -> Result<Option<CrawlStats>> {
    match (local, remote_url) {
        (Some(p), _) => {
            let fetch = crate::http_range::FileFetch::open(p)?;
            index_nested_from(
                fetch, crawl_id, crawl_name, collection, search, workers, progress,
            )
        }
        (None, Some(u)) => {
            let fetch = crate::http_range::HttpFetch::open(u)?;
            index_nested_from(
                fetch, crawl_id, crawl_name, collection, search, workers, progress,
            )
        }
        _ => Ok(None),
    }
}

/// Core of [`index_nested`], generic over the outer WACZ's byte source
/// (`FileFetch` locally, `HttpFetch` remotely).
#[allow(clippy::too_many_arguments)]
fn index_nested_from<F: RangeFetch + Clone + Send + Sync>(
    outer: F,
    crawl_id: &str,
    crawl_name: &str,
    collection: &str,
    search: &Mutex<SearchIndex>,
    workers: usize,
    progress: Option<&dyn IndexProgress>,
) -> Result<Option<CrawlStats>> {
    let inners = {
        let mut zip = zip::ZipArchive::new(RangeReader::new(outer.clone()))
            .context("opening WACZ to check for nesting")?;
        crate::wacz::nested_wacz_locations(&mut zip)
    };
    if inners.is_empty() {
        return Ok(None);
    }
    info!(count = inners.len(), "indexing a nested multi-WACZ");

    let mut agg = CrawlStats::default();
    for (i, inner) in inners.iter().enumerate() {
        if let Some(pr) = progress {
            pr.phase(&format!("nested WACZ {}/{}", i + 1, inners.len()));
        }
        let stats = match inner.inline {
            // Stored: read it in place as a window of the outer file.
            Some((base, len)) => index_inner(
                crate::http_range::SubRangeFetch::new(outer.clone(), base, len),
                &inner.name,
                crawl_id,
                crawl_name,
                collection,
                search,
                workers,
                progress,
            )?,
            // Compressed inner entry: extract it (decompressing) to a temp file,
            // then full-scan it. Rare — not produced by Browsertrix.
            None => {
                let mut zip = zip::ZipArchive::new(RangeReader::new(outer.clone()))?;
                let mut tmp =
                    tempfile::NamedTempFile::new().context("temp for compressed nested WACZ")?;
                std::io::copy(&mut zip.by_name(&inner.name)?, tmp.as_file_mut())
                    .with_context(|| format!("extracting nested {}", inner.name))?;
                index_wacz(tmp.path(), crawl_id, crawl_name, collection, search)?
            }
        };
        agg.pages += stats.pages;
        merge_min(&mut agg.earliest_capture, stats.earliest_capture);
        merge_max(&mut agg.latest_capture, stats.latest_capture);
        if agg.warcinfo.is_none() {
            agg.warcinfo = stats.warcinfo;
        }
        for (code, n) in stats.status_counts {
            *agg.status_counts.entry(code).or_insert(0) += n;
        }
    }
    agg.nested_waczs = Some(inners.len() as u64);
    Ok(Some(agg))
}

/// Index one inner WACZ presented as a [`RangeFetch`] window. CDX-guided
/// (streaming, no extraction) when its WARCs are Stored; otherwise the window is
/// materialized to a temp file and full-scanned (rare).
#[allow(clippy::too_many_arguments)]
fn index_inner<F: RangeFetch + Clone + Send + Sync>(
    fetch: F,
    label: &str,
    crawl_id: &str,
    crawl_name: &str,
    collection: &str,
    search: &Mutex<SearchIndex>,
    workers: usize,
    progress: Option<&dyn IndexProgress>,
) -> Result<CrawlStats> {
    let streamable = zip::ZipArchive::new(RangeReader::new(fetch.clone()))
        .ok()
        .map(|mut z| crate::wacz::warcs_stored(&mut z).unwrap_or(false))
        .unwrap_or(false);
    if streamable {
        index_wacz_streaming(
            fetch, crawl_id, crawl_name, collection, search, label, workers, progress,
        )
    } else {
        let tmp = materialize_fetch(&fetch).context("materializing nested WACZ for scan")?;
        index_wacz(tmp.path(), crawl_id, crawl_name, collection, search)
    }
}

/// Copy an entire [`RangeFetch`] to a temp file (chunked), for the fallback
/// paths that need a seekable local file.
fn materialize_fetch<F: RangeFetch>(fetch: &F) -> Result<tempfile::NamedTempFile> {
    use std::io::Write;
    const CHUNK: u64 = 4 * 1024 * 1024;
    let total = fetch.total_len();
    let mut tmp = tempfile::NamedTempFile::new()?;
    let mut pos = 0u64;
    while pos < total {
        let end = (pos + CHUNK).min(total);
        tmp.as_file_mut().write_all(&fetch.fetch(pos, end)?)?;
        pos = end;
    }
    Ok(tmp)
}

/// Keep the smaller of two optional 14-digit capture timestamps (they sort
/// lexicographically), for aggregating a nested WACZ's capture range.
fn merge_min(acc: &mut Option<String>, v: Option<String>) {
    if let Some(v) = v {
        if acc.as_ref().is_none_or(|a| v < *a) {
            *acc = Some(v);
        }
    }
}
/// Keep the larger of two optional 14-digit capture timestamps.
fn merge_max(acc: &mut Option<String>, v: Option<String>) {
    if let Some(v) = v {
        if acc.as_ref().is_none_or(|a| v > *a) {
            *acc = Some(v);
        }
    }
}

/// Index all WARC entries inside a WACZ file into the Tantivy full-text index.
///
/// Records are collected across every inner WARC (rendered `urn:text:` records
/// often live in a separate WARC from the HTML response), merged into one
/// document per URL, and indexed once. The body prefers Browsertrix's rendered
/// text and falls back to scraped HTML; the title comes from the HTML.
fn index_wacz(
    wacz_path: &Path,
    // WACZ id/name (tagged on each page as crawl_id/crawl_name).
    crawl_id: &str,
    crawl_name: &str,
    // Curated collection id (slug) the WACZ belongs to.
    collection: &str,
    search: &Mutex<SearchIndex>,
) -> Result<CrawlStats> {
    let warc_paths: Vec<_> = iter_warc_paths(wacz_path)?
        .collect::<Result<Vec<_>>>()
        .with_context(|| format!("listing WARC entries in {}", wacz_path.display()))?;

    let per_warc: Vec<(Vec<RawRecord>, Option<Warcinfo>)> = warc_paths
        .par_iter()
        .map(|entry_name| {
            let tmp = extract_warc_from_wacz(wacz_path, entry_name).with_context(|| {
                format!("extracting {} from {}", entry_name, wacz_path.display())
            })?;
            collect_page_records(tmp.path())
        })
        .collect::<Result<Vec<_>>>()?;

    // Flatten to all records + the first warcinfo (warcinfo leads its WARC, so
    // the first WARC's is the crawl-level record), then merge and index.
    let mut warcinfo: Option<Warcinfo> = None;
    let mut raws: Vec<RawRecord> = Vec::new();
    for (r, wi) in per_warc {
        if warcinfo.is_none() {
            warcinfo = wi;
        }
        raws.extend(r);
    }
    // Fold in pages.jsonl/extraPages.jsonl extracted text (see the streaming
    // path); some crawls store rendered text only there, not in the WARCs. Read
    // the CDX here too for the capture-quality status tally (every capture,
    // including bodyless 4xx/5xx that never became RawRecords).
    let mut status_counts = BTreeMap::new();
    if let Ok(file) = std::fs::File::open(wacz_path) {
        if let Ok(mut zip) = zip::ZipArchive::new(file) {
            raws.extend(
                crate::wacz::read_page_texts(&mut zip)
                    .into_iter()
                    .map(|pt| RawRecord::Text {
                        url: pt.url,
                        timestamp: pt.ts,
                        text: pt.text,
                        title: pt.title,
                    }),
            );
            if let Ok(cdx) = crate::wacz::cdx_records(&mut zip) {
                status_counts = tally_status(&cdx);
            }
        }
    }
    index_merged(
        raws,
        warcinfo,
        status_counts,
        crawl_id,
        crawl_name,
        collection,
        search,
        &wacz_path.display().to_string(),
    )
}

/// Index a WACZ by CDX-guided/streaming extraction over a `Read + Seek` source
/// (a local file or an HTTP range reader): read only the page-relevant records
/// the CDX points at, rather than scanning every WARC record. Produces the same
/// index as [`index_wacz`] (both share [`record_to_raw`] and [`index_merged`]).
#[allow(clippy::too_many_arguments)]
fn index_wacz_streaming<F>(
    fetch: F,
    crawl_id: &str,
    crawl_name: &str,
    collection: &str,
    search: &Mutex<SearchIndex>,
    label: &str,
    concurrency: usize,
    progress: Option<&dyn IndexProgress>,
) -> Result<CrawlStats>
where
    F: crate::http_range::RangeFetch + Clone + Send + Sync,
{
    let (raws, warcinfo, status_counts) =
        collect_page_records_via_cdx(fetch, concurrency, progress)?;
    index_merged(
        raws,
        warcinfo,
        status_counts,
        crawl_id,
        crawl_name,
        collection,
        search,
        label,
    )
}

/// Merge per-record contributions into one document per URL and index them.
/// Shared by the scan-everything ([`index_wacz`]) and CDX-guided
/// ([`index_wacz_streaming`]) paths.
#[allow(clippy::too_many_arguments)]
fn index_merged(
    raws: Vec<RawRecord>,
    warcinfo: Option<Warcinfo>,
    status_counts: BTreeMap<u16, u64>,
    crawl_id: &str,
    crawl_name: &str,
    collection: &str,
    search: &Mutex<SearchIndex>,
    label: &str,
) -> Result<CrawlStats> {
    let build_start = std::time::Instant::now();
    let mut pages: HashMap<String, MergedPage> = HashMap::new();
    {
        for raw in raws {
            match raw {
                RawRecord::Html {
                    url,
                    timestamp,
                    title,
                    body,
                    description,
                    headings,
                    keywords,
                    author,
                    media_type,
                    lang,
                    status,
                    modified_year,
                } => {
                    let e = pages.entry(url).or_default();
                    // The HTML capture is the authoritative timestamp for replay.
                    e.timestamp = timestamp;
                    if !title.is_empty() {
                        e.title = Some(title);
                    }
                    if !body.is_empty() {
                        e.html_body = Some(body);
                    }
                    if !description.is_empty() {
                        e.description = Some(description);
                    }
                    if !headings.is_empty() {
                        e.headings = Some(headings);
                    }
                    if !keywords.is_empty() {
                        e.keywords = Some(keywords);
                    }
                    if !author.is_empty() {
                        e.author = Some(author);
                    }
                    if !media_type.is_empty() {
                        e.media_type = Some(media_type);
                    }
                    if !lang.is_empty() {
                        e.lang = Some(lang);
                    }
                    if status.is_some() {
                        e.status = status;
                    }
                    if modified_year.is_some() {
                        e.modified_year = modified_year;
                    }
                }
                RawRecord::Text {
                    url,
                    timestamp,
                    text,
                    title,
                } => {
                    let e = pages.entry(url).or_default();
                    if e.timestamp.is_empty() {
                        e.timestamp = timestamp;
                    }
                    e.rendered_text = Some(text);
                    // Rendered text always comes from an HTML page.
                    e.media_type.get_or_insert_with(|| "html".to_string());
                    // A pages.jsonl title fills in only when the HTML capture
                    // gave none - the scraped HTML <title> wins when present.
                    if let Some(t) = title {
                        if !t.is_empty() {
                            e.title.get_or_insert(t);
                        }
                    }
                }
            }
        }
    }

    let mut count = 0u64;
    let mut earliest: Option<String> = None;
    let mut latest: Option<String> = None;
    {
        use crate::search::Page;
        let mut s = search.lock().unwrap();
        for (url, m) in pages {
            // Prefer the fully rendered text; fall back to scraped HTML.
            let body = m.rendered_text.or(m.html_body).unwrap_or_default();
            let title = m.title.unwrap_or_default();
            let description = m.description.unwrap_or_default();
            let headings = m.headings.unwrap_or_default();
            let keywords = m.keywords.unwrap_or_default();
            let author = m.author.unwrap_or_default();
            let media_type = m.media_type.unwrap_or_default();
            let lang = m.lang.unwrap_or_default();
            if title.is_empty() && body.is_empty() && description.is_empty() {
                continue;
            }
            s.index_page(&Page {
                url: &url,
                timestamp: &m.timestamp,
                title: &title,
                body: &body,
                description: &description,
                headings: &headings,
                keywords: &keywords,
                author: &author,
                media_type: &media_type,
                lang: &lang,
                status: m.status,
                modified_year: m.modified_year,
                crawl_id,
                crawl_name,
                collection,
            })?;
            count += 1;
            // Track the capture date range (14-digit timestamps sort
            // chronologically as plain strings).
            if !m.timestamp.is_empty() {
                if earliest.as_deref().is_none_or(|e| m.timestamp.as_str() < e) {
                    earliest = Some(m.timestamp.clone());
                }
                if latest.as_deref().is_none_or(|l| m.timestamp.as_str() > l) {
                    latest = Some(m.timestamp.clone());
                }
            }
        }
    }

    debug!(build_ms = build_start.elapsed().as_millis() as u64, wacz = %label, "built index");
    info!(pages = count, wacz = %label, "indexed pages from WACZ");
    Ok(CrawlStats {
        pages: count,
        earliest_capture: earliest,
        latest_capture: latest,
        warcinfo,
        // Set by index_nested for a multi-WACZ; a single WACZ isn't nested.
        nested_waczs: None,
        status_counts,
    })
}

/// Parse an extracted WARC file into raw per-record contributions (HTML
/// responses and `urn:text:` rendered-text resources). Other record types
/// (images, JS, CSS, redirects, other `urn:` pseudo-records) are ignored.
fn collect_page_records(warc_path: &Path) -> Result<(Vec<RawRecord>, Option<Warcinfo>)> {
    let records: Vec<WarcRecord> = iter_records(warc_path)
        .with_context(|| format!("reading {}", warc_path.display()))?
        .collect::<Result<Vec<_>>>()?;

    let mut out = Vec::new();
    let mut warcinfo: Option<Warcinfo> = None;
    for record in &records {
        // Capture the crawl's warcinfo (checked before the URI gate in
        // record_to_raw, since warcinfo records carry no WARC-Target-URI).
        if warcinfo.is_none() {
            if let Some(info) = Warcinfo::from_record(record) {
                if !info.is_empty() {
                    warcinfo = Some(info);
                }
            }
        }
        if let Some(raw) = record_to_raw(record) {
            out.push(raw);
        }
    }

    Ok((out, warcinfo))
}

/// Page records read from a WACZ, the crawl-level `warcinfo`, and the HTTP status
/// tally over the CDX (the capture-quality signal).
type PageRecords = (Vec<RawRecord>, Option<Warcinfo>, BTreeMap<u16, u64>);

/// CDX-guided extraction over a `Read + Seek` WACZ: read the CDX, fetch only the
/// page-relevant records (HTML/PDF responses and `urn:text:` rendered text) by
/// seeking to `data_start + offset`, and transform each with [`record_to_raw`].
/// Images/JS/JSON/pageinfo/thumbnail captures are never fetched. Streaming
/// indexes exactly what the CDX lists (authoritative for Browsertrix WACZs).
fn collect_page_records_via_cdx<F>(
    fetch: F,
    concurrency: usize,
    progress: Option<&dyn IndexProgress>,
) -> Result<PageRecords>
where
    F: crate::http_range::RangeFetch + Clone + Send + Sync,
{
    use crate::wacz;
    use std::sync::atomic::{AtomicU64, Ordering};

    if let Some(p) = progress {
        p.phase("reading index");
    }
    let read_start = std::time::Instant::now();

    // Setup (serial): read the ZIP central directory, the CDX, each WARC's
    // data-start, and the warcinfo over a buffered range reader.
    let mut zip = zip::ZipArchive::new(crate::http_range::RangeReader::new(fetch.clone()))
        .context("opening WACZ ZIP")?;
    wacz::ensure_warcs_stored(&mut zip)?;
    let cdx = wacz::cdx_records(&mut zip)?;
    let status_counts = tally_status(&cdx);
    let starts = wacz::warc_data_starts(&mut zip)?;
    let warcinfo = wacz::find_warcinfo_streaming(&mut zip)?;
    // Extracted page text from pages.jsonl/extraPages.jsonl (read once, while the
    // ZIP is open). Some crawls store rendered text only here - not as urn:text:
    // WARC records the CDX points at - so without this that text is unsearchable.
    let page_texts = wacz::read_page_texts(&mut zip);
    drop(zip);

    // Records that can become a page; skip media/pseudo-records. The count is the
    // determinate-bar total (each is one fetch + extract).
    let wanted: Vec<&crate::wacz::CdxjRecord> = cdx
        .iter()
        .filter(|c| {
            c.length != 0
                && (c.url.starts_with("urn:text:")
                    || c.mime.contains("html")
                    || c.mime.contains("pdf"))
        })
        .collect();
    if let Some(p) = progress {
        p.set_total(wanted.len() as u64);
    }

    // Fetch + extract each wanted record concurrently. The CDX gives every record
    // an independent (offset, length), so fetches don't depend on each other:
    // fanning out hides per-record round-trip latency (the win for remote WACZs)
    // and parallelizes HTML/PDF text extraction (CPU) across cores.
    let done = AtomicU64::new(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(concurrency)
        .build()
        .context("building record-fetch thread pool")?;
    let mut out: Vec<RawRecord> = pool.install(|| {
        wanted
            .par_iter()
            .flat_map_iter(|c| {
                let base = c.filename.rsplit('/').next().unwrap_or(&c.filename);
                let raws: Vec<RawRecord> = match starts.get(base) {
                    Some(&start) => {
                        let (from, len) = (start + c.offset, c.length);
                        match fetch
                            .fetch(from, from + len)
                            .map_err(anyhow::Error::from)
                            .and_then(|bytes| wacz::records_from_slice(&bytes, from, len))
                        {
                            Ok(records) => records.iter().filter_map(record_to_raw).collect(),
                            Err(e) => {
                                tracing::warn!(url = %c.url, "skipping unreadable CDX record: {e:#}");
                                Vec::new()
                            }
                        }
                    }
                    None => Vec::new(),
                };
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if let Some(p) = progress {
                    p.set_records(n);
                }
                raws
            })
            .collect()
    });

    // Fold in the pages.jsonl/extraPages.jsonl text as rendered-text records;
    // index_merged merges them into the matching page docs by URL (most already
    // exist from their HTML response, so this enriches rather than duplicates).
    let page_text_count = page_texts.len();
    out.extend(page_texts.into_iter().map(|pt| RawRecord::Text {
        url: pt.url,
        timestamp: pt.ts,
        text: pt.text,
        title: pt.title,
    }));

    debug!(
        records = wanted.len(),
        page_texts = page_text_count,
        read_ms = read_start.elapsed().as_millis() as u64,
        concurrency,
        "read page records via CDX"
    );
    // Records read. The merge + Tantivy indexing that follows has no per-record
    // total, so drop the determinate bar back to a spinner - otherwise it sits at
    // 100% with a decaying rate/ETA during the slow tail (very visible for a local
    // file, where reads are near-instant and the tail dominates).
    if let Some(p) = progress {
        p.phase("building index");
    }
    Ok((out, warcinfo, status_counts))
}

/// Cache a representative thumbnail for a crawl (best-effort). Any failure - no
/// main page, no `og:image`, or an image we can't fetch/decode - is logged at
/// debug and ignored; the UI falls back to a CSS placeholder.
fn cache_thumbnail<F>(
    fetch: F,
    thumbs_dir: &Path,
    crawl_id: &str,
    main_page_url: Option<&str>,
    pinned_dest: &Path,
) where
    F: crate::http_range::RangeFetch + Clone + Send + Sync,
{
    let Some(url) = main_page_url else {
        return;
    };
    match crate::thumbnail::generate(fetch, thumbs_dir, crawl_id, url, pinned_dest) {
        Ok(true) => debug!(crawl_id, "cached representative thumbnail"),
        Ok(false) => {}
        Err(e) => debug!(crawl_id, "thumbnail generation failed: {e:#}"),
    }
}

/// Per-host ceiling on fetch concurrency. Even when a user asks for more (or the
/// local core count is very high), we never run more than this many concurrent
/// range requests against a single WACZ's host — a proactive politeness cap that
/// bounds simultaneous load and defends against a mis-typed `--concurrency`. 64 is
/// generous for object stores like S3 while still a hard bound for small servers.
const MAX_CONCURRENCY: usize = 64;

/// Default worker count for the concurrent CDX-guided record fetch+extract, when
/// `--concurrency` isn't given.
///
/// Remote defaults to a deliberately gentle 4: a single WACZ's requests all hit
/// one host, and indice is meant to be pointed at arbitrary (often small)
/// servers, so it's polite by default while still ~4x faster than serial. Users
/// hitting an object store (e.g. S3) can raise it with `--concurrency`.
///
/// Local defaults to the core count: it's your own disk (no politeness concern)
/// and the work is CPU-bound text extraction, so cores are the sweet spot.
fn default_concurrency(remote: bool) -> usize {
    if remote {
        4
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    }
}

/// Transform one WARC record into an indexable [`RawRecord`], or `None` if it is
/// not a page: warcinfo, `dns:`, other `urn:` pseudo-records (pageinfo,
/// thumbnail, …), non-HTML/PDF responses, or empty payloads. Shared by the
/// scan-everything path ([`collect_page_records`]) and the CDX-guided path
/// ([`collect_page_records_via_cdx`]) so both index identically.
fn record_to_raw(record: &WarcRecord) -> Option<RawRecord> {
    let uri = record.target_uri.as_str();
    if uri.is_empty() || uri.starts_with("dns:") {
        return None;
    }

    // Browsertrix stores fully rendered page text as a `urn:text:<url>` resource
    // record (WARC-Type: resource). Map it back to the real URL as the body.
    if let Some(real_url) = uri.strip_prefix("urn:text:") {
        let text = String::from_utf8_lossy(&record.payload).trim().to_string();
        if text.is_empty() {
            return None;
        }
        return Some(RawRecord::Text {
            url: real_url.to_string(),
            timestamp: record.timestamp.clone(),
            text,
            title: None,
        });
    }

    // Skip other urn: pseudo-records (pageinfo, thumbnail, view, …).
    if uri.starts_with("urn:") || !record.warc_type.eq_ignore_ascii_case("response") {
        return None;
    }
    let mime = record.content_type.to_ascii_lowercase();

    // PDF responses: extract text as the body, title from the URL's filename.
    if mime.contains("pdf") {
        if record.payload.is_empty() {
            return None;
        }
        let Some(text) = crate::pdf::extract_pdf_text(&record.payload) else {
            debug!(url = uri, "PDF text extraction yielded nothing; skipping");
            return None;
        };
        return Some(RawRecord::Html {
            url: uri.to_string(),
            timestamp: record.timestamp.clone(),
            title: pdf_title_from_url(uri),
            body: text,
            description: String::new(),
            headings: String::new(),
            keywords: String::new(),
            author: String::new(),
            media_type: "pdf".to_string(),
            lang: String::new(),
            status: record.http_status,
            modified_year: last_modified_year(&record.http_headers),
        });
    }

    if !mime.contains("html") || record.payload.is_empty() {
        return None;
    }
    let html = extract_html_text(&record.payload);
    if html.title.is_empty() && html.body.is_empty() && html.description.is_empty() {
        return None;
    }
    Some(RawRecord::Html {
        url: uri.to_string(),
        timestamp: record.timestamp.clone(),
        title: html.title,
        body: html.body,
        description: html.description,
        headings: html.headings,
        keywords: html.keywords,
        author: html.author,
        media_type: "html".to_string(),
        lang: html.lang,
        status: record.http_status,
        modified_year: last_modified_year(&record.http_headers),
    })
}

/// The year from an HTTP `Last-Modified` header, or `None` if the header is
/// absent or unparseable. Only the modern IMF-fixdate form (RFC 7231, e.g.
/// `Wed, 21 Oct 2015 07:28:00 GMT`) is parsed — the two obsolete HTTP-date
/// formats (RFC 850 and asctime) are rare and yield `None`.
fn last_modified_year(headers: &[(String, String)]) -> Option<u64> {
    let value = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("last-modified"))
        .map(|(_, v)| v.as_str())?;
    // IMF-fixdate is RFC 2822-compatible (chrono accepts the "GMT" zone).
    let dt = chrono::DateTime::parse_from_rfc2822(value.trim()).ok()?;
    let year = chrono::Datelike::year(&dt);
    (year > 0).then_some(year as u64)
}

/// Derive a page title for a PDF from the last path segment of its URL
/// (e.g. `https://x.org/docs/report.pdf` -> `report.pdf`), falling back to the
/// full URL when there is no usable segment.
fn pdf_title_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/')
        .find(|seg| !seg.is_empty())
        .unwrap_or(url)
        .to_string()
}

/// Strip archive extensions to get a clean display name.
fn file_display_name(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    for suffix in &[".warc.gz", ".warc", ".wacz"] {
        if let Some(stem) = name.strip_suffix(suffix) {
            return stem.to_string();
        }
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests;
