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

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};

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
    /// Live **page** documents carrying this id. Deleted ones are excluded, so
    /// this is what a search can still turn up, and it is directly comparable
    /// with the manifest's `page_count` (a crawl's `collection` document is not
    /// counted, or every number here would read one too high).
    pub live_pages: u64,
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
    /// The collection the manifest records. Unlike the orphan direction, this
    /// one is known for certain, so the repair command never needs a
    /// placeholder.
    pub collection: crate::collections::CollectionId,
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

/// The result of asking, which is not always the result of comparing.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// `home` holds no index, so there is nothing to compare.
    ///
    /// A separate answer from a clean one on purpose. Reporting "the index and
    /// the manifest agree" for a directory that is not an archive is a lie a
    /// curator would believe, and the default `--home .` makes it easy to run
    /// this from the wrong place.
    NoIndex,
    Checked(Reconciliation),
}

/// Compare the index against the manifest for `home`.
///
/// The two store reads happen under the index write lock, blocking until it is
/// free. The lock is not here to protect a write, since there is none; it is
/// here so that a finding means something. An ingest that has committed its
/// documents and not yet saved its manifest entry is *momentarily* in exactly
/// the state this pass looks for, so without the lock a healthy archive
/// mid-ingest reports as damaged. Holding it means every indice writer has
/// finished, and any disagreement left is durable.
///
/// What follows the reads is deliberately **outside** the lock: the disk scan
/// and the per-orphan collection lookups are derived from a snapshot already
/// taken, and they only decide what a printed command says, never whether a
/// finding exists. Keeping them inside would hold the lock that gates the whole
/// write surface across an archive-wide directory walk, which on a large
/// archive means every workroom write waits ten seconds and then 503s.
pub fn reconcile(home: &Path, progress: &dyn IndexProgress) -> Result<Outcome> {
    // Before the lock, because taking the lock *creates* the index directory,
    // and so does opening the index: a read-only pass must not bring an archive
    // into being. Same guard, for the same reason, as `delete` and `optimize`.
    if !super::paths::index_initialized(home) {
        return Ok(Outcome::NoIndex);
    }

    let search = SearchIndex::open_read_only(&index_dir(home).join("full_text"))?;

    let (manifest, live) = {
        let _index = super::lock::lock_index(home, "reconciling the index and manifest", progress)?;
        progress.phase("reading the manifest");
        let manifest = Manifest::open(&index_dir(home))?;
        progress.phase("listing crawls in the index");
        let live = search.live_crawl_ids()?;
        (manifest, live)
    };

    let recorded: HashSet<&str> = manifest.waczs.iter().map(|w| w.id.as_str()).collect();

    // Only scan the disk if something is actually orphaned; on a healthy
    // archive this pass costs one manifest read and one index read.
    let orphan_ids: Vec<&String> = live
        .keys()
        .filter(|id| !recorded.contains(id.as_str()))
        .collect();
    let on_disk = if orphan_ids.is_empty() {
        BTreeMap::new()
    } else {
        progress.phase("looking for the orphans' files on disk");
        wacz_ids_on_disk(home)?
    };

    let orphans: Vec<Orphan> = orphan_ids
        .into_iter()
        .map(|id| Orphan {
            crawl_id: id.clone(),
            live_pages: live[id],
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
            collection: w.collection.clone(),
            recorded_pages: w.page_count,
        })
        .collect();

    progress.finish();
    Ok(Outcome::Checked(Reconciliation {
        orphans,
        missing,
        in_manifest: manifest.waczs.len(),
        in_index: live.len(),
    }))
}

/// Every WACZ under `<home>/archive`, keyed by the crawl id its location would
/// produce.
///
/// This is what makes an orphan recoverable rather than merely reportable. The
/// id has to be computed through [`Source::for_file`] rather than from the
/// absolute path, because that is what the ingest recorded: a path under `home`
/// is stored relative to it, so ids survive the whole archive being moved, and
/// hashing the absolute path here would reproduce none of them.
/// An unreadable directory is an **error**, not an absence. The alternative is
/// the worst report this module could produce: skip `archive/<slug>/` on a
/// permission error or a stale NFS mount, find nothing that hashes to the id,
/// and tell a curator their documents are the last copy of a crawl whose WACZ
/// is sitting right there.
fn wacz_ids_on_disk(home: &Path) -> Result<BTreeMap<String, Source>> {
    let mut found = BTreeMap::new();
    let root = archive_dir(home);
    // An archive directory that does not exist yet is a real absence.
    if !root.exists() {
        return Ok(found);
    }
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).with_context(|| {
            format!("reading {} while looking for orphaned WACZs", dir.display())
        })?;
        for entry in entries {
            let entry =
                entry.with_context(|| format!("listing {} for orphaned WACZs", dir.display()))?;
            // `file_type` from the entry does not follow symlinks, unlike
            // `Path::is_dir`. A link back to an ancestor under `archive/` would
            // otherwise push the same directories forever, and this walk runs
            // in a pass that other writers queue behind.
            let kind = entry
                .file_type()
                .with_context(|| format!("inspecting {}", entry.path().display()))?;
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "wacz") {
                let abs = path.canonicalize().unwrap_or(path);
                let source = Source::for_file(&abs, home);
                found.insert(wacz_id(&source), source);
            }
        }
    }
    Ok(found)
}

/// Wrap `s` for a POSIX shell, so a path holding a quote or a space still
/// pastes as one argument.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `home` as a shell argument, absolute so the command survives being pasted
/// somewhere else. The default `--home .` is the common case and the one that
/// would otherwise only work from the directory the report was run in.
fn home_argument(home: &Path) -> String {
    let abs = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    shell_quote(&abs.display().to_string())
}

/// A source as an `indice index` argument.
///
/// Absolute for a local file, because the manifest stores the path relative to
/// `home` while `index` resolves its location argument against the **current
/// directory**. Passing `--home` does not fix that, which is easy to assume and
/// wrong: the command then finds the right archive and the wrong file. A remote
/// source has no local path, so its location string is already the argument.
fn index_argument(home: &Path, source: &Source) -> String {
    match source.resolve(home) {
        // Canonicalized so it matches the `--home` in the same command rather
        // than mixing, say, `/var/...` with `/private/var/...`. Falls back to
        // the joined path when the file is gone, which is the `missing` case.
        Some(path) => path.canonicalize().unwrap_or(path).display().to_string(),
        None => source.location(),
    }
}

/// The command that repairs `orphan`, or `None` when no command can.
///
/// `None` is the case where nothing on disk hashes to the id, so there is
/// nothing to re-index from and the documents are the only surviving copy of
/// the crawl. The caller has to say that in prose; returning it here as a
/// sentence dressed up as a command was how this first went wrong.
pub fn orphan_remedy(home: &Path, orphan: &Orphan) -> Option<String> {
    let source = orphan.source.as_ref()?;
    Some(format!(
        "indice index {} --collection {} --force --home {}",
        shell_quote(&index_argument(home, source)),
        shell_quote(orphan.collection.as_deref().unwrap_or("<collection>")),
        home_argument(home),
    ))
}

/// The command that clears an orphan whose file is gone, which is a rebuild.
///
/// Not `indice crawl delete`: that plans from the manifest entry, so on the one
/// id that has none it fails with "no crawl with id". A rebuild writes a fresh
/// index from the manifest, so documents nothing references are left behind.
///
/// `None` when the manifest is empty, because a rebuild with no registered
/// sources exits early ("no WACZs registered; nothing to reindex") and would
/// leave the orphan exactly where it is. Then the index holds nothing worth
/// keeping and removing it is the honest advice.
pub fn unrecoverable_remedy(home: &Path, report: &Reconciliation) -> Option<String> {
    (report.in_manifest > 0).then(|| format!("indice reindex --home {}", home_argument(home)))
}

/// The command that repairs `missing`, as a curator would type it.
pub fn missing_remedy(home: &Path, missing: &Missing) -> String {
    format!(
        "indice index {} --collection {} --force --home {}",
        shell_quote(&index_argument(home, &missing.source)),
        shell_quote(missing.collection.as_str()),
        home_argument(home),
    )
}
