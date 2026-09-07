//! Deletion planning and execution for crawls and collections (index docs,
//! on-disk files, manifest entries, and committed assets).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

use crate::collections::{Manifest, Wacz};
use crate::search::SearchIndex;

use super::paths::{archive_dir, index_dir};

/// What deleting a crawl removes — computed up front so a caller can confirm.
#[derive(Debug, Clone)]
pub struct CrawlDeletion {
    pub id: String,
    pub name: String,
    /// The collection the crawl belonged to.
    pub collection: String,
    /// The local WACZ file that will be / was removed: a `File` source with a
    /// copy on disk that no other entry references. `None` for a URL/streamed
    /// source, or a file shared with another crawl.
    pub local_file: Option<PathBuf>,
    /// Whether this was the last member of its collection (the now-empty grouping
    /// is left in place — collections are curated, so deletion is explicit).
    pub last_in_collection: bool,
}

/// The local WACZ file removing `wacz` should delete: only a `File` source, only
/// if it exists, and only if no *other* manifest entry references the same path
/// (so a shared file is never pulled out from under another crawl).
fn local_wacz_to_remove(manifest: &Manifest, wacz: &Wacz, home: &Path) -> Option<PathBuf> {
    if wacz.source.is_remote() {
        return None;
    }
    let path = wacz.source.resolve(home)?;
    // Only ever remove files under <home>/archive, never a curator's original
    // elsewhere. `place_local_wacz` already files every local WACZ into archive,
    // so a stored File source is always under it — this makes that invariant an
    // enforced guard, not just an assumption.
    if !path.starts_with(archive_dir(home)) {
        return None;
    }
    if !path.exists() {
        return None;
    }
    let referenced_elsewhere = manifest
        .waczs
        .iter()
        .any(|o| o.id != wacz.id && o.source == wacz.source);
    (!referenced_elsewhere).then_some(path)
}

/// Inspect what [`delete_crawl`] would remove, changing nothing. Errors if the id
/// is unknown.
pub fn plan_crawl_deletion(home: &Path, crawl_id: &str) -> Result<CrawlDeletion> {
    let manifest = Manifest::open(&index_dir(home))?;
    let wacz = manifest.wacz_by_id(crawl_id).ok_or_else(|| {
        anyhow::anyhow!(
            "no crawl with id \"{crawl_id}\" - it's the id in the crawl's page URL (/crawl/<id>)"
        )
    })?;
    Ok(CrawlDeletion {
        id: crawl_id.to_string(),
        name: wacz.name.clone(),
        collection: wacz.collection.clone(),
        local_file: local_wacz_to_remove(&manifest, wacz, home),
        last_in_collection: manifest.members_of(&wacz.collection).count() <= 1,
    })
}

/// Delete a crawl: its index documents, manifest entry, local WACZ (a `File`
/// source only, when unreferenced), and cached/pinned thumbnails + curator note.
///
/// Order (index docs → on-disk files → manifest entry) makes a crash mid-delete
/// safe to re-run: the entry — the record of *what* to clean up — is removed
/// last, so a retry can still find and finish any leftovers. Tantivy reclaims
/// disk only on a later segment merge, so the index won't shrink immediately.
pub fn delete_crawl(home: &Path, crawl_id: &str) -> Result<CrawlDeletion> {
    let plan = plan_crawl_deletion(home, crawl_id)?;

    // 1. Drop the crawl's documents from the search index and commit.
    let full_text = index_dir(home).join("full_text");
    if full_text.join("meta.json").exists() {
        let mut search =
            SearchIndex::open(&full_text).context("opening the search index to delete a crawl")?;
        search.delete_crawl_docs(crawl_id);
        search.commit().context("committing the crawl deletion")?;
    }

    // 2. Remove on-disk files (best-effort; a re-run finishes any leftovers).
    if let Some(path) = &plan.local_file {
        let _ = std::fs::remove_file(path);
        // Tidy an emptied per-item archive subdir (e.g. from a Browsertrix import).
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    let _ = std::fs::remove_file(
        index_dir(home)
            .join("thumbs")
            .join(format!("{crawl_id}.jpg")),
    );
    let _ = std::fs::remove_file(crate::collections::pinned_thumb_path(
        home,
        &plan.collection,
        crawl_id,
    ));
    let _ = std::fs::remove_file(crate::collections::crawl_note_path(
        home,
        &plan.collection,
        crawl_id,
    ));

    // 3. Remove the manifest entry last.
    let mut manifest = Manifest::open(&index_dir(home))?;
    manifest.remove_wacz(crawl_id);
    manifest
        .save()
        .context("saving the manifest after deletion")?;

    info!(crawl = %crawl_id, "crawl deleted");
    Ok(plan)
}
/// What deleting a collection removes.
#[derive(Debug, Clone)]
pub struct CollectionDeletion {
    pub id: String,
    pub name: String,
    pub member_count: usize,
    /// Ids of member crawls deleted — non-empty only with `with_crawls`.
    pub crawls_deleted: Vec<String>,
}

/// Inspect what [`delete_collection`] would remove, changing nothing. Errors if
/// the id is unknown.
pub fn plan_collection_deletion(home: &Path, id: &str) -> Result<CollectionDeletion> {
    let manifest = Manifest::open(&index_dir(home))?;
    let coll = manifest
        .collection_by_id(id)
        .ok_or_else(|| anyhow::anyhow!("no collection with id \"{id}\""))?;
    Ok(CollectionDeletion {
        id: id.to_string(),
        name: coll.name.clone(),
        member_count: manifest.members_of(id).count(),
        crawls_deleted: Vec::new(),
    })
}

/// Delete a collection grouping (its finding aid). Without `with_crawls` a
/// non-empty collection is refused; with it, every member crawl is deleted first
/// (files + index docs + manifest entries), then the grouping.
pub fn delete_collection(home: &Path, id: &str, with_crawls: bool) -> Result<CollectionDeletion> {
    let mut plan = plan_collection_deletion(home, id)?;
    if plan.member_count > 0 && !with_crawls {
        anyhow::bail!(
            "collection \"{id}\" still has {} crawl(s); pass --with-crawls to delete them too, \
             or move/delete the crawls first",
            plan.member_count
        );
    }

    if with_crawls {
        let members: Vec<String> = Manifest::open(&index_dir(home))?
            .members_of(id)
            .map(|w| w.id.clone())
            .collect();
        for cid in members {
            delete_crawl(home, &cid)?;
            plan.crawls_deleted.push(cid);
        }
    }

    // Drop the collection's annotation docs from the search index. They carry no
    // crawl_id, so deleting member crawls above doesn't remove them; collect
    // their ids now, before the on-disk annotations.jsonl is deleted with the dir.
    let ann_ids: Vec<String> = crate::annotations::load(home, id)
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.id)
        .collect();

    // Remove the grouping last: the manifest entry, then its finding-aid dir.
    let mut manifest = Manifest::open(&index_dir(home))?;
    let removed = manifest.remove_collection(id);
    manifest
        .save()
        .context("saving the manifest after deleting a collection")?;
    if removed.is_some() {
        let _ = std::fs::remove_dir_all(crate::collections::collection_dir(home, id));
    }

    if !ann_ids.is_empty() {
        let full_text = index_dir(home).join("full_text");
        if full_text.join("meta.json").exists() {
            let mut search = SearchIndex::open(&full_text)
                .context("opening the search index to delete collection annotations")?;
            for aid in &ann_ids {
                search.delete_annotation_doc(aid);
            }
            search
                .commit()
                .context("committing collection annotation deletion")?;
        }
    }

    info!(collection = %id, with_crawls, deleted = plan.crawls_deleted.len(), "collection deleted");
    Ok(plan)
}
