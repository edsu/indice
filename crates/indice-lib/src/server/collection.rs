//! Collection routes: the finding-aid page, its annotations index, and the
//! machine-readable replay manifest + page list wabac.js consumes.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::annotations::{self};
use crate::collections::{Manifest, Wacz};
use crate::views;

use super::*;

pub(super) async fn collection_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e).into_response(),
    };
    let Some(c) = manifest.collection_by_id(&id) else {
        return (StatusCode::NOT_FOUND, "collection not found").into_response();
    };
    let members: Vec<&Wacz> = manifest.members_of(&id).collect();

    // Aggregates derived from members.
    let total_size: u64 = members.iter().map(|w| w.file_size).sum();
    let software = collection_software(&members);
    let range = members_capture_range(&members);

    let mut meta = Vec::new();
    if let Some(cur) = &c.curator {
        meta.push(views::MetaRow::new("Curator", cur.clone()));
    }
    meta.push(views::MetaRow::new("Crawls", members.len().to_string()));
    meta.push(views::MetaRow::new("Size", human_size(total_size)));
    if !software.is_empty() {
        meta.push(views::MetaRow::new("Software", software.join(", ")));
    }
    if let Some(r) = &range {
        meta.push(views::MetaRow::new("Capture dates", r.clone()));
    }
    if let Some(q) = capture_quality(&merged_status_counts(&members)) {
        meta.push(views::MetaRow::new("Capture quality", q));
    }
    let created = c.created.get(..10).unwrap_or(&c.created);
    meta.push(views::MetaRow::new("Created", created));

    let member_items: Vec<views::MemberItem> = members
        .iter()
        .map(|w| views::MemberItem {
            id: w.id.clone(),
            name: w.name.clone(),
            present: w.is_present(&state.home),
            remote: w.source.is_remote(),
            provenance: provenance_summary(w),
            thumb: thumb_href(&state.home, &state.index_dir, &w.collection, &w.id),
        })
        .collect();

    // Scoped facet overview: what's *in* this collection, each value a search
    // scoped to it. Turns the page into a faceted entry point, not just a list.
    let overview = state
        .search
        .read()
        .unwrap()
        .facet_overview_scoped(crate::search::FacetScope::Collection(&id))
        .unwrap_or_default();
    let facets = scoped_facet_sections(&overview, &format!("collection:{id}"));

    let (manage, who) = admin_ctx(&state, &headers);
    let can_login = login_available(&state, &who);
    let page = views::CollectionPage {
        name: c.name.clone(),
        description: c.description.clone(),
        narrative: c.narrative.as_deref().map(crate::markdown::render),
        creator: c.creator.clone(),
        dates: c.dates.clone(),
        rights: c.rights.clone(),
        subjects: c.subjects.clone(),
        meta,
        facets,
        members: member_items,
        replay_href: collection_replay_href(&id, &c.name, collection_default_page(&members)),
        id: id.clone(),
        management: manage,
        signed_in: who,
        can_login,
        annotation_count: annotations::load(&state.home, &id)
            .map(|v| v.len())
            .unwrap_or(0),
    };
    views::collection(&page).into_response()
}

/// GET /collection/{id}/annotations — a public browse of every annotation in the
/// collection, each linking into the collection replay (where it re-anchors).
pub(super) async fn collection_annotations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e),
    };
    let Some(c) = manifest.collection_by_id(&id) else {
        return (StatusCode::NOT_FOUND, "collection not found").into_response();
    };
    let (manage, who) = admin_ctx(&state, &headers);
    let can_login = login_available(&state, &who);
    let anns = annotations::load(&state.home, &id).unwrap_or_default();
    let items = anns
        .iter()
        .map(|a| {
            let region = a.target.selector.as_ref().map(|s| match s {
                annotations::Selector::TextQuoteSelector { exact, .. } => exact.clone(),
            });
            views::AnnoLink {
                author: a
                    .creator
                    .name
                    .clone()
                    .unwrap_or_else(|| "anonymous".to_string()),
                date: a
                    .modified
                    .clone()
                    .unwrap_or_else(|| a.created.clone())
                    .chars()
                    .take(10)
                    .collect(),
                note_html: crate::markdown::render(&a.body.value),
                page_url: a.target.source.clone(),
                replay_href: collection_replay_href(
                    &id,
                    &c.name,
                    Some((a.target.source.clone(), a.target.timestamp.clone())),
                ),
                region,
            }
        })
        .collect();
    let page = views::AnnotationsIndexPage {
        collection_name: c.name.clone(),
        collection_id: id.clone(),
        items,
        management: manage,
        signed_in: who,
        can_login,
    };
    views::annotations_index(&page).into_response()
}

/// A wabac (ReplayWeb.page) multi-WACZ collection manifest for a collection: the
/// JSON that `<replay-web-page source="…/replay.json">` loads to replay every
/// member crawl as one collection. Each member maps to a resource pointing at the
/// same byte-serving endpoint single-WACZ replay already uses (`viewer_source`):
/// `/files/{id}` for local/Browsertrix sources, the remote URL for a plain URL.
///
/// `name`/`crawlId` are the WACZ id (kept identical so a future server-side pages
/// endpoint can return the same id as `filename` — see the scale-valve phase of
/// rustyweb-homepage-replay-bukh / rustyweb-cross-wacz-replay-dk4). `hash` carries
/// the `sha256:` prefix wabac expects.
pub(super) async fn collection_replay_json(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e),
    };
    let Some(c) = manifest.collection_by_id(&id) else {
        return (StatusCode::NOT_FOUND, "collection not found").into_response();
    };
    let resources: Vec<serde_json::Value> = manifest
        .members_of(&id)
        .map(|w| {
            let mut res = serde_json::json!({
                "name": w.id,
                "path": viewer_source(w),
                "crawlId": w.id,
            });
            // The member hash. wabac uses it as the member's identity, so every
            // member MUST have a distinct one (or none) — giving several members
            // the same hash collapses them in wabac's loader and breaks
            // multi-WACZ replay ("Archived Page Not Found", no member requests).
            //   - Local/downloaded WACZ: our computed whole-file sha256.
            //   - Streamed remote WACZ: no locally-computed sha256, but a
            //     Browsertrix import kept the file hash from its replay.json
            //     (already `sha256:…`) — use it, so streamed members keep
            //     distinct, real, verifiable hashes.
            //   - Otherwise (e.g. a plain remote URL): omit it; wabac then treats
            //     the member as unverified. See rustyweb-streamed-wacz-fixity-zle5.
            let hash = if !w.sha256.is_empty() {
                Some(format!("sha256:{}", w.sha256))
            } else {
                w.browsertrix
                    .as_ref()
                    .map(|b| b.resource_hash.trim())
                    .filter(|h| !h.is_empty())
                    // Normalize to the `algo:hash` form wabac expects. Browsertrix
                    // hashes are stored bare (64-hex sha256); older/test data may
                    // already carry a `sha256:` prefix.
                    .map(|h| {
                        if h.contains(':') {
                            h.to_string()
                        } else {
                            format!("sha256:{h}")
                        }
                    })
            };
            if let Some(h) = hash {
                res["hash"] = serde_json::Value::String(h);
            }
            res
        })
        .collect();
    if resources.is_empty() {
        return (StatusCode::NOT_FOUND, "collection has no crawls to replay").into_response();
    }
    let body = serde_json::json!({
        "resources": resources,
        "metadata": {
            "title": c.name,
            "desc": c.description,
            // No `pagesQueryUrl` yet: wabac replays this as a native multi-WACZ
            // collection, loading each member's CDX and resolving URLs itself.
            // Deferring the pagesQueryUrl scale valve (server-side resolution via
            // `collection_pages`) until its lazy-loading + resolution-completeness
            // are browser-verified — the hash fix unblocked it, but proving the
            // flat-footprint win needs more than a render check. See
            // rustyweb-scale-footprint-qw5.10.
        },
    });
    (StatusCode::OK, axum::Json(body)).into_response()
}

#[derive(Deserialize)]
pub(super) struct PagesParams {
    /// Exact-URL resolution (wabac's on-demand URL→WACZ lookup).
    url: Option<String>,
    /// Free-text page-list search (the viewer's Pages sidebar search box).
    search: Option<String>,
    /// 1-based page number.
    page: Option<usize>,
    #[serde(rename = "pageSize")]
    page_size: Option<usize>,
}

/// wabac `pagesQueryUrl` endpoint for a collection: the page list / search and
/// on-demand URL→WACZ resolution that back multi-WACZ replay, answered from the
/// Tantivy index. Response shape is wabac's: `{ total, items: [{ url, ts, title,
/// filename }] }`, where `filename` is the member WACZ id (== the manifest's
/// `resources[].name`). `ts` is emitted as ISO 8601 so wabac's `new Date(ts)`
/// parses it (the index stores a 14-digit timestamp).
pub(super) async fn collection_pages(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(params): Query<PagesParams>,
) -> Response {
    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(25).clamp(1, 200);
    let offset = (page - 1) * page_size;
    match state.search.read().unwrap().collection_pages(
        &id,
        params.url.as_deref(),
        params.search.as_deref(),
        offset,
        page_size,
    ) {
        Ok((total, hits)) => {
            let items: Vec<serde_json::Value> = hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "url": h.url,
                        "ts": ts_to_iso(&h.timestamp),
                        "title": h.title,
                        "filename": h.crawl_id,
                    })
                })
                .collect();
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({ "total": total, "items": items })),
            )
                .into_response()
        }
        Err(e) => error_response(e),
    }
}

/// Build the scoped facet sections for a detail page. Each dimension becomes a
/// labeled group whose links run a search within `scope` (e.g. `collection:slug`)
/// further filtered by that value. The Collection dimension is skipped (moot on a
/// scoped page), and empty dimensions are dropped.
pub(super) fn scoped_facet_sections(
    overview: &[crate::search::FacetGroup],
    scope: &str,
) -> Vec<views::FacetSection> {
    // (facet field == filter field, heading, sort by value desc, max shown)
    const DIMS: [(&str, &str, bool, usize); 4] = [
        ("site", "Top sites", false, 10),
        ("year", "By year", true, 12),
        ("type", "Types", false, 6),
        ("lang", "Languages", false, 8),
    ];
    DIMS.iter()
        .filter_map(|(field, label, by_value_desc, max)| {
            let group = overview.iter().find(|g| g.field == *field)?;
            let mut buckets: Vec<&crate::search::FacetBucket> = group.buckets.iter().collect();
            if buckets.is_empty() {
                return None;
            }
            if *by_value_desc {
                buckets.sort_by(|a, b| b.value.cmp(&a.value));
            }
            let links = buckets
                .into_iter()
                .take(*max)
                .map(|b| views::BrowseLink {
                    label: b.value.clone(),
                    count: b.count,
                    href: format!(
                        "/search?q={}",
                        url_encode(&format!("{scope} {field}:{}", b.value))
                    ),
                })
                .collect();
            Some(views::FacetSection {
                label: label.to_string(),
                links,
            })
        })
        .collect()
}
