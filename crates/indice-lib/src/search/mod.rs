use std::path::{Path, PathBuf};
use tantivy::schema::Value;

use anyhow::{Context, Result};
use tantivy::{Index, IndexWriter, TantivyDocument, Term};

mod extract;
mod facets;
mod parse;
mod query;
mod schema;
mod types;

#[cfg(test)]
mod tests;

// Re-imported so the submodules reach each other (and callers keep their
// existing `search::X` paths) through one `use super::*;`.
pub use extract::*;
pub use facets::*;
use parse::*;
pub use types::*;
// `site_of` is also used by the thumbnail picker.
pub(crate) use parse::site_of;
use schema::*;

/// Truncate `s` to at most `max_bytes`, on a UTF-8 char boundary.
fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub struct SearchIndex {
    index: Index,
    /// The index directory (`…/full_text`), kept so [`Self::optimize`] can sweep
    /// orphaned segment files off disk.
    index_dir: PathBuf,
    /// Present only when opened for writing. The server opens read-only so it
    /// does not hold Tantivy's exclusive write lock, letting `index` run while
    /// the server is serving.
    writer: Option<IndexWriter>,
    /// Bytes of page body text stored per document for snippets (the full body
    /// is always indexed). Defaults to the built-in cap; the indexer overrides
    /// it from the home config (the "frugality" knob). See [`crate::config`].
    stored_body_cap: usize,
}

impl SearchIndex {
    /// Open the index for writing (indexing) with the default writer heap.
    /// Creates it if needed and acquires Tantivy's exclusive write lock.
    pub fn open(index_dir: &Path) -> Result<Self> {
        Self::open_with_heap(index_dir, crate::config::DEFAULT_WRITER_HEAP_BYTES)
    }

    /// Like [`open`], but with an explicit Tantivy writer-heap budget (bytes) —
    /// the indexer sets this from the home config (the RAM/throughput knob).
    ///
    /// [`open`]: Self::open
    pub fn open_with_heap(index_dir: &Path, writer_heap_bytes: usize) -> Result<Self> {
        let index = Self::open_index(index_dir)?;
        let writer = index.writer(writer_heap_bytes)?;
        Ok(Self {
            index,
            index_dir: index_dir.to_path_buf(),
            writer: Some(writer),
            stored_body_cap: crate::config::DEFAULT_STORED_BODY_CAP_BYTES,
        })
    }

    /// Open the index read-only (searching). Does not create a writer, so it
    /// does not take the write lock; indexing can proceed concurrently.
    pub fn open_read_only(index_dir: &Path) -> Result<Self> {
        let index = Self::open_index(index_dir)?;
        Ok(Self {
            index,
            index_dir: index_dir.to_path_buf(),
            writer: None,
            stored_body_cap: crate::config::DEFAULT_STORED_BODY_CAP_BYTES,
        })
    }

    /// Set how much body text to store per document for snippets (bytes). The
    /// indexer sets this from the home config before indexing; `usize::MAX`
    /// stores the full body.
    pub fn set_stored_body_cap(&mut self, cap_bytes: usize) {
        self.stored_body_cap = cap_bytes;
    }

    fn open_index(index_dir: &Path) -> Result<Index> {
        std::fs::create_dir_all(index_dir)?;
        let schema = build_schema();
        if index_dir.join("meta.json").exists() {
            let index = Index::open_in_dir(index_dir)
                .with_context(|| format!("opening Tantivy index at {}", index_dir.display()))?;
            // A stored schema that differs from the current one (e.g. after new
            // fields were added) can't be written or queried correctly. Fail
            // with a clear message instead of panicking on a missing field.
            if index.schema() != schema {
                anyhow::bail!(
                    "the search index at {} was built with an older schema; \
                     run `indice reindex` to rebuild it",
                    index_dir.display()
                );
            }
            Ok(index)
        } else {
            // Compress the doc store with zstd rather than the default lz4: the
            // store holds the page text (the largest, corpus-linear part of the
            // index) and zstd compresses text markedly better, with negligible
            // read cost. Applied on create, so `reindex` picks it up. See the
            // size/scale model in DESIGN.md.
            let settings = tantivy::IndexSettings {
                docstore_compression: tantivy::store::Compressor::Zstd(
                    tantivy::store::ZstdCompressor::default(),
                ),
                ..Default::default()
            };
            Index::builder()
                .schema(schema)
                .settings(settings)
                .create_in_dir(index_dir)
                .with_context(|| format!("creating Tantivy index at {}", index_dir.display()))
        }
    }

    fn writer_mut(&mut self) -> &mut IndexWriter {
        self.writer
            .as_mut()
            .expect("SearchIndex opened read-only; no writer available")
    }

    /// Remove all documents (pages and the collection doc) belonging to a single
    /// crawl, by its `crawl_id`. Used both to re-index a crawl as an upsert and
    /// to delete it outright. The delete only takes effect on [`commit`].
    ///
    /// Tantivy applies a delete only to documents committed before it, so when
    /// re-indexing the caller should `delete_crawl_docs()` first, then
    /// `index_page()` / `index_collection()`, then `commit()` — the fresh
    /// documents survive.
    ///
    /// [`commit`]: Self::commit
    pub fn delete_crawl_docs(&mut self, crawl_id: &str) {
        let field = self.index.schema().get_field(FIELD_CRAWL_ID).unwrap();
        self.writer_mut()
            .delete_term(Term::from_field_text(field, crawl_id));
    }

    /// Index a single page from an archive. Fields not set on the [`Page`]
    /// default to empty, so callers only populate what they have.
    pub fn index_page(&mut self, page: &Page) -> Result<()> {
        let schema = self.index.schema();
        let mut doc = TantivyDocument::default();
        doc.add_text(schema.get_field(FIELD_DOC_TYPE).unwrap(), "page");
        doc.add_text(schema.get_field(FIELD_CRAWL_ID).unwrap(), page.crawl_id);
        doc.add_text(schema.get_field(FIELD_CRAWL_NAME).unwrap(), page.crawl_name);
        doc.add_text(schema.get_field(FIELD_COLLECTION).unwrap(), page.collection);
        doc.add_text(schema.get_field(FIELD_URL).unwrap(), page.url);
        doc.add_text(schema.get_field(FIELD_TS).unwrap(), page.timestamp);
        doc.add_text(schema.get_field(FIELD_TITLE).unwrap(), page.title);
        doc.add_text(schema.get_field(FIELD_BODY).unwrap(), page.body);
        doc.add_text(
            schema.get_field(FIELD_BODY_SNIP).unwrap(),
            truncate_on_char_boundary(page.body, self.stored_body_cap),
        );
        doc.add_text(
            schema.get_field(FIELD_DESCRIPTION).unwrap(),
            page.description,
        );
        doc.add_text(schema.get_field(FIELD_HEADINGS).unwrap(), page.headings);
        doc.add_text(schema.get_field(FIELD_KEYWORDS).unwrap(), page.keywords);
        doc.add_text(schema.get_field(FIELD_AUTHOR).unwrap(), page.author);
        // Derived URL fields: an exact host for `domain:` filtering, and the
        // URL's words tokenized so they're searchable as ordinary terms.
        doc.add_text(schema.get_field(FIELD_DOMAIN).unwrap(), domain_of(page.url));
        doc.add_text(schema.get_field(FIELD_SITE).unwrap(), site_of(page.url));
        doc.add_text(
            schema.get_field(FIELD_URL_TOKENS).unwrap(),
            url_search_text(page.url),
        );
        // Numeric year/month for range filtering and the timeline; omitted when
        // there's no usable date.
        if let Some(year) = year_of(page.timestamp) {
            doc.add_u64(schema.get_field(FIELD_YEAR).unwrap(), year);
        }
        if let Some(month) = month_of(page.timestamp) {
            doc.add_u64(schema.get_field(FIELD_MONTH).unwrap(), month);
        }
        doc.add_text(schema.get_field(FIELD_MEDIA_TYPE).unwrap(), page.media_type);
        // Language: the declared `<html lang>` wins; if absent, fall back to
        // detecting it from the body text (empty when nothing is confident).
        let lang = if page.lang.trim().is_empty() {
            detect_lang(page.body).unwrap_or_default()
        } else {
            primary_lang(page.lang)
        };
        doc.add_text(schema.get_field(FIELD_LANG).unwrap(), &lang);
        if let Some(status) = page.status {
            doc.add_u64(schema.get_field(FIELD_STATUS).unwrap(), status as u64);
        }
        if let Some(year) = page.modified_year {
            doc.add_u64(schema.get_field(FIELD_MODIFIED).unwrap(), year);
        }
        self.writer_mut().add_document(doc)?;
        Ok(())
    }

    /// Index a collection-level document so the collection itself is searchable.
    /// `body` should be the concatenation of the description and seed page titles/URLs.
    pub fn index_collection(
        &mut self,
        crawl_id: &str,
        crawl_name: &str,
        collection: &str,
        body: &str,
    ) -> Result<()> {
        let schema = self.index.schema();
        let mut doc = TantivyDocument::default();
        doc.add_text(schema.get_field(FIELD_DOC_TYPE).unwrap(), "collection");
        doc.add_text(schema.get_field(FIELD_CRAWL_ID).unwrap(), crawl_id);
        doc.add_text(schema.get_field(FIELD_CRAWL_NAME).unwrap(), crawl_name);
        doc.add_text(schema.get_field(FIELD_COLLECTION).unwrap(), collection);
        doc.add_text(schema.get_field(FIELD_URL).unwrap(), "");
        doc.add_text(schema.get_field(FIELD_TS).unwrap(), "");
        doc.add_text(schema.get_field(FIELD_TITLE).unwrap(), crawl_name);
        doc.add_text(schema.get_field(FIELD_BODY).unwrap(), body);
        doc.add_text(
            schema.get_field(FIELD_BODY_SNIP).unwrap(),
            truncate_on_char_boundary(body, self.stored_body_cap),
        );
        // Collection docs have no page URL or HTML metadata; keep those empty.
        doc.add_text(schema.get_field(FIELD_DESCRIPTION).unwrap(), "");
        doc.add_text(schema.get_field(FIELD_HEADINGS).unwrap(), "");
        doc.add_text(schema.get_field(FIELD_KEYWORDS).unwrap(), "");
        doc.add_text(schema.get_field(FIELD_AUTHOR).unwrap(), "");
        doc.add_text(schema.get_field(FIELD_DOMAIN).unwrap(), "");
        doc.add_text(schema.get_field(FIELD_SITE).unwrap(), "");
        doc.add_text(schema.get_field(FIELD_URL_TOKENS).unwrap(), "");
        doc.add_text(schema.get_field(FIELD_MEDIA_TYPE).unwrap(), "");
        doc.add_text(schema.get_field(FIELD_LANG).unwrap(), "");
        self.writer_mut().add_document(doc)?;
        Ok(())
    }

    /// Index one page annotation so notes are discoverable in full-text search.
    /// The note text is the searchable body (+ snippet); `author` is searchable
    /// via `author:`. `url`/`timestamp` point at the annotated capture, and
    /// `collection` scopes it (so `collection:` filtering still works). The note
    /// keeps its own `doc_type = "annotation"` so results can render it as a note
    /// rather than a page. Idempotent per id when paired with
    /// [`delete_annotation_doc`](Self::delete_annotation_doc): delete then add.
    pub fn index_annotation(
        &mut self,
        annotation_id: &str,
        collection: &str,
        url: &str,
        timestamp: &str,
        author: &str,
        note: &str,
    ) -> Result<()> {
        let schema = self.index.schema();
        let mut doc = TantivyDocument::default();
        doc.add_text(schema.get_field(FIELD_DOC_TYPE).unwrap(), "annotation");
        doc.add_text(
            schema.get_field(FIELD_ANNOTATION_ID).unwrap(),
            annotation_id,
        );
        doc.add_text(schema.get_field(FIELD_COLLECTION).unwrap(), collection);
        doc.add_text(schema.get_field(FIELD_URL).unwrap(), url);
        doc.add_text(schema.get_field(FIELD_TS).unwrap(), timestamp);
        doc.add_text(schema.get_field(FIELD_AUTHOR).unwrap(), author);
        doc.add_text(schema.get_field(FIELD_BODY).unwrap(), note);
        doc.add_text(
            schema.get_field(FIELD_BODY_SNIP).unwrap(),
            truncate_on_char_boundary(note, self.stored_body_cap),
        );
        self.writer_mut().add_document(doc)?;
        Ok(())
    }

    /// Remove a single annotation document by its id (the write takes effect on
    /// [`commit`](Self::commit)). Pairs with [`index_annotation`](Self::index_annotation)
    /// for an upsert; a crawl delete/reindex never touches it (annotation docs
    /// carry no `crawl_id`).
    pub fn delete_annotation_doc(&mut self, annotation_id: &str) {
        let field = self.index.schema().get_field(FIELD_ANNOTATION_ID).unwrap();
        self.writer_mut()
            .delete_term(Term::from_field_text(field, annotation_id));
    }

    pub fn commit(&mut self) -> Result<()> {
        self.writer_mut().commit()?;
        Ok(())
    }

    /// Number of searchable segments. A healthy index has a handful; hundreds
    /// means background merges haven't kept up (e.g. they failed on a full
    /// disk), which slows *every* query — a search fans out across all segments.
    pub fn segment_count(&self) -> Result<usize> {
        Ok(self.index.searchable_segment_ids()?.len())
    }

    /// Total number of live documents across all segments — the denominator for
    /// the bytes-per-doc size/scale model.
    pub fn num_docs(&self) -> Result<u64> {
        Ok(self.index.reader()?.searcher().num_docs())
    }

    /// Uncompressed stored-text bytes per field, over up to `scan_cap` live docs
    /// (`0` = all). Complements [`crate::index::index_stats`] (which breaks the
    /// footprint down by Tantivy file *type*) by showing what fills the doc
    /// *store* — i.e. which fields' stored text dominate. The `.store` on disk is
    /// zstd-compressed, so these uncompressed sizes are a relative indicator, not
    /// the exact on-disk split. Scans only live docs, so deleted-but-unmerged
    /// docs don't skew it.
    pub fn stored_field_sizes(&self, scan_cap: usize) -> Result<StoredFieldStats> {
        let searcher = self.index.reader()?.searcher();
        let schema = self.index.schema();
        let mut by: std::collections::HashMap<String, (u64, u64)> =
            std::collections::HashMap::new();
        let mut scanned = 0usize;
        'outer: for seg in searcher.segment_readers() {
            let store = seg.get_store_reader(10)?;
            let alive = seg.alive_bitset();
            for doc_id in 0..seg.max_doc() {
                if let Some(bs) = alive {
                    if !bs.is_alive(doc_id) {
                        continue;
                    }
                }
                let doc: TantivyDocument = store.get(doc_id)?;
                for (field, entry) in schema.fields() {
                    if let Some(v) = doc.get_first(field) {
                        let len = if let Some(s) = v.as_str() {
                            s.len() as u64
                        } else if v.as_u64().is_some() {
                            8
                        } else {
                            0
                        };
                        if len > 0 {
                            let e = by.entry(entry.name().to_string()).or_default();
                            e.0 += len;
                            e.1 += 1;
                        }
                    }
                }
                scanned += 1;
                if scan_cap != 0 && scanned >= scan_cap {
                    break 'outer;
                }
            }
        }
        let mut fields: Vec<(String, u64, u64)> =
            by.into_iter().map(|(k, (b, c))| (k, b, c)).collect();
        fields.sort_by_key(|(_, b, _)| std::cmp::Reverse(*b));
        Ok(StoredFieldStats { scanned, fields })
    }

    /// Compact the index by merging segments down toward `target_segments`
    /// (clamped to ≥ 1), so queries fan out across far fewer segments. Merges
    /// smallest-first in bounded batches, waiting for each merge before the next.
    /// A merge writes the new segment before its inputs are freed, so it needs
    /// transient free disk ≈ the size of the largest segment it produces — i.e.
    /// roughly `index_size / target`. A smaller `target` compacts more but raises
    /// that peak (`target` = 1 needs ~a second copy of the whole index). Needs a
    /// writer. Returns `(before, after)` counts.
    pub fn optimize(
        &mut self,
        target_segments: usize,
        progress: Option<&dyn crate::index::IndexProgress>,
    ) -> Result<(usize, usize)> {
        // Segments merged per round: bounds each merge's size (hence peak disk)
        // and gives the spinner something to tick.
        const BATCH: usize = 32;
        let target = target_segments.max(1);
        // Take exclusive control of merging. Otherwise, as soon as our first
        // explicit merge finishes, Tantivy's default LogMergePolicy (triggered
        // on merge completion) schedules *background* merges over the remaining
        // segments — which then race our next explicit merge and consume its
        // segments out from under it ("couldn't find segment in SegmentManager").
        self.writer_mut()
            .set_merge_policy(Box::new(tantivy::indexer::NoMergePolicy));
        let before = self.index.searchable_segment_ids()?.len();
        if let Some(p) = progress {
            p.begin("optimize");
            p.phase(&format!("{before} segments"));
        }

        let mut prev = usize::MAX;
        let mut retries = 0;
        loop {
            // Re-read each round: a merge replaces its inputs with one segment.
            let mut metas = self.index.searchable_segment_metas()?;
            let n = metas.len();
            // Stop at the target, or if a round made no progress (defensive:
            // never spin, even if a merge unexpectedly didn't reduce the count).
            if n <= target || n >= prev {
                break;
            }
            prev = n;
            // Smallest-first keeps early merges cheap; merge enough to move
            // toward `target`, capped by BATCH so one merge can't blow up disk.
            metas.sort_by_key(|m| m.num_docs());
            let take = (n - target + 1).clamp(2, BATCH);
            let ids: Vec<_> = metas.iter().take(take).map(|m| m.id()).collect();
            match self.writer_mut().merge(&ids).wait() {
                Ok(_) => {
                    retries = 0;
                    if let Some(p) = progress {
                        p.phase(&format!("{} segments", n - (take - 1)));
                    }
                }
                Err(e) => {
                    // A background merge scheduled during a prior ingest can still
                    // be in flight and consume some of the segments we just chose
                    // ("segments … could not be found in the SegmentManager").
                    // Let it settle and retry with a freshly-read segment set,
                    // bounded so a genuinely stuck merge still surfaces.
                    retries += 1;
                    if retries > 8 {
                        return Err(e).context("merging segments while optimizing the index");
                    }
                    prev = usize::MAX; // re-arm the no-progress guard for the retry
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }

        // Expunge deletes (pkvm): the count-based pass above stops once we're at
        // or under `target`, so a segment still carrying tombstoned docs (e.g.
        // from deleted crawls) keeps their data on disk — a delete never frees
        // space until its segment is rewritten. Merge each delete-carrying
        // segment on its own (a single-segment merge drops its tombstones), so
        // `optimize` reclaims delete space regardless of `--max-segments`, without
        // over-collapsing clean segments into one big one.
        let with_deletes: Vec<_> = self
            .index
            .searchable_segment_metas()?
            .into_iter()
            .filter(|m| m.num_deleted_docs() > 0)
            .map(|m| m.id())
            .collect();
        if !with_deletes.is_empty() {
            if let Some(p) = progress {
                p.phase(&format!(
                    "expunging deletes ({} segment(s))",
                    with_deletes.len()
                ));
            }
            for id in with_deletes {
                let mut retries = 0;
                loop {
                    match self.writer_mut().merge(&[id]).wait() {
                        Ok(_) => break,
                        Err(e) => {
                            retries += 1;
                            if retries > 8 {
                                return Err(e)
                                    .context("expunging deletes while optimizing the index");
                            }
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        }
                    }
                }
            }
        }

        // Delete the now-orphaned input segment files and persist the tidy meta.
        self.writer_mut()
            .garbage_collect_files()
            .wait()
            .context("garbage-collecting merged segment files")?;
        self.writer_mut().commit().context("committing optimize")?;

        // Tantivy's GC only removes files it *managed*. Segment files left behind
        // by a hard-killed merge or ingest (Ctrl-C mid-write) were never
        // registered, so GC skips them and they accumulate forever. Now that the
        // meta is committed, sweep any segment file not referenced by a live
        // segment — safe because we hold the exclusive write lock.
        // A segment's uuid as it appears in on-disk filenames: hyphens stripped,
        // lowercased (Tantivy names segment files `<uuid>.<ext>`).
        let live: std::collections::HashSet<String> = self
            .index
            .searchable_segment_ids()?
            .iter()
            .map(|id| id.uuid_string().replace('-', "").to_ascii_lowercase())
            .collect();
        let (orphans, freed) = sweep_orphan_segment_files(&self.index_dir, &live);
        if orphans > 0 {
            tracing::info!(
                orphans,
                freed_bytes = freed,
                "removed orphaned segment files (leftovers from an interrupted merge/ingest)"
            );
            if let Some(p) = progress {
                p.phase(&format!("removed {orphans} orphaned file(s)"));
            }
        }

        let after = self.index.searchable_segment_ids()?.len();
        if let Some(p) = progress {
            p.finish();
        }
        Ok((before, after))
    }

    /// Disable Tantivy's automatic segment merging on this writer, so each
    /// `commit()` leaves its own segment. Tests use this to build a deliberately
    /// fragmented index to exercise [`Self::optimize`].
    #[cfg(test)]
    pub(crate) fn disable_auto_merge(&mut self) {
        self.writer_mut()
            .set_merge_policy(Box::new(tantivy::indexer::NoMergePolicy));
    }
}

/// Remove orphaned segment files from `index_dir`: `<uuid>.<ext>` files whose
/// uuid is not a `live` (meta-referenced) segment. These accumulate when a merge
/// or ingest is hard-killed mid-write — the partial segment files are left behind
/// and were never registered in Tantivy's managed set, so `garbage_collect_files`
/// never removes them. Returns `(files_removed, bytes_freed)`. Best-effort: an IO
/// error on one file is skipped, not fatal.
///
/// Caller MUST hold the write lock and have just committed, so every segment file
/// not in `live` is genuinely dead. Bookkeeping files (`meta.json`,
/// `.managed.json`, `*.lock`) and non-segment names are left untouched.
fn sweep_orphan_segment_files(
    index_dir: &Path,
    live: &std::collections::HashSet<String>,
) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(index_dir) else {
        return (0, 0);
    };
    let mut removed = 0usize;
    let mut freed = 0u64;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Segment component files are `<uuid>.<ext>` (store/pos/term/idx/fast/
        // fieldnorm) or `<uuid>.<n>.del`. Bookkeeping is *.json / *.lock.
        let Some((stem, ext)) = name.rsplit_once('.') else {
            continue;
        };
        if ext == "json" || ext == "lock" {
            continue;
        }
        // The uuid is the first dotted component (handles `<uuid>.<n>.del`).
        let uuid = stem.split('.').next().unwrap_or(stem);
        let norm = uuid.replace('-', "").to_ascii_lowercase();
        // Only touch things that actually look like a 32-hex segment uuid.
        if norm.len() != 32 || !norm.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        if live.contains(&norm) {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
            freed += size;
        }
    }
    (removed, freed)
}
