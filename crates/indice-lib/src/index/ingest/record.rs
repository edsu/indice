//! Recording a finished crawl in the manifest: the provenance assembly step.
//!
//! Everything the pipeline learned about a WACZ — its datapackage metadata, the
//! `warcinfo` and capture stats gathered while indexing, and its fixity — is
//! folded into one [`Wacz`] entry here, and the collection's finding aid is
//! seeded (fill-gaps only) from the same metadata.

use crate::collections::{CollectionId, Manifest, Source, Wacz};
use crate::index::paths::year_prefix;

use super::pages::CrawlStats;

/// Everything the pipeline learned about one WACZ, for [`upsert`] to fold into
/// a manifest entry.
///
/// The phases hand each other a named value rather than an argument list. That
/// is worth more here than anywhere else in the pipeline: this is the frame
/// where a new piece of provenance lands, and it is the frame crawl custody
/// could not reach when `added_by` had to be set out of band instead of being
/// threaded through. It reaches it now — `actor` below is that field.
pub(super) struct Indexed<'a> {
    pub id: &'a str,
    /// The curated collection (id, display name) this crawl belongs to.
    pub collection: (&'a CollectionId, &'a str),
    /// What the manifest records as the source (post-`--download` if that ran).
    pub source: &'a Source,
    pub display_name: &'a str,
    pub meta: crate::wacz::WaczMetadata,
    pub stats: CrawlStats,
    /// `(sha256, file_size)` from
    /// [`WaczAccess::fixity`](super::WaczAccess::fixity) — the hash is empty
    /// for a streamed remote, which is never read whole.
    pub fixity: (String, u64),
    /// Who to credit if this crawl is new to the manifest; `None` for the CLI,
    /// which has no request identity.
    pub actor: Option<&'a crate::identity::SubjectId>,
}

/// Upsert this crawl's manifest entry and seed its collection's finding aid.
pub(super) fn upsert(manifest: &mut Manifest, crawl: Indexed) {
    let Indexed {
        id,
        collection,
        source,
        display_name,
        meta,
        stats,
        fixity,
        actor,
    } = crawl;
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

    // What survives from an entry that is already in the manifest. `upsert_wacz`
    // rebuilds the entry from scratch, so anything not carried here is lost —
    // which is why a reindex has to read it back rather than recompute it.
    //
    // The single lookup matters: **custody is decided by whether this crawl is
    // new to the manifest, not by whether it currently has a custody line.**
    // Falling back to the actor whenever `added_by` happened to be `None` would
    // let an existing *unattributed* crawl be claimed, and an unattributed
    // crawl is deliberately nobody's — `may_delete_crawl` is owner-or-admin, so
    // only an admin may remove it. A curator could otherwise take an
    // admin-accessioned crawl simply by adding the same URL to a different
    // collection: the already-indexed skip guard is scoped to the collection,
    // so that add falls through here and re-homes the entry. Guarded by
    // `tests/integration.rs::re_homing_an_unattributed_crawl_does_not_claim_it`.
    //
    // So: custody is set once, at accession, and an existing entry keeps
    // whatever it has, including nothing. That also covers the two cases the
    // out-of-band setter needed an `added_by.is_none()` check for — a rebuild
    // keeps recorded custody, and re-indexing someone else's crawl cannot
    // transfer it.
    let (browsertrix, archive_it, added_by) = match manifest.wacz_by_id(id) {
        Some(prior) => (
            prior.browsertrix.clone(),
            prior.archive_it.clone(),
            prior.added_by.clone(),
        ),
        // Genuinely new to the manifest: this is the accession, so credit
        // whoever is acting. `None` for the CLI, which has no identity.
        None => (None, None, actor.cloned()),
    };

    manifest.upsert_wacz(Wacz {
        id: id.to_string(),
        collection: collection_id.clone(),
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
        added_by,
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
