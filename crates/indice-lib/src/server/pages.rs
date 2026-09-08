//! The reading-room entry points: health, the homepage, and the search
//! results page.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::collections::{Manifest, Wacz};
use crate::views;

use super::*;

/// Liveness/readiness probe for a reverse proxy or orchestrator (Docker
/// HEALTHCHECK, Kubernetes, YunoHost, …). Deliberately trivial and un-gated: the
/// server only starts once the index has opened, so a 200 here means the process
/// is up and serving. Returns `ok` as plain text.
pub(super) async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

pub(super) async fn homepage(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e).into_response(),
    };

    let cards: Vec<views::CollectionCard> = manifest
        .collections
        .iter()
        .map(|c| {
            let members: Vec<&Wacz> = manifest.members_of(&c.id).collect();
            views::CollectionCard {
                id: c.id.clone(),
                name: c.name.clone(),
                count: members.len(),
                description: c.description.clone(),
                replay_href: collection_replay_href(
                    &c.id,
                    &c.name,
                    collection_default_page(&members),
                ),
                // Capture date range (temporal span is meaningful at the
                // collection level; per-tool software lives on the WACZ page).
                date_range: members_capture_range(&members),
                // Representative image: a curator-set collection thumbnail if
                // present, else the first member crawl that has one.
                thumb: collection_thumb_href(&state.home, &c.id).or_else(|| {
                    members.iter().find_map(|w| {
                        thumb_href(&state.home, &state.index_dir, &w.collection, &w.id)
                    })
                }),
                // Which source kinds the members span — both true = mixed.
                has_local: members.iter().any(|w| !w.source.is_remote()),
                has_remote: members.iter().any(|w| w.source.is_remote()),
            }
        })
        .collect();

    // Browse entry points: years (most recent first) and the busiest sites,
    // each a search link. Derived from an archive-wide facet overview.
    let overview = state
        .search
        .read()
        .unwrap()
        .facet_overview()
        .unwrap_or_default();
    let browse = views::HomeBrowse {
        years: browse_links(&overview, "year", "year", 12, true),
        sites: browse_links(&overview, "site", "site", 8, false),
    };

    let (manage, who) = admin_ctx(&state, &headers);
    let can_login = login_available(&state, &who);
    views::home(&cards, &browse, manage, who.as_deref(), can_login).into_response()
}

/// Build homepage browse links from one facet dimension: `field` is the facet
/// group to read, `query_field` the `field:value` used in the search link.
/// `by_value_desc` sorts by the value (e.g. year, newest first) instead of by
/// count; `max` caps how many are shown.
fn browse_links(
    overview: &[crate::search::FacetGroup],
    field: &str,
    query_field: &str,
    max: usize,
    by_value_desc: bool,
) -> Vec<views::BrowseLink> {
    let Some(group) = overview.iter().find(|g| g.field == field) else {
        return Vec::new();
    };
    let mut buckets: Vec<&crate::search::FacetBucket> = group.buckets.iter().collect();
    if by_value_desc {
        buckets.sort_by(|a, b| b.value.cmp(&a.value));
    }
    buckets
        .into_iter()
        .take(max)
        .map(|b| views::BrowseLink {
            label: b.value.clone(),
            count: b.count,
            href: format!(
                "/search?q={}",
                url_encode(&format!("{query_field}:{}", b.value))
            ),
        })
        .collect()
}

/// Search results per page.
const PAGE_SIZE: usize = 20;

/// Format a `YYYYMM` month as `YYYY-MM` for display.
fn format_ym(ym: u64) -> String {
    format!("{:04}-{:02}", ym / 100, ym % 100)
}

/// The active `field:value` facet filters present in a query, in order. Only
/// single-token filters are recognized: a range like `month:[202101 TO 202106]`
/// is a valid query but splits into several whitespace tokens, so it does not
/// appear as a removable chip. Filter fields come from `search::is_filter_field`
/// so this stays in sync with the facet dimensions.
pub(super) fn active_filters(q: &str) -> Vec<(String, String)> {
    q.split_whitespace()
        .filter_map(|tok| {
            let (f, v) = tok.split_once(':')?;
            (crate::search::is_filter_field(f) && !v.is_empty())
                .then(|| (f.to_string(), v.to_string()))
        })
        .collect()
}

/// Add a `field:value` filter to a query, leaving the rest (including quoted
/// phrases) untouched. A no-op if that exact filter is already present.
pub(super) fn query_with_filter(q: &str, field: &str, value: &str) -> String {
    let token = format!("{field}:{value}");
    let base = q.trim();
    if base.split_whitespace().any(|t| t == token) {
        return base.to_string();
    }
    if base.is_empty() {
        token
    } else {
        format!("{base} {token}")
    }
}

/// Remove a `field:value` filter from a query (all occurrences of that token).
pub(super) fn query_without_filter(q: &str, field: &str, value: &str) -> String {
    let token = format!("{field}:{value}");
    q.split_whitespace()
        .filter(|t| *t != token)
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Deserialize)]
pub(super) struct SearchPageParams {
    q: String,
    /// A `field:value` token to scope the search (e.g. `collection:<id>`),
    /// carried by the header search box when viewing a collection/crawl. ANDed
    /// into the query so it rides the normal filter machinery (removable chip).
    #[serde(default)]
    scope: String,
    /// 1-based page number; absent/`<1` means the first page.
    page: Option<usize>,
}

pub(super) async fn search_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<SearchPageParams>,
) -> impl IntoResponse {
    // Fold any scope token (e.g. `collection:<id>` from the header search on a
    // collection page) into the query, so downstream faceting + the removable
    // active-filter chip treat it like any other `field:value`.
    let typed = params.q.trim();
    let scope = params.scope.trim();
    let q = if scope.is_empty() || typed.split_whitespace().any(|t| t == scope) {
        typed.to_string()
    } else if typed.is_empty() {
        scope.to_string()
    } else {
        format!("{scope} {typed}")
    };
    if q.is_empty() {
        return (
            StatusCode::SEE_OTHER,
            [("location", "/"), ("content-type", "text/html")],
            String::new(),
        )
            .into_response();
    }

    let page = params.page.unwrap_or(1).max(1);
    let offset = (page - 1) * PAGE_SIZE;
    let response = match state
        .search
        .read()
        .unwrap()
        .search_faceted(&q, PAGE_SIZE, offset)
    {
        Ok(r) => r,
        Err(e) => return error_response(e).into_response(),
    };
    let results = &response.results;

    // Map each WACZ id to the wabac `source` to use: /files/{id} for a local
    // WACZ, or the remote URL directly for an http source.
    let waczs = load_waczs(&state);
    let source_for = |wacz_id: &str| -> String {
        waczs
            .iter()
            .find(|w| w.id == wacz_id)
            .map(viewer_source)
            .unwrap_or_else(|| format!("/files/{wacz_id}"))
    };
    // Curated collection id -> display name, for the "in <collection>" link.
    let collection_names: std::collections::HashMap<String, String> =
        Manifest::open(&state.index_dir)
            .map(|m| {
                m.collections
                    .iter()
                    .map(|c| (c.id.clone(), c.name.clone()))
                    .collect()
            })
            .unwrap_or_default();

    let rows: Vec<views::SearchResultRow> = results
        .iter()
        .map(|r| {
            let is_collection = r.doc_type == "collection";
            let is_annotation = r.doc_type == "annotation";

            // The curated collection this result belongs to (falls back to the
            // slug/id if the name isn't found).
            let coll_display = collection_names
                .get(&r.collection)
                .map(String::as_str)
                .unwrap_or(&r.collection)
                .to_string();
            let coll_href = url_encode(&r.collection);

            let title = if is_annotation {
                // A note has no page title; show the annotated page's URL.
                if r.url.is_empty() {
                    "Note".to_string()
                } else {
                    r.url.clone()
                }
            } else if r.title.is_empty() {
                if is_collection {
                    r.crawl_name.clone()
                } else {
                    r.url.clone()
                }
            } else {
                r.title.clone()
            };

            let href = if is_annotation {
                // Notes carry no crawl; open the annotated page in the collection
                // replay (where the note re-anchors + highlights via the panel).
                collection_replay_href(
                    &r.collection,
                    &coll_display,
                    Some((r.url.clone(), r.timestamp.clone())),
                )
            } else {
                let name_enc = url_encode(&r.crawl_name);
                let source_enc = url_encode(&source_for(&r.crawl_id));
                // Carry the breadcrumb into the replay viewer: the collection
                // (name + id) and the crawl id (so its crumb links to the crawl).
                let coll_q = format!(
                    "&collection={}&collection_id={coll_href}&crawl={}",
                    url_encode(&coll_display),
                    url_encode(&r.crawl_id)
                );
                if is_collection {
                    // Link to the collection's root in the viewer.
                    format!("/replay/viewer?source={source_enc}&name={name_enc}{coll_q}")
                } else {
                    format!(
                        "/replay/viewer?source={source_enc}&url={}&ts={}&name={name_enc}{coll_q}",
                        url_encode(&r.url),
                        r.timestamp
                    )
                }
            };

            // Prefer the hit-highlighted body snippet; if the query didn't match
            // the body (e.g. a title-only or URL-only hit), fall back to the
            // page's description so the result still has context. The snippet is
            // already-safe HTML (Tantivy emits `<b>` tags); the description is
            // plain text, so escape it before splicing as pre-escaped HTML.
            // Fallback chain: the hit-highlighted body snippet; else the page
            // description; else a plain leading excerpt of the stored body prefix
            // (e.g. a title/URL-only hit, or a match deeper than the stored cap);
            // else nothing (the row shows title + URL). Plain text is escaped
            // before splicing as pre-escaped HTML; the snippet is already safe.
            let snippet_html = if !r.snippet.is_empty() {
                Some(r.snippet.clone())
            } else if !r.description.is_empty() {
                Some(html_escape(&r.description))
            } else if !r.body_excerpt.is_empty() {
                Some(html_escape(&r.body_excerpt))
            } else {
                None
            };

            let timestamp_display = if !is_collection && !r.timestamp.is_empty() {
                format_timestamp(&r.timestamp)
            } else {
                String::new()
            };

            views::SearchResultRow {
                href,
                title,
                is_collection,
                is_annotation,
                author: r.author.clone(),
                url: r.url.clone(),
                timestamp_display,
                snippet_html,
                coll_href,
                coll_display,
                capture_count: r.capture_count,
                status: r.status,
            }
        })
        .collect();

    let total_pages = response.total_hits.div_ceil(PAGE_SIZE).max(1);
    let page_nav = views::PageNav {
        page,
        total_pages,
        total_hits: response.total_hits,
        capped: response.capped,
        query_encoded: url_encode(&q),
    };

    // Facet sidebar: clickable buckets that add/remove a `field:value` filter,
    // plus chips for the filters already active in the query. Refining resets
    // to page 1.
    let filters = active_filters(&q);
    let search_href = |new_q: &str| format!("/search?q={}", url_encode(new_q));
    // The `crawl:` filter's value is an opaque WACZ id (from a crawl-page facet
    // link); show the crawl's name in the chip instead. Other filters show their
    // value as-is. The removal token still uses the raw id.
    let manifest = Manifest::open(&state.index_dir).ok();
    let active: Vec<views::ActiveFilter> = filters
        .iter()
        .map(|(f, v)| {
            let display = if f == "crawl" {
                manifest
                    .as_ref()
                    .and_then(|m| m.wacz_by_id(v))
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| v.clone())
            } else {
                v.clone()
            };
            views::ActiveFilter {
                label: crate::search::filter_label(f).to_string(),
                value: display,
                remove_href: search_href(&query_without_filter(&q, f, v)),
            }
        })
        .collect();
    let groups: Vec<views::FacetGroupView> = response
        .facets
        .iter()
        .map(|g| views::FacetGroupView {
            label: g.label.clone(),
            items: g
                .buckets
                .iter()
                .map(|b| {
                    let is_active = filters.iter().any(|(f, v)| f == &g.field && v == &b.value);
                    let new_q = if is_active {
                        query_without_filter(&q, &g.field, &b.value)
                    } else {
                        query_with_filter(&q, &g.field, &b.value)
                    };
                    views::FacetItem {
                        value: b.value.clone(),
                        count: b.count,
                        href: search_href(&new_q),
                        active: is_active,
                    }
                })
                .collect(),
        })
        .collect();
    let sidebar = views::FacetSidebar { active, groups };

    // Timeline: one clickable bar per crawl month, oldest first, height scaled
    // to the busiest month. Clicking toggles a `month:YYYYMM` filter.
    let max_count = response
        .timeline
        .iter()
        .map(|t| t.count)
        .max()
        .unwrap_or(1)
        .max(1);
    let timeline: Vec<views::TimelineBar> = response
        .timeline
        .iter()
        .map(|t| {
            let month = t.ym.to_string();
            let is_active = filters.iter().any(|(f, v)| f == "month" && v == &month);
            let new_q = if is_active {
                query_without_filter(&q, "month", &month)
            } else {
                query_with_filter(&q, "month", &month)
            };
            views::TimelineBar {
                label: format_ym(t.ym),
                count: t.count,
                pct: (t.count as f64 / max_count as f64 * 100.0).round() as u32,
                href: search_href(&new_q),
                active: is_active,
            }
        })
        .collect();

    let (manage, who) = admin_ctx(&state, &headers);
    let can_login = login_available(&state, &who);
    views::search_results(
        &q,
        &page_nav,
        &sidebar,
        &timeline,
        &rows,
        manage,
        who.as_deref(),
        can_login,
    )
    .into_response()
}
