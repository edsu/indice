//! Find where the Tantivy index and the manifest disagree, and say how to fix
//! it.
//!
//! # Why this exists
//!
//! [`lock`](super::lock) serializes writers, so no two operations lose each
//! other's work. One gap stays open and no amount of locking closes it:
//! **there is no transaction spanning Tantivy and `waczs.json`**. An ingest
//! commits documents and *then* saves the manifest entry, so a crash, a power
//! loss or a `SIGKILL` between those two leaves documents in the index that no
//! manifest entry mentions.
//!
//! For a curator that looks like a haunting. The crawl's pages come back in
//! search results, its crawl page 404s (rendering goes through the manifest),
//! and it cannot be deleted through the UI either, because `delete_crawl` plans
//! from the manifest entry that is not there. It is the same end state as the
//! lost-update bug in #126, reached by hardware instead of by a race, which is
//! why no lock helps.
//!
//! The gap is permanent by construction. indice keeps human-readable files on
//! disk rather than putting the manifest in a database, so a single transaction
//! over both stores is not on offer. Repair is the alternative to prevention,
//! and this module is the repair.
//!
//! # Why it only reports
//!
//! Nothing here writes. The pass prints what it found and the exact command to
//! fix each finding, because the two repairs have very different stakes:
//! re-indexing is additive and safe, while dropping documents throws away the
//! only remaining copy of a crawl's text. A curator deciding that one crawl at
//! a time, with the file path in front of them, is the right shape.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::collections::{wacz_id, Manifest, Source};
use crate::search::SearchIndex;

use super::paths::{archive_dir, index_dir};
use super::IndexProgress;

/// Documents in the index that no manifest entry claims.
///
/// The crash-after-commit case, and the one a curator actually reports.
#[derive(Debug, Clone)]
pub struct Orphan {
    pub crawl_id: String,
    /// Live documents carrying this id. Deleted ones are excluded, so this is
    /// what a search can still turn up.
    pub live_docs: u64,
    /// The WACZ still on disk whose location hashes to this id, when there is
    /// one.
    ///
    /// Recoverable only because [`wacz_id`] hashes the crawl's *home-relative
    /// location string* rather than its contents, and a local ingest files the
    /// file under `archive/<collection>/`. So the id can be recomputed from
    /// what is on disk, and a match means the crawl is fully recoverable: a
    /// re-index rebuilds the manifest entry with real provenance instead of a
    /// reconstruction. `None` means the source was remote or the file is gone,
    /// and then the text in the index is all that is left of it.
    pub source: Option<Source>,
    /// The collection the crawl's own documents say it belonged to. Read from
    /// the index because the manifest entry that would normally answer this is
    /// the thing that went missing, and without it a repair command could only
    /// offer a placeholder for the curator to guess at.
    pub collection: Option<String>,
}

/// A manifest entry whose documents are not in the index.
///
/// The rarer direction: the manifest saved and a later commit was lost.
#[derive(Debug, Clone)]
pub struct Missing {
    pub crawl_id: String,
    pub name: String,
    pub source: Source,
    /// Pages the manifest recorded at ingest, which is what separates damage
    /// from a crawl that never had anything to index. See
    /// [`is_confirmed`](Self::is_confirmed).
    pub recorded_pages: Option<u64>,
}

impl Missing {
    /// Whether the manifest itself proves the documents are gone.
    ///
    /// Zero documents is not damage on its own: a WACZ with nothing extractable
    /// indexes to nothing, and reporting that as loss would send a curator
    /// hunting for a crawl that was always empty. A recorded `page_count`
    /// above zero is the manifest asserting that pages existed, so their
    /// absence is real. Without that number the finding is worth showing and
    /// not worth alarming anyone about.
    pub fn is_confirmed(&self) -> bool {
        matches!(self.recorded_pages, Some(n) if n > 0)
    }
}

/// What one pass found.
#[derive(Debug, Clone)]
pub struct Reconciliation {
    pub orphans: Vec<Orphan>,
    pub missing: Vec<Missing>,
    /// Crawls the manifest lists, as the denominator for a report.
    pub in_manifest: usize,
    /// Distinct crawl ids with live documents in the index.
    pub in_index: usize,
}

impl Reconciliation {
    /// Whether the two stores agree.
    pub fn is_consistent(&self) -> bool {
        self.orphans.is_empty() && self.missing.is_empty()
    }

    /// Findings the manifest proves are damage, as opposed to the ambiguous
    /// ones. What an exit code should key off.
    pub fn confirmed_findings(&self) -> usize {
        self.orphans.len() + self.missing.iter().filter(|m| m.is_confirmed()).count()
    }
}

/// Compare the index against the manifest for `home`.
///
/// Takes the index write lock for the whole pass and blocks until it is free,
/// announcing the holder the way every other CLI writer does. The lock is not
/// here to protect a write, since there is none; it is here so that a finding
/// means something. An ingest that has committed its documents and not yet
/// saved its manifest entry is *momentarily* in exactly the state this pass
/// looks for, so without the lock a healthy archive mid-ingest reports as
/// damaged. Holding it means every indice writer has finished, and any
/// disagreement left is durable.
pub fn reconcile(home: &Path, progress: &dyn IndexProgress) -> Result<Reconciliation> {
    let _index = super::lock::lock_index(home, "reconciling the index and manifest", progress)?;

    progress.phase("reading the manifest");
    let manifest = Manifest::open(&index_dir(home))?;

    progress.phase("listing crawls in the index");
    let search = SearchIndex::open_read_only(&index_dir(home).join("full_text"))?;
    let live = search.live_crawl_ids()?;

    let recorded: BTreeMap<&str, &crate::collections::Wacz> =
        manifest.waczs.iter().map(|w| (w.id.as_str(), w)).collect();

    // Only scan the disk if something is actually orphaned; on a healthy
    // archive this pass should cost one manifest read and one index read.
    let orphan_ids: Vec<&String> = live
        .keys()
        .filter(|id| !recorded.contains_key(id.as_str()))
        .collect();
    let on_disk = if orphan_ids.is_empty() {
        BTreeMap::new()
    } else {
        progress.phase("looking for the orphans' files on disk");
        wacz_ids_on_disk(home)
    };

    let orphans: Vec<Orphan> = orphan_ids
        .into_iter()
        .map(|id| Orphan {
            crawl_id: id.clone(),
            live_docs: live[id],
            source: on_disk.get(id).cloned(),
            // A failure here costs a placeholder in the printed command, not
            // the finding, so it is not worth failing the whole pass over.
            collection: search.crawl_collection(id).ok().flatten(),
        })
        .collect();

    let missing = manifest
        .waczs
        .iter()
        .filter(|w| !live.contains_key(&w.id))
        .map(|w| Missing {
            crawl_id: w.id.clone(),
            name: w.name.clone(),
            source: w.source.clone(),
            recorded_pages: w.page_count,
        })
        .collect();

    Ok(Reconciliation {
        orphans,
        missing,
        in_manifest: manifest.waczs.len(),
        in_index: live.len(),
    })
}

/// Every WACZ under `<home>/archive`, keyed by the crawl id its location would
/// produce.
///
/// This is what makes an orphan recoverable rather than merely reportable. The
/// id has to be computed through [`Source::for_file`] rather than from the
/// absolute path, because that is what the ingest recorded: a path under `home`
/// is stored relative to it, so ids survive the whole archive being moved, and
/// hashing the absolute path here would reproduce none of them.
fn wacz_ids_on_disk(home: &Path) -> BTreeMap<String, Source> {
    let mut found = BTreeMap::new();
    let mut stack = vec![archive_dir(home)];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "wacz") {
                let abs = path.canonicalize().unwrap_or(path);
                let source = Source::for_file(&abs, home);
                found.insert(wacz_id(&source), source);
            }
        }
    }
    found
}

/// The command that repairs `orphan`, as a curator would type it.
pub fn orphan_remedy(orphan: &Orphan) -> String {
    match &orphan.source {
        Some(source) => format!(
            "indice index '{}' --collection '{}' --force",
            source.location(),
            orphan.collection.as_deref().unwrap_or("<collection>")
        ),
        None => format!(
            "no file on disk hashes to {}; its documents are the only copy left",
            orphan.crawl_id
        ),
    }
}

/// The command that repairs `missing`, as a curator would type it.
pub fn missing_remedy(missing: &Missing) -> String {
    format!(
        "indice index '{}' --collection <collection> --force",
        missing.source.location()
    )
}
