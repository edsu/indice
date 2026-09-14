//! Import-provenance setters (Browsertrix, Archive-It) recorded on already-indexed
//! manifest entries.

use std::path::Path;

use anyhow::{Context, Result};

use crate::collections::{wacz_id, BrowsertrixRef, Manifest, Source};

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
