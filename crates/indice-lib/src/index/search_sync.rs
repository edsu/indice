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
        annotation.creator.name.as_deref().unwrap_or(""),
        &annotation.body.value,
    )?;
    search.commit().context("committing the annotation index")?;
    Ok(())
}

/// Remove a single annotation from the live search index by its id and commit.
/// A no-op if the index hasn't been built yet.
pub fn delete_annotation_from_index(home: &Path, annotation_id: &str) -> Result<()> {
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
