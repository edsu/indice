//! Curatorial (finding-aid) metadata setters for collections and crawls:
//! finding-aid fields, seed-from-ingest, notes, and pinned thumbnails.

use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

use crate::collections::Manifest;

use super::paths::index_dir;

/// Create or update a collection's curatorial (finding-aid) metadata (its id is
/// the slug of `name`). Only fields set in `fields` change; the finding aid is
/// written to `<home>/collections/<slug>/README.md`. Returns the collection id.
pub fn set_collection(
    home: &Path,
    name: &str,
    fields: &crate::collections::CollectionFields,
    // Who is editing, when known. `None` from the CLI, which has no request
    // identity — an unattributed edit is recorded as such rather than invented.
    actor: Option<&crate::identity::SubjectId>,
) -> Result<String> {
    let index_dir = index_dir(home);
    std::fs::create_dir_all(&index_dir)
        .with_context(|| format!("creating index dir {}", index_dir.display()))?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let id = Manifest::update(&index_dir, |manifest| {
        Ok(manifest.apply_fields(name, fields, &now, actor))
    })?;
    info!(collection = %id, "collection metadata updated");
    Ok(id)
}

/// Auto-seed a collection's finding-aid metadata from ingest (WACZ datapackage,
/// Browsertrix API): fills only fields that are still empty, never clobbering a
/// curator's edits (see [`crate::collections::Manifest::seed_fields`]). A no-op
/// when `fields` is empty. Returns the collection id.
pub fn seed_collection(
    home: &Path,
    name: &str,
    fields: &crate::collections::CollectionFields,
) -> Result<String> {
    let index_dir = index_dir(home);
    std::fs::create_dir_all(&index_dir)
        .with_context(|| format!("creating index dir {}", index_dir.display()))?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let id = crate::collections::CollectionId::from_name(name);
    Manifest::update(&index_dir, |manifest| {
        manifest.seed_fields(&id, name, fields, &now);
        Ok(())
    })?;
    Ok(id.to_string())
}

/// Pin a curator-supplied local image as a collection's representative
/// thumbnail, committed at `collections/<slug>/thumbnail.jpg`. The collection is
/// identified by name (its slug); create it first with `collection set`.
pub fn set_collection_thumbnail(home: &Path, name: &str, image_file: &Path) -> Result<()> {
    let slug = crate::collections::CollectionId::from_name(name);
    let dest = crate::collections::collection_thumb_path(home, &slug);
    crate::thumbnail::set_manual(&dest, image_file)
        .with_context(|| format!("setting thumbnail for collection {slug}"))?;
    info!(collection = %slug, image = %image_file.display(), "pinned collection thumbnail");
    Ok(())
}
/// Set a crawl's curator note at `<home>/collections/<slug>/crawls/<id>.md`. Manifest-
/// only side effect (no reindex); errors if the crawl id is unknown.
pub fn set_crawl_note(home: &Path, crawl_id: &str, note: &str) -> Result<()> {
    let manifest = Manifest::open(&index_dir(home))?;
    let Some(wacz) = manifest.wacz_by_id(crawl_id) else {
        anyhow::bail!(
            "no crawl with id \"{crawl_id}\" - it's the id in the crawl's page URL (/crawl/<id>)"
        );
    };
    crate::collections::write_crawl_note(home, &wacz.collection, crawl_id, note)?;
    info!(crawl = %crawl_id, "crawl note updated");
    Ok(())
}

/// Pin a curator-supplied local image as a crawl's representative thumbnail,
/// committed under the collection (`collections/<slug>/crawls/<id>.jpg`) so it's
/// git-trackable and a later (re)index won't overwrite it. Manifest-only side
/// effect (no reindex).
pub fn set_crawl_thumbnail(home: &Path, crawl_id: &str, image_file: &Path) -> Result<()> {
    let manifest = Manifest::open(&index_dir(home))?;
    let Some(wacz) = manifest.wacz_by_id(crawl_id) else {
        anyhow::bail!(
            "no crawl with id \"{crawl_id}\" - it's the id in the crawl's page URL (/crawl/<id>)"
        );
    };
    let dest = crate::collections::pinned_thumb_path(home, &wacz.collection, crawl_id);
    crate::thumbnail::set_manual(&dest, image_file)
        .with_context(|| format!("setting thumbnail for crawl {crawl_id}"))?;
    info!(crawl = %crawl_id, image = %image_file.display(), "pinned crawl thumbnail");
    Ok(())
}
