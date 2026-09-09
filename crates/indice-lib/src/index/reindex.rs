//! Rebuild the full-text index from the manifest's recorded sources, into a
//! sibling index that is swapped in atomically on success.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use tracing::info;

use crate::collections::{CollectionId, Manifest, Source};
use crate::search::SearchIndex;

use super::ingest::index_one;
use super::paths::index_dir;
use super::swap::{index_swap_paths, reconcile_index_swap, swap_in_new_index};
use super::{IndexProgress, SourceResolver};

/// Rebuild the full-text index from the sources already recorded in
/// `collections.json`, preserving the manifest (including each collection's
/// display name).
///
/// Unlike [`index_location`] (which scans `<home>/archive`), this re-indexes
/// every registered source - including remote URLs, which are re-fetched - and
/// recreates the Tantivy index from scratch, so a schema change is picked up.
/// Local files that have gone missing are skipped with a warning rather than
/// aborting the whole run.
pub fn reindex(
    home: &Path,
    // Concurrent record fetches per source for CDX-guided streaming; `None` picks
    // a per-source default (see `default_concurrency`).
    concurrency: Option<usize>,
    // Resolves any Browsertrix sources in the manifest to fresh presigned URLs
    // (binary-provided). `None` → such a source errors (needs credentials).
    resolver: Option<&dyn SourceResolver>,
    // Optional progress sink; drives the same per-WACZ bar as `index`.
    progress: Option<&dyn IndexProgress>,
) -> Result<()> {
    let index_dir = index_dir(home);
    let mut manifest = Manifest::open(&index_dir)?;
    if manifest.waczs.is_empty() {
        info!("no WACZs registered; nothing to reindex");
        return Ok(());
    }

    // Snapshot each WACZ (source, name, collection id + name) before upserting
    // back, so its collection membership and the collection's metadata survive.
    let targets: Vec<(Source, String, CollectionId, String)> = manifest
        .waczs
        .iter()
        .map(|w| {
            let coll_name = manifest
                .collection_by_id(&w.collection)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| w.name.clone());
            (
                w.source.clone(),
                w.name.clone(),
                w.collection.clone(),
                coll_name,
            )
        })
        .collect();

    // Resolve config before destroying the old index, so a malformed config.yaml
    // aborts the reindex rather than leaving no index. reindex re-streams every
    // source, so honoring the writer heap here matters most.
    let config = crate::config::Config::load(home)?;

    // Atomic rebuild: build the fresh index into a sibling `full_text.new` and
    // swap it in only once the rebuild fully succeeds, so a hard failure (crash,
    // kill, disk-full) mid-rebuild leaves the existing `full_text` untouched —
    // you are never left worse off than before the reindex, and a running
    // `serve` keeps reading the old index until the swap. First clear any
    // leftovers from a previously-interrupted run (recovering the live index if
    // a crash landed mid-swap).
    reconcile_index_swap(&index_dir)?;
    let (_full_text, new_dir, _old_dir) = index_swap_paths(&index_dir);
    let mut search_index =
        SearchIndex::open_with_heap(new_dir.as_path(), config.writer_heap_bytes())
            .with_context(|| format!("creating search index at {}", new_dir.display()))?;
    search_index.set_stored_body_cap(config.stored_body_cap_bytes());
    let search = Mutex::new(search_index);

    let total = targets.len();
    let mut done = 0usize;
    let mut skipped = 0usize;
    for (source, name, collection_id, collection_name) in &targets {
        // Skip local files that no longer exist rather than failing the run;
        // their manifest entry is preserved (see `indice verify`). Only *file*
        // sources get this on-disk check: URL and Browsertrix sources are remote
        // and must flow to `index_one`, which re-resolves them (the resolver
        // mints a fresh presigned URL for Browsertrix). Using `is_url()` here
        // would misroute Browsertrix sources — `resolve()` returns None for them,
        // so they'd be skipped as "missing" on every reindex (kx… / nk69).
        if !source.is_remote() {
            match source.resolve(home) {
                Some(p) if p.exists() => {}
                _ => {
                    tracing::warn!(source = %source.location(), "skipping missing local WACZ");
                    skipped += 1;
                    continue;
                }
            }
        }
        info!(
            source = %source.location(),
            progress = format!("{}/{}", done + skipped + 1, total),
            "reindexing"
        );
        // Resilient: a source that fails after retries (e.g. a remote host that's
        // down or blocking) is skipped with a warning rather than aborting the
        // whole rebuild — a long reindex over many remote sources shouldn't be
        // torched by one bad source. Its manifest entry is preserved, and
        // membership is re-supplied so the collection survives.
        match index_one(
            source,
            home,
            &mut manifest,
            &search,
            Some(name),
            (collection_id, collection_name),
            false,
            concurrency,
            resolver,
            progress,
        ) {
            Ok((wacz_name, pages)) => {
                done += 1;
                // Print the per-WACZ summary as each one finishes, so the next
                // WACZ's progress bar doesn't erase the record of it (the line
                // persists above the new bar).
                if let Some(p) = progress {
                    p.wacz_indexed(&wacz_name, pages);
                }
            }
            Err(e) => {
                tracing::warn!(
                    source = %source.location(),
                    "skipping WACZ that failed to reindex: {e:#}"
                );
                skipped += 1;
            }
        }
    }

    // Re-index every collection's page annotations into the fresh index, so
    // notes are searchable after a rebuild just like pages. Annotations live in
    // committable JSONL (not the WACZs), so they're indexed here rather than in
    // `index_one`. A collection whose annotations file is missing/unreadable is
    // simply skipped (an empty or absent file is the common case).
    {
        let mut si = search.lock().unwrap();
        for coll in &manifest.collections {
            let anns = match crate::annotations::load(home, &coll.id) {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!(collection = %coll.id, "skipping annotations: {e:#}");
                    continue;
                }
            };
            for a in &anns {
                let author = a.creator.name.as_deref().unwrap_or("");
                si.index_annotation(
                    &a.id,
                    &coll.id,
                    &a.target.source,
                    &a.target.timestamp,
                    author,
                    &a.body.value,
                )?;
            }
        }
    }

    // The rebuild always runs to completion and the (possibly partial) index is
    // committed, so it's usable even if some sources were skipped.
    search.into_inner().unwrap().commit()?;
    // The fresh build succeeded; swap it in for the old index atomically, then
    // persist the manifest so on-disk metadata matches the now-live index. A
    // partial rebuild (some sources skipped) is still swapped in — it's usable
    // and no worse than the old index — but we still exit non-zero below.
    swap_in_new_index(&index_dir)?;
    manifest.save()?;
    if let Some(p) = progress {
        p.finish();
    }
    if skipped > 0 {
        // Usable but incomplete: return an error so the process exits non-zero and
        // cron/CI notices, while leaving the mostly-rebuilt index in place.
        anyhow::bail!(
            "reindex finished but {skipped} of {total} source(s) were skipped \
             (indexed {done}); the search index is missing them — fix the cause \
             and run `indice reindex` again to include them"
        );
    }
    info!(reindexed = done, total, "reindex complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::set_browsertrix_provenance;
    use crate::index::testsupport::*;
    use tempfile::TempDir;

    #[test]
    fn browsertrix_provenance_is_recorded_and_survives_reindex() {
        let tmp = TempDir::new().unwrap();
        let dest = index_fixture("simple.wacz", tmp.path(), None);

        set_browsertrix_provenance(
            tmp.path(),
            &dest,
            "https://app.browsertrix.com",
            "item-1",
            "sha256:aa",
            Some(4),
        )
        .unwrap();

        let recorded = |home: &Path| {
            Manifest::open(&home.join("index")).unwrap().waczs[0]
                .browsertrix
                .clone()
        };
        let b = recorded(tmp.path()).expect("provenance recorded");
        assert_eq!(b.item_id, "item-1");
        assert_eq!(b.resource_hash, "sha256:aa");
        assert_eq!(b.review_status, Some(4));

        // A reindex rebuilds each manifest entry from scratch; provenance set
        // out-of-band by the importer must be carried over, not wiped.
        reindex(tmp.path(), None, None, None).unwrap();
        let after = recorded(tmp.path()).expect("provenance after reindex");
        assert_eq!(after.item_id, "item-1");
        assert_eq!(
            after.review_status,
            Some(4),
            "review rating survives reindex"
        );
    }
    #[test]
    fn reindex_does_not_rewrite_an_already_seeded_finding_aid() {
        // Once a collection's finding aid is seeded, a reindex must not rewrite it
        // (no new empty field to fill) — so a curator's hand formatting / comments
        // survive.
        let tmp = TempDir::new().unwrap();
        index_fixture("a.wacz", tmp.path(), None); // collection "test", seeds `dates`
        let readme = tmp.path().join("collections/test/README.md");
        // Curator adds a YAML comment, keeping the seeded field.
        let edited = std::fs::read_to_string(&readme)
            .unwrap()
            .replace("name: test", "name: test  # hand-labelled");
        std::fs::write(&readme, &edited).unwrap();

        reindex(tmp.path(), None, None, None).unwrap();

        assert_eq!(
            std::fs::read_to_string(&readme).unwrap(),
            edited,
            "reindex must leave an already-seeded finding aid byte-for-byte intact"
        );
    }
    #[test]
    fn reindex_after_rename_does_not_spawn_a_phantom_collection() {
        // Editing the display `name:` must not create a second collection on the
        // next reindex — seeding is keyed on the stable id, not the slug of name.
        let tmp = TempDir::new().unwrap();
        index_fixture("a.wacz", tmp.path(), None); // id "test"
        let readme = tmp.path().join("collections/test/README.md");
        let renamed = std::fs::read_to_string(&readme)
            .unwrap()
            .replace("name: test", "name: Test Archive");
        std::fs::write(&readme, renamed).unwrap();

        reindex(tmp.path(), None, None, None).unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("collections"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(entries.len(), 1, "no phantom collection dir: {entries:?}");
        assert!(!tmp.path().join("collections/test-archive").exists());
    }
    #[test]
    fn reindex_rebuilds_from_manifest() {
        // Index once with a custom name, then blow away just the full-text index
        // (as a schema change / corruption would require) and reindex from the
        // manifest.
        let tmp = TempDir::new().unwrap();
        index_fixture("simple.wacz", tmp.path(), Some("keepname"));

        let full_text = tmp.path().join("index").join("full_text");
        std::fs::remove_dir_all(&full_text).unwrap();

        reindex(tmp.path(), None, None, None).unwrap();

        // The manifest (custom name + collection membership) is preserved...
        let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
        assert_eq!(manifest.waczs.len(), 1);
        assert_eq!(manifest.waczs[0].name, "keepname");
        assert_eq!(
            manifest.waczs[0].collection, "test",
            "collection membership must survive a reindex"
        );

        // ...and the content is searchable again.
        let idx = crate::search::SearchIndex::open(full_text.as_path()).unwrap();
        assert!(
            !idx.search("example", 10).unwrap().is_empty(),
            "reindexed content should be searchable"
        );
    }
    #[test]
    fn reindex_with_no_collections_is_ok() {
        let tmp = TempDir::new().unwrap();
        // No collections.json yet: reindex should be a no-op, not an error.
        reindex(tmp.path(), None, None, None).unwrap();
    }
    #[test]
    fn reindex_skips_a_failing_source_and_keeps_going() {
        // A resilient reindex: one good WACZ plus one that exists but isn't a
        // valid WACZ. The bad source is skipped (warned) rather than aborting the
        // whole rebuild, so the good source is still indexed and searchable, and
        // the skipped source's manifest entry is preserved for a later re-run.
        let tmp = TempDir::new().unwrap();
        index_fixture("simple.wacz", tmp.path(), None);

        // Plant a corrupt WACZ and register it as a member alongside the good one.
        std::fs::write(tmp.path().join("archive/bad.wacz"), b"not a zip file").unwrap();
        let waczs_path = tmp.path().join("index/waczs.json");
        let mut entries: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&waczs_path).unwrap()).unwrap();
        entries.push(serde_json::json!({
            "id": "deadbeef",
            "collection": "deadbeef",
            "source": "archive/bad.wacz",
            "name": "BadOne",
            "date_indexed": "2026-01-01T00:00:00Z",
            "file_size": 14,
            "sha256": "00"
        }));
        std::fs::write(&waczs_path, serde_json::to_string(&entries).unwrap()).unwrap();

        // Rebuild from the manifest: the run completes over the good source but
        // reports a non-zero exit (an error) because one source was skipped.
        let err = reindex(tmp.path(), None, None, None)
            .expect_err("a skipped source should surface as a non-zero exit, not abort mid-run");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("skipped") && msg.contains("reindex"),
            "error should summarize the skipped source(s) and suggest re-running: {msg}"
        );

        // ...yet the good source is still fully indexed and searchable.
        let idx =
            crate::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
                .unwrap();
        assert!(
            !idx.search("example", 10).unwrap().is_empty(),
            "the good source should still be indexed after skipping the bad one"
        );

        // ...and the skipped source's manifest entry is preserved (not dropped),
        // so `indice reindex` can pick it up again once the cause is fixed.
        let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
        assert_eq!(
            manifest.waczs.len(),
            2,
            "skipped source's manifest entry should be preserved"
        );
    }
    #[test]
    fn reindex_does_not_skip_browsertrix_sources_at_the_guard() {
        use std::sync::atomic::{AtomicBool, Ordering};
        // Regression for rustyweb-reindex-skips-browsertrix-nk69. The reindex
        // "skip missing local file" guard used `!source.is_url()`, which is true
        // for a Browsertrix source (only Url counts as a url); `resolve()` returns
        // None for it, so it was skipped as "missing local WACZ" *before*
        // index_one ran — silently dropping every Browsertrix member on each
        // reindex. The guard is now `!source.is_remote()` (file sources only), so
        // a Browsertrix source flows to index_one, which resolves it. A spy
        // resolver proves it's reached (with the bug, resolve is never called).
        let tmp = TempDir::new().unwrap();
        index_fixture("simple.wacz", tmp.path(), None);

        // Rewrite the member's source to a public Browsertrix locator — remote,
        // but not a plain Url: exactly the shape the old guard mis-skipped.
        let waczs_path = tmp.path().join("index/waczs.json");
        let mut entries: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&waczs_path).unwrap()).unwrap();
        entries[0]["source"] =
            serde_json::Value::String("browsertrix-public|example.com|org1|coll1|file.wacz".into());
        std::fs::write(&waczs_path, serde_json::to_string(&entries).unwrap()).unwrap();

        struct Spy {
            called: AtomicBool,
        }
        impl SourceResolver for Spy {
            fn resolve(&self, _s: &Source) -> Result<String> {
                self.called.store(true, Ordering::SeqCst);
                // Fail the fetch on purpose — we only care that index_one got far
                // enough to ask the resolver, not that a real WACZ is streamed.
                anyhow::bail!("spy resolver: not actually fetching")
            }
        }
        let spy = Spy {
            called: AtomicBool::new(false),
        };

        // reindex ends in an error (the stub resolve fails, so the source is
        // skipped *downstream*), but the resolver having been called proves the
        // source reached index_one instead of being dropped by the guard.
        let _ = reindex(tmp.path(), None, Some(&spy), None);
        assert!(
            spy.called.load(Ordering::SeqCst),
            "a Browsertrix source must reach index_one (resolver called), \
             not be skipped by the reindex guard"
        );
    }
}
