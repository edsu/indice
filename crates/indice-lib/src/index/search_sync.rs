//! Keep the full-text index in step with individual annotation writes
//! (upsert / delete), without a full reindex.

use std::path::Path;

use anyhow::{Context, Result};

use crate::search::SearchIndex;

use super::paths::index_dir;

/// Upsert a single annotation into the live search index by its id (delete any
/// prior doc for that id, then add the current one) and commit. Keeps search in
/// step with a create or edit without a full reindex. A no-op if the index
/// hasn't been built yet (nothing to keep in step with). The caller holds the
/// server write lock; pair with `reload_searcher` to publish the change.
pub fn index_annotation_upsert(
    home: &Path,
    collection: &str,
    annotation: &crate::annotations::Annotation,
) -> Result<()> {
    // A rebuild re-indexes annotations from the JSONL and then swaps the whole
    // directory, so a note written between its annotation pass and the swap
    // would have its document deleted with the old index. The JSONL survives,
    // so the note is not lost — but it is silently unsearchable until someone
    // rebuilds again, and nothing reports it.
    //
    // Locked *before* the "is there an index yet?" check, not after. The swap
    // is two renames (`full_text` aside, then `full_text.new` into place), and
    // between them `full_text/meta.json` does not exist — so a caller that
    // checked first would conclude there is nothing to index and return
    // successfully, producing exactly the silent unsearchability the lock is
    // here to prevent.
    // No index at all: nothing to sync, and locking would create one.
    if !super::paths::index_initialized(home) {
        return Ok(());
    }
    let _index = super::lock::lock_index(home, "an annotation write", crate::index::no_progress())?;
    let full_text = index_dir(home).join("full_text");
    if !full_text.join("meta.json").exists() {
        return Ok(());
    }
    let mut search =
        SearchIndex::open(&full_text).context("opening the search index to index an annotation")?;
    search.delete_annotation_doc(&annotation.id);
    search.index_annotation(
        &annotation.id,
        collection,
        &annotation.target.source,
        &annotation.target.timestamp,
        // Indexed author is shown on public search results, so it must be the
        // sanitized display name rather than the stored login identity.
        annotation.creator.public_name().unwrap_or(""),
        &annotation.body.value,
    )?;
    search.commit().context("committing the annotation index")?;
    Ok(())
}

/// Remove a single annotation from the live search index by its id and commit.
/// A no-op if the index hasn't been built yet.
pub fn delete_annotation_from_index(home: &Path, annotation_id: &str) -> Result<()> {
    // Locked before the existence check, for the reason spelled out in
    // `index_annotation_upsert`: mid-swap there is no `full_text`, and a caller
    // that checked first would silently do nothing.
    // No index at all: nothing to sync, and locking would create one.
    if !super::paths::index_initialized(home) {
        return Ok(());
    }
    let _index = super::lock::lock_index(home, "an annotation write", crate::index::no_progress())?;
    let full_text = index_dir(home).join("full_text");
    if !full_text.join("meta.json").exists() {
        return Ok(());
    }
    let mut search = SearchIndex::open(&full_text)
        .context("opening the search index to delete an annotation")?;
    search.delete_annotation_doc(annotation_id);
    search
        .commit()
        .context("committing the annotation deletion")?;
    Ok(())
}
