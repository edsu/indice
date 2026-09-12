//! Acquisition: turning an `index` argument into something readable.
//!
//! Two jobs. [`resolve_sources`] validates the argument and files a local WACZ
//! into the collection's archive folder (the *where does it live* question).
//! [`open`] then decides *how it will be read* — in place, downloaded, or
//! streamed over HTTP ranges — and hands back a [`WaczAccess`] handle so the rest
//! of the pipeline never has to re-ask.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

use crate::collections::{file_sha256, Manifest, Source};
use crate::index::paths::archive_dir;
use crate::index::{IndexProgress, SourceResolver};

use super::WaczAccess;

/// Decide how a source will be read and return it alongside the **effective**
/// source (what the manifest records — `--download` turns a URL into a local
/// file, but a Browsertrix source keeps its stable identity):
///
/// - a local file: read in place;
/// - a remote URL with `--download`: fetched into `<home>/archive` and adopted as
///   a local file (durable/offline);
/// - a remote URL (default): streamed over HTTP range requests — but only if its
///   WARCs are Stored; if they're deflated, fall back to a temp download + scan,
///   keeping the URL as the source;
/// - a Browsertrix resource: resolved to a fresh presigned URL (they expire) and
///   streamed; the recorded source stays the stable Browsertrix identity, so the
///   id survives re-imports and replay re-resolves later.
pub(super) fn open(
    source: &Source,
    home: &Path,
    collection_slug: &str,
    download: bool,
    resolver: Option<&dyn SourceResolver>,
    progress: &dyn IndexProgress,
) -> Result<(Source, WaczAccess)> {
    let effective_source: Source = match source {
        Source::Url(u) if download => {
            info!(url = %u, "downloading remote WACZ into archive");
            progress.phase("downloading");
            Source::File(download_into_archive(u, home, collection_slug)?)
        }
        _ => source.clone(),
    };

    let access = match &effective_source {
        Source::File(_) => WaczAccess::Local {
            // A File source always resolves against `home`.
            path: effective_source.resolve(home).unwrap(),
            _tmp: None,
        },
        Source::Url(u) => {
            if remote_warcs_streamable(u).unwrap_or(false) {
                WaczAccess::Stream { url: u.clone() }
            } else {
                info!(url = %u, "remote WACZ can't be streamed (no range support or compressed WARCs); downloading to index");
                progress.phase("downloading");
                let tmp = download_to_temp(u).with_context(|| format!("downloading {u}"))?;
                let path = tmp.path().to_path_buf();
                // The temp file must outlive the read, so the handle rides along.
                WaczAccess::Local {
                    path,
                    _tmp: Some(tmp),
                }
            }
        }
        bt @ (Source::Browsertrix { .. } | Source::BrowsertrixPublic { .. }) => {
            progress.phase("resolving");
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
            WaczAccess::Stream { url }
        }
    };

    Ok((effective_source, access))
}

/// Turn one `index` argument into a source to index, filing local WACZs into the
/// collection's archive folder. An `http(s)://` URL yields a URL source. A local
/// `.wacz` file may live anywhere: it's brought into `<home>/archive/<slug>/` —
/// **moved** if it already sits under `archive/` (reorganized within indice's
/// own space), **copied** otherwise (the original is left intact) — so the home
/// directory stays self-contained and portable and the archive is browsable by
/// collection. Directories and non-`.wacz` paths are errors with guidance.
pub(super) fn resolve_sources(
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
pub(super) fn local_warcs_streamable(path: &Path) -> Result<bool> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("reading ZIP central directory of {}", path.display()))?;
    crate::wacz::warcs_stored(&mut zip)
}

/// Display name for a source: the WACZ filename stem, for a file or URL.
pub(super) fn source_display_name(source: &Source) -> String {
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

/// Strip archive extensions to get a clean display name.
pub(super) fn file_display_name(path: &Path) -> String {
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
