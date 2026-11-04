//! Collection routes: the finding-aid page, its annotations index, and the
//! machine-readable replay manifest + page list wabac.js consumes.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::annotations::{self};
use crate::collections::{Manifest, Wacz};
use crate::search::PagesScope;
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
    // Custody, resolved to a display name through the roster — this page is
    // public, so the stored SubjectId must not be printed.
    if let Some(subject) = &c.created_by {
        let name = state
            .users
            .current()
            .resolve(subject.clone())
            .display_name()
            .to_string();
        meta.push(views::MetaRow::new("Created by", name));
    }

    // Newest first. A crawl with no recorded date sorts last.
    let mut newest_first = members.clone();
    newest_first.sort_by(|a, b| b.capture_start.cmp(&a.capture_start));
    let member_items: Vec<views::MemberItem> = newest_first
        .iter()
        .map(|w| views::MemberItem {
            id: w.id.clone(),
            name: w.display_name(),
            when: w
                .capture_start
                .as_deref()
                .and_then(crate::collections::long_date),
            present: w.is_present(&state.home),
            remote: w.source.is_remote(),
            provenance: provenance_summary(w),
            thumb: thumb_href(&state.home, &state.index_dir, &w.collection, &w.id),
        })
        .collect();

    // Scoped facet overview: what's *in* this collection, each value a search
    // scoped to it. Turns the page into a faceted entry point, not just a list.
    let scope_id = id.clone();
    let overview = state
        .read_index(move |s| {
            s.facet_overview_scoped(crate::search::FacetScope::Collection(&scope_id))
        })
        .await
        .unwrap_or_default();
    let facets = scoped_facet_sections(&overview.facets, &format!("collection:{id}"));

    let chrome = chrome(&state, &headers);
    // Deaccession is an admin act; don't offer a curator a button that 403s.
    let can_delete =
        resolve_caller(&state, &headers).is_some_and(|(p, _)| p.role().can_administer());
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
        // From the index, like the search timeline, so each bar counts what
        // its `month:` link finds.
        crawl_months: month_axis(
            &overview
                .months
                .iter()
                .map(|t| (t.ym, t.count))
                .collect::<Vec<_>>(),
            |ym| {
                format!(
                    "/search?q={}",
                    url_encode(&format!("collection:{id} month:{ym}"))
                )
            },
            |_| false,
        ),
        replay_href: collection_replay_href(&id, collection_default_page(&members)),
        id: id.clone(),
        can_delete,
        chrome,
        annotation_count: annotations::load(&state.home, &c.id)
            .map(|v| v.len())
            .unwrap_or(0),
        drafted_note: c.drafted.as_ref().map(|d| {
            // Reviewed by whoever last saved the finding aid, resolved through
            // the roster: this page is public, so never the stored identity.
            let reviewer = c.updated_by.as_ref().map(|s| {
                state
                    .users
                    .current()
                    .resolve(s.clone())
                    .display_name()
                    .to_string()
            });
            drafted_note(d, reviewer.as_deref())
        }),
    };
    views::collection(&page).into_response()
}

/// "Description and subjects drafted with M (S), reviewed by R."
fn drafted_note(d: &crate::collections::Drafted, reviewer: Option<&str>) -> String {
    let mut names: Vec<String> = d.fields.iter().map(|f| f.to_string()).collect();
    let list = match names.len() {
        0 => "Some fields".to_string(),
        1 => names.remove(0),
        _ => {
            let last = names.pop().unwrap();
            format!("{} and {last}", names.join(", "))
        }
    };
    let mut list: Vec<char> = list.chars().collect();
    if let Some(first) = list.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    let list: String = list.into_iter().collect();
    let service = if d.service.is_empty() {
        String::new()
    } else {
        format!(" ({})", d.service)
    };
    match reviewer {
        Some(r) => format!("{list} drafted with {}{service}, reviewed by {r}.", d.model),
        // Saved without a known editor (`collection draft --apply` from the
        // command line): claim no review that may not have happened.
        None => format!("{list} drafted with {}{service}.", d.model),
    }
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
    let chrome = chrome(&state, &headers);
    let anns = annotations::load(&state.home, &c.id).unwrap_or_default();
    let items = anns
        .iter()
        .map(|a| {
            let region = a.target.selector.as_ref().map(|s| match s {
                annotations::Selector::TextQuoteSelector { exact, .. } => exact.clone(),
            });
            views::AnnoLink {
                // This page is public, so the sanitized name — never the stored
                // login identity (see `Creator::public_name`).
                author: a.creator.public_name().unwrap_or("anonymous").to_string(),
                date: a
                    .modified
                    .clone()
                    .unwrap_or_else(|| a.created.clone())
                    .chars()
                    .take(10)
                    .collect(),
                note_html: crate::markdown::render(&a.body.value),
                page_url: a.target.source.clone(),
                // Names the note, so the page opens on its passage.
                replay_href: match replay_href(
                    ReplayScope::Collection(&id),
                    &a.target.timestamp,
                    &a.target.source,
                ) {
                    Some(href) => format!("{href}#note={}", url_encode(&a.id)),
                    // A page replay can't open: the collection instead.
                    None => format!("/collection/{}", url_encode(&id)),
                },
                region,
            }
        })
        .collect();
    let page = views::AnnotationsIndexPage {
        collection_name: c.name.clone(),
        collection_id: id.clone(),
        items,
        chrome,
    };
    views::annotations_index(&page).into_response()
}

/// A wabac (ReplayWeb.page) multi-WACZ collection manifest for a collection: the
/// JSON that `<replay-web-page source="…/replay.json">` loads to replay every
/// member crawl as one collection. Each member maps to a resource pointing at the
/// same byte-serving endpoint single-WACZ replay already uses (`viewer_source`):
/// `/files/{id}` for local/Browsertrix sources, the remote URL for a plain URL.
///
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
    let files: Vec<(String, &Wacz)> = manifest
        .members_of(&id)
        .map(|w| (w.id.clone(), w))
        .collect();
    replay_manifest(
        &state,
        &files,
        format!("/collection/{}/pages", url_encode(&c.id)),
        &c.name,
        c.description.as_deref(),
    )
    .await
}

/// A wabac multi-WACZ manifest of `files`, for a collection's replay or for
/// one crawl written into several WACZs (`crawl_replay_json`).
///
/// Each file is listed under the name paired with it, which the pages
/// endpoint at `pages_url` returns as `filename`, so wabac can match a
/// looked-up page to its member: a collection names a file by its WACZ id, a
/// crawl by its place in the crawl (`crawl_file_names`). `hash` carries the
/// `sha256:` prefix wabac expects.
///
/// A streamed Browsertrix file's `path` is a presigned URL to its bytes, so
/// wabac reads them directly instead of through `/files/{id}`, which would
/// add a redirect to every one of a page's hundreds of range reads. If the URL
/// can't be had, the file falls back to `/files/{id}`, which tries again and
/// says why it can't. A presigned URL expires, so the manifest is not to be
/// cached; when a read is refused once it has, wabac fetches the manifest
/// again.
pub(super) async fn replay_manifest(
    state: &AppState,
    files: &[(String, &Wacz)],
    pages_url: String,
    title: &str,
    desc: Option<&str>,
) -> Response {
    let presigned = match &state.resolver {
        Some(resolver) => {
            let waczs: Vec<&Wacz> = files.iter().map(|(_, w)| *w).collect();
            super::presigned::urls_for(&state.presigned, resolver, &waczs).await
        }
        None => HashMap::new(),
    };
    let resources: Vec<serde_json::Value> = files
        .iter()
        .map(|(name, w)| {
            let path = presigned
                .get(&w.source.location())
                .cloned()
                .unwrap_or_else(|| viewer_source(w));
            let mut res = serde_json::json!({
                "name": name,
                "path": path,
                "crawlId": name,
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
        return (StatusCode::NOT_FOUND, "nothing to replay").into_response();
    }
    let body = serde_json::json!({
        "resources": resources,
        // Where wabac asks which member holds a page (`collection_pages`).
        // Without it, wabac opens and indexes every member WACZ before it can
        // show any page: for a collection of streamed crawls, hundreds of
        // remote reads. With it, wabac opens only the member that holds the
        // page being read. Top-level, not under `metadata`, because that is
        // where wabac's `loadFromJSON` reads it. Relative, so it resolves on
        // whichever origin serves replay.
        "pagesQueryUrl": pages_url,
        "metadata": {
            "title": title,
            "desc": desc,
        },
    });
    (
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        axum::Json(body),
    )
        .into_response()
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
    pages_response(&state, PagesFor::Collection(id), params).await
}

/// What a pages endpoint answers for: [`crate::search::PagesScope`], owned, so
/// it can move to the thread that reads the index.
pub(super) enum PagesFor {
    Collection(String),
    /// Each file's WACZ id, and the name its crawl's manifest lists it under.
    Crawls(Vec<(String, String)>),
}

/// The body of a wabac `pagesQueryUrl` endpoint, for a collection's replay or
/// for one crawl's (`crawl_pages`).
pub(super) async fn pages_response(
    state: &AppState,
    scope: PagesFor,
    params: PagesParams,
) -> Response {
    if let Some(refusal) = params.search.as_deref().and_then(query_too_long) {
        return refusal;
    }
    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(25).clamp(1, 200);
    // Saturating, so an absurd `page` can't overflow; the index caps how
    // deep it will go (`MAX_PAGES_OFFSET`).
    let offset = (page - 1).saturating_mul(page_size);
    let PagesParams { url, search, .. } = params;
    // What each hit's file is called in the manifest: its id, unless renamed.
    let names: HashMap<String, String> = match &scope {
        PagesFor::Collection(_) => HashMap::new(),
        PagesFor::Crawls(files) => files.iter().cloned().collect(),
    };
    match state
        .read_index(move |s| match &scope {
            PagesFor::Collection(id) => s.pages(
                PagesScope::Collection(id),
                url.as_deref(),
                search.as_deref(),
                offset,
                page_size,
            ),
            PagesFor::Crawls(files) => {
                let ids: Vec<String> = files.iter().map(|(id, _)| id.clone()).collect();
                s.pages(
                    PagesScope::Crawls(&ids),
                    url.as_deref(),
                    search.as_deref(),
                    offset,
                    page_size,
                )
            }
        })
        .await
    {
        Ok((total, hits)) => {
            let items: Vec<serde_json::Value> = hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "url": h.url,
                        "ts": ts_to_iso(&h.timestamp),
                        "title": h.title,
                        "filename": names.get(&h.crawl_id).unwrap_or(&h.crawl_id),
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
