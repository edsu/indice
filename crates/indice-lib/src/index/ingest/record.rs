//! Recording a finished crawl in the manifest: the provenance assembly step.
//!
//! Everything the pipeline learned about a WACZ — its datapackage metadata, the
//! `warcinfo` and capture stats gathered while indexing, and its fixity — is
//! folded into one [`Wacz`] entry here, and the collection's finding aid is
//! seeded (fill-gaps only) from the same metadata.

use crate::collections::{Manifest, Source, Wacz};
use crate::index::paths::year_prefix;

use super::pages::CrawlStats;

/// Upsert this crawl's manifest entry and seed its collection's finding aid.
///
/// `fixity` is the `(sha256, file_size)` pair from
/// [`WaczAccess::fixity`](super::WaczAccess::fixity) — the hash is empty for a
/// streamed remote, which is never read whole.
#[allow(clippy::too_many_arguments)]
pub(super) fn upsert(
    manifest: &mut Manifest,
    id: &str,
    // The curated collection (id, display name) this crawl belongs to.
    collection: (&str, &str),
    // What the manifest records as the source (post-`--download` if that ran).
    source: &Source,
    display_name: &str,
    meta: crate::wacz::WaczMetadata,
    stats: CrawlStats,
    fixity: (String, u64),
) {
    let (collection_id, collection_name) = collection;
    let (sha, file_size) = fixity;
    let date_indexed = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    // Provenance: collect the software reported by the datapackage and by the
    // warcinfo record (deduped) - we don't label which crawled vs packaged.
    // operator/user-agent/robots come from warcinfo when present.
    let warcinfo = stats.warcinfo.unwrap_or_default();
    let mut software: Vec<String> = Vec::new();
    for s in meta.software.into_iter().chain(warcinfo.software) {
        if !software.contains(&s) {
            software.push(s);
        }
    }
    manifest.ensure_collection(collection_id, collection_name, &date_indexed);

    // Seed the collection's finding aid from this WACZ's datapackage — fill-gaps,
    // so only empty fields are set: the first indexed WACZ with a value wins and
    // a curator's edits are never overwritten. A single crawl's `description`
    // isn't really the whole collection's scope, but a draft beats a blank and
    // invites the curator to refine it.
    let year = meta.created.as_deref().and_then(year_prefix);
    let seed = crate::collections::CollectionFields {
        narrative: meta.description.clone(),
        subjects: (!meta.keywords.is_empty()).then(|| meta.keywords.clone()),
        dates: year,
        creator: meta.creator.clone(),
        rights: (!meta.licenses.is_empty()).then(|| meta.licenses.join(", ")),
        ..Default::default()
    };
    if !seed.is_empty() {
        manifest.seed_fields(collection_id, collection_name, &seed, &date_indexed);
    }

    // Preserve import provenance (set out-of-band by the importers) across a
    // reindex, which otherwise rebuilds the entry from scratch.
    let browsertrix = manifest.wacz_by_id(id).and_then(|w| w.browsertrix.clone());
    let archive_it = manifest.wacz_by_id(id).and_then(|w| w.archive_it.clone());

    manifest.upsert_wacz(Wacz {
        id: id.to_string(),
        collection: collection_id.to_string(),
        source: source.clone(),
        name: display_name.to_string(),
        date_indexed,
        file_size,
        sha256: sha,
        description: meta.description,
        crawl_date: meta.created,
        seed_pages: meta.seed_pages,
        software,
        operator: warcinfo.operator,
        user_agent: warcinfo.user_agent,
        robots: warcinfo.robots,
        page_count: Some(stats.pages),
        capture_start: stats.earliest_capture,
        capture_end: stats.latest_capture,
        browsertrix,
        archive_it,
        nested_waczs: stats.nested_waczs,
        // Provenance previously parsed-but-dropped / newly read.
        modified: meta.modified,
        is_part_of: warcinfo.is_part_of,
        hostname: warcinfo.hostname,
        conforms_to: warcinfo.conforms_to,
        keywords: meta.keywords,
        licenses: meta.licenses,
        status_counts: stats.status_counts,
    });
}

/// Build the body text for a collection-level Tantivy document from its metadata.
pub(super) fn collection_body(meta: &crate::wacz::WaczMetadata) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(desc) = &meta.description {
        parts.push(desc.clone());
    }
    for page in &meta.seed_pages {
        if let Some(title) = &page.title {
            parts.push(title.clone());
        }
        parts.push(page.url.clone());
    }
    parts.join(" ")
}
