//! The search results page: the count line, active filters, the month
//! timeline, the facet sidebar, and the result rows.

use maud::{html, Markup, PreEscaped};

use super::*;

/// A human label for an HTTP status code, for the result badge's `title`
/// tooltip. Common archived-error codes get a phrase; anything else falls back
/// to the bare code.
fn http_status_label(code: u16) -> String {
    let reason = match code {
        301 => "Moved Permanently",
        302 => "Found (redirect)",
        307 | 308 => "Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    };
    if reason.is_empty() {
        format!("Archived HTTP {code}")
    } else {
        format!("Archived HTTP {code} {reason}")
    }
}

/// How many values of a facet show before "Show all".
const FACET_SHOWN: usize = 8;

/// A facet group's values split into those shown and those behind "Show all":
/// the first [`FACET_SHOWN`], plus any applied one wherever it falls, so a
/// filter in use is never hidden.
fn split_facet_items(items: &[FacetItem]) -> (Vec<&FacetItem>, Vec<&FacetItem>) {
    let mut shown = Vec::new();
    let mut rest = Vec::new();
    for (i, it) in items.iter().enumerate() {
        if i < FACET_SHOWN || it.active {
            shown.push(it);
        } else {
            rest.push(it);
        }
    }
    (shown, rest)
}

/// One facet value in the sidebar.
fn facet_item(it: &FacetItem) -> Markup {
    html! {
        li.facet-item.active[it.active] {
            // aria-current says which value is applied; the checkmark beside it
            // is drawn by CSS, which a screen reader does not announce.
            a href=(it.href) aria-current=[it.active.then_some("true")] {
                span.facet-value { (it.value) }
                span.facet-count { (it.count) }
            }
        }
    }
}

/// One row of the search results table. The handler computes the replay `href`,
/// display strings, and the (pre-escaped) snippet HTML; the view just lays it out.
pub struct SearchResultRow {
    pub href: String,
    pub title: String,
    pub is_collection: bool,
    /// True for a `doc_type = "annotation"` hit: a curator's note, shown with a
    /// "Note" badge + author instead of a page URL, linking to the annotated page.
    pub is_annotation: bool,
    /// The note author, for an annotation hit (empty otherwise).
    pub author: String,
    /// Display URL (empty for a collection-level hit, which shows a badge).
    pub url: String,
    /// Pre-formatted timestamp, empty when there is none to show.
    pub timestamp_display: String,
    /// Pre-escaped snippet HTML (may contain Tantivy `<b>` highlight tags).
    pub snippet_html: Option<String>,
    /// URL-encoded curated-collection id, for the "in <collection>" link.
    pub coll_href: String,
    /// Display name of the curated collection.
    pub coll_display: String,
    /// How many captures of this URL matched (>1 shows a "captured N times" note).
    pub capture_count: usize,
    /// HTTP status of the capture, when recorded. A badge is shown only for
    /// non-200 (archived error pages); 200 is the norm and stays unmarked.
    pub status: Option<u16>,
}

/// Pagination state for the results page: the current 1-based page, the total
/// number of pages, and the total match count (across all pages).
pub struct PageNav {
    pub page: usize,
    pub total_pages: usize,
    pub total_hits: usize,
    /// True when more captures matched than were scanned for grouping, so the
    /// total is shown as a floor (e.g. "1000+").
    pub capped: bool,
    /// The URL-encoded query, so page links can preserve it.
    pub query_encoded: String,
}

/// The facet sidebar: the filters currently active in the query, plus a group
/// of clickable counts per facet dimension.
pub struct FacetSidebar {
    pub active: Vec<ActiveFilter>,
    pub groups: Vec<FacetGroupView>,
}

/// A `field:value` filter currently applied, with a link that removes it.
pub struct ActiveFilter {
    /// The query field, e.g. `collection`.
    pub field: String,
    pub label: String,
    pub value: String,
    pub remove_href: String,
}

/// One facet dimension in the sidebar.
pub struct FacetGroupView {
    pub label: String,
    pub items: Vec<FacetItem>,
}

/// One clickable facet value: its count, the link that toggles it, and whether
/// it is currently applied.
pub struct FacetItem {
    pub value: String,
    pub count: u64,
    pub href: String,
    pub active: bool,
}

/// One bar of the results timeline: a crawl month, its count, a height
/// percentage (0–100), a toggle link, and whether that month is filtered.
pub struct TimelineBar {
    pub label: String,
    pub count: u64,
    pub pct: u32,
    pub href: String,
    pub active: bool,
}

/// The search results page: top bar, tips, a count line, an active-filter row,
/// a month timeline, then a facet sidebar beside the results table with
/// prev/next pagination.
pub fn search_results(
    query: &str,
    nav: &PageNav,
    sidebar: &FacetSidebar,
    timeline: &[TimelineBar],
    rows: &[SearchResultRow],
    chrome: &Chrome,
) -> Markup {
    // Preserve the query when linking to another page.
    let page_href = |p: usize| format!("/search?q={}&page={}", nav.query_encoded, p);
    let body = html! {
        (search_tips())
        div.count {
            @if nav.total_hits == 0 {
                "Nothing matched " em { (query) } "."
            } @else {
                (nav.total_hits) @if nav.capped { "+" } " result" @if nav.total_hits != 1 { "s" } " for " em { (query) }
                @if nav.total_pages > 1 {
                    " · page " (nav.page) " of " (nav.total_pages)
                }
            }
        }
        @if !sidebar.active.is_empty() {
            div.active-filters {
                span.active-label { "Filters:" }
                @for f in &sidebar.active {
                    a.filter-chip href=(f.remove_href) {
                        span.chip-label { (f.label) ": " }
                        (f.value) " ✕"
                    }
                }
            }
        }
        @if timeline.len() >= 2 {
            div.timeline title="Results by crawl month — click a bar to filter" {
                @for (i, b) in timeline.iter().enumerate() {
                    @let new_year = i == 0 || timeline[i - 1].label.get(..4) != b.label.get(..4);
                    a.tl-bar.active[b.active].tl-year[new_year] href=(b.href) title=(format!("{}: {} result{}", b.label, b.count, if b.count == 1 { "" } else { "s" })) {
                        span.tl-fill style=(format!("height:{}%", b.pct.max(3))) {}
                        span.tl-label { (b.label) }
                        @if new_year { span.tl-yr { (b.label.get(..4).unwrap_or(&b.label)) } }
                    }
                }
            }
        }
        // An empty result says what to try next: each applied filter can be
        // dropped (a collection filter is how a scoped search arrives), then
        // the query itself, then browsing.
        @if nav.total_hits == 0 {
            ul.next-steps {
                @for f in &sidebar.active {
                    li {
                        a href=(f.remove_href) {
                            @if f.field == "collection" {
                                "Search all collections"
                            } @else {
                                "Search without " (f.label) ": " (f.value)
                            }
                        }
                    }
                }
                li { "Try fewer words, or check the spelling." }
                li { a href="/" { "Browse the collections by year or by site" } }
            }
        }
        div.results-layout {
            div.results-main {
                @if !rows.is_empty() {
                    table.results {
                        tbody {
                            @for r in rows {
                                tr {
                                    td {
                                        div.result-title { a href=(r.href) { (r.title) } }
                                        div.result-meta {
                                            @if r.is_collection {
                                                span.result-coll-badge { "Collection" }
                                            } @else if r.is_annotation {
                                                span.result-note-badge { "Note" }
                                                @if !r.author.is_empty() {
                                                    span.result-note-author { " by " (r.author) }
                                                }
                                            } @else {
                                                div.result-url { (r.url) }
                                            }
                                            @if !r.is_collection && !r.timestamp_display.is_empty() {
                                                div.result-ts {
                                                    (r.timestamp_display)
                                                    @if r.capture_count > 1 {
                                                        span.capture-count { " · captured " (r.capture_count) " times" }
                                                    }
                                                }
                                            }
                                            // Flag archived non-200 captures (404/500/…); 200 stays unmarked.
                                            @if let Some(code) = r.status {
                                                @if code != 200 {
                                                    span.result-status title=(http_status_label(code)) { "HTTP " (code) }
                                                }
                                            }
                                        }
                                        @if let Some(s) = &r.snippet_html {
                                            div.snippet { (PreEscaped(s)) }
                                        }
                                        div.result-coll {
                                            "in " a href=(format!("/collection/{}", r.coll_href)) { em { (r.coll_display) } }
                                        }
                                    }
                                    td.replay-col {
                                        a.btn.small href=(r.href) { "Replay →" }
                                    }
                                }
                            }
                        }
                    }
                }
                @if nav.total_pages > 1 {
                    nav.pagination {
                        @if nav.page > 1 {
                            a.page-prev href=(page_href(nav.page - 1)) { "← Previous" }
                        } @else {
                            span.page-prev.disabled { "← Previous" }
                        }
                        span.page-info { "Page " (nav.page) " of " (nav.total_pages) }
                        @if nav.page < nav.total_pages {
                            a.page-next href=(page_href(nav.page + 1)) { "Next →" }
                        } @else {
                            span.page-next.disabled { "Next →" }
                        }
                    }
                }
            }
            // After the results, so they come first on a phone and for a
            // keyboard; beside them on a wide screen. Each group shows its first
            // FACET_SHOWN values plus any applied one, and the rest behind a
            // disclosure: a site facet can run to hundreds of hosts.
            @if !sidebar.groups.is_empty() {
                aside.facets aria-label="Refine" {
                    h2.facets-title { "Refine" }
                    @for g in &sidebar.groups {
                        @let (shown, rest) = split_facet_items(&g.items);
                        div.facet-group {
                            h3 { (g.label) }
                            ul {
                                @for it in &shown { (facet_item(it)) }
                            }
                            @if !rest.is_empty() {
                                details.facet-more {
                                    summary { "Show all " (g.items.len()) }
                                    ul {
                                        @for it in &rest { (facet_item(it)) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    // Header search prefilled with the current query (the results box).
    let search = SearchBox {
        query: query.to_string(),
        ..Default::default()
    };
    layout(&format!("{query} - indice"), chrome, Some(&search), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(n: usize, active: bool) -> FacetItem {
        FacetItem {
            value: format!("v{n}"),
            count: 1,
            href: String::new(),
            active,
        }
    }

    #[test]
    fn a_facet_shows_its_first_values_and_any_applied_one() {
        let items: Vec<FacetItem> = (0..12).map(|n| item(n, n == 10)).collect();
        let (shown, rest) = split_facet_items(&items);
        let names = |v: &[&FacetItem]| v.iter().map(|i| i.value.clone()).collect::<Vec<_>>();
        assert_eq!(
            names(&shown),
            ["v0", "v1", "v2", "v3", "v4", "v5", "v6", "v7", "v10"]
        );
        assert_eq!(names(&rest), ["v8", "v9", "v11"]);
    }
}
