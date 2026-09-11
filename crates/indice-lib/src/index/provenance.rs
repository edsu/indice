//! Import-provenance setters (Browsertrix, Archive-It) recorded on already-indexed
//! manifest entries.

use std::path::Path;

use anyhow::{Context, Result};

use crate::collections::{wacz_id, BrowsertrixRef, Manifest, Source};
use crate::identity::SubjectId;

use super::paths::index_dir;

/// Record Browsertrix import provenance on an already-indexed WACZ. `wacz_file`
/// is the local file (under `<home>/archive`) that was just indexed; it's looked
/// up by the same id indexing assigns (a hash of its home-relative path). Used
/// by the importer for provenance display and incremental re-sync. Manifest-only
/// side effect (no reindex).
pub fn set_browsertrix_provenance(
    home: &Path,
    wacz_file: &Path,
    host: &str,
    item_id: &str,
    resource_hash: &str,
    review_status: Option<u8>,
) -> Result<()> {
    let abs = wacz_file
        .canonicalize()
        .with_context(|| format!("resolving {}", wacz_file.display()))?;
    let id = wacz_id(&Source::for_file(&abs, home));
    set_browsertrix_provenance_by_id(home, &id, host, item_id, resource_hash, review_status)
}

/// As [`set_browsertrix_provenance`], but for a crawl identified by its id — used
/// by the streaming importer, whose source is a [`Source::Browsertrix`] with no
/// local file (its id is `wacz_id(&source)`).
pub fn set_browsertrix_provenance_by_id(
    home: &Path,
    crawl_id: &str,
    host: &str,
    item_id: &str,
    resource_hash: &str,
    review_status: Option<u8>,
) -> Result<()> {
    let mut manifest = Manifest::open(&index_dir(home))?;
    let wacz = manifest
        .waczs
        .iter_mut()
        .find(|w| w.id == crawl_id)
        .with_context(|| format!("no indexed crawl with id {crawl_id}"))?;
    wacz.browsertrix = Some(BrowsertrixRef {
        host: host.to_string(),
        item_id: item_id.to_string(),
        resource_hash: resource_hash.to_string(),
        review_status,
    });
    manifest.save()?;
    Ok(())
}

/// Record Archive-It import provenance on an already-indexed crawl (by its id),
/// so a re-run can skip it. Mirrors [`set_browsertrix_provenance_by_id`].
pub fn set_archiveit_provenance_by_id(
    home: &Path,
    crawl_id: &str,
    host: &str,
    ait_collection_id: i64,
    ait_crawl_id: i64,
    warc_count: u64,
    collection_title: &str,
) -> Result<()> {
    let mut manifest = Manifest::open(&index_dir(home))?;
    let wacz = manifest
        .waczs
        .iter_mut()
        .find(|w| w.id == crawl_id)
        .with_context(|| format!("no indexed crawl with id {crawl_id}"))?;
    wacz.archive_it = Some(crate::collections::ArchiveItRef {
        host: host.to_string(),
        collection_id: ait_collection_id,
        crawl_id: ait_crawl_id,
        warc_count,
        collection_title: collection_title.to_string(),
    });
    manifest.save()?;
    Ok(())
}

/// Record who accessioned `crawl_ids`, in one manifest open/save.
///
/// Set out-of-band, after indexing, for the same reason the import provenance
/// is: the ingest pipeline is shared with the CLI and has no notion of a
/// request identity, and threading an actor down through it would add an
/// eleventh parameter to four already-crowded signatures. Only entries that
/// are still unattributed are touched, so re-running an add can't reassign
/// custody of someone else's crawl.
pub fn set_added_by(
    home: &Path,
    crawl_ids: &std::collections::HashSet<String>,
    actor: &SubjectId,
) -> Result<()> {
    if crawl_ids.is_empty() {
        return Ok(());
    }
    let mut manifest = Manifest::open(&index_dir(home))?;
    let mut touched = false;
    for wacz in manifest.waczs.iter_mut() {
        if wacz.added_by.is_none() && crawl_ids.contains(&wacz.id) {
            wacz.added_by = Some(actor.clone());
            touched = true;
        }
    }
    if touched {
        manifest.save()?;
    }
    Ok(())
}

/// Every crawl id currently in the manifest.
///
/// Used to snapshot before an ingest so the caller can attribute exactly the
/// entries that ingest created — a location can yield several crawls (a
/// directory of WACZs, or nested ones), and "everything in the target
/// collection without custody" would wrongly claim CLI-indexed neighbours.
pub fn crawl_ids(home: &Path) -> Result<std::collections::HashSet<String>> {
    Ok(Manifest::open(&index_dir(home))?
        .waczs
        .iter()
        .map(|w| w.id.clone())
        .collect())
}
