//! The search results page: the count line, active filters, the month
//! timeline, the facet sidebar, and the result rows.

use maud::{html, Markup, PreEscaped};

use super::*;

/// A human label for an HTTP status code, for the result badge's `title`
/// tooltip. Common archived-error codes get a phrase (`http-status-<code>`);
/// anything else falls back to the bare code.
fn http_status_label(l10n: &L10n, code: u16) -> String {
    let key = format!("http-status-{code}");
    let code = code.to_string();
    let code = FluentArg::Str(&code);
    // A code with no message has no phrase: the lookup would answer with the key.
    if l10n.has_message(&key) {
        let reason = l10n.tr(&key);
        l10n.tr_args(
            "search-status-title",
            &[("code", code), ("reason", FluentArg::Str(&reason))],
        )
    } else {
        l10n.tr_args("search-status-title-bare", &[("code", code)])
    }
}

/// The count line's number and noun, "1 result" or "1,000+ results". `capped`
/// shows the total as a floor.
pub fn results_count(l10n: &L10n, hits: usize, capped: bool) -> String {
    let plus = if capped { "+" } else { "" };
    counted_text(
        l10n,
        "search-results-count",
        hits as u64,
        &format!("{}{plus}", l10n.number(hits as u64)),
    )
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
fn facet_item(it: &FacetItem, l10n: &L10n) -> Markup {
    html! {
        li.facet-item.active[it.active] {
            // aria-current says which value is applied; the checkmark beside it
            // is drawn by CSS, which a screen reader does not announce.
            a href=(it.href) aria-current=[it.active.then_some("true")] {
                span.facet-value { (it.value) }
                span.facet-count { (l10n.number(it.count)) }
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

/// "Page 2 of 5".
fn page_of(l10n: &L10n, nav: &PageNav) -> String {
    l10n.tr_args(
        "search-page-of",
        &[
            ("page", FluentArg::Str(&l10n.number(nav.page as u64))),
            (
                "total",
                FluentArg::Str(&l10n.number(nav.total_pages as u64)),
            ),
        ],
    )
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
    let l10n = &chrome.l10n;
    let page_href = |p: usize| format!("/search?q={}&page={}", nav.query_encoded, p);
    let body = html! {
        (search_tips(l10n))
        div.count {
            @if nav.total_hits == 0 {
                (l10n.tr("search-nothing-matched")) " " em { (query) } "."
            } @else {
                (results_count(l10n, nav.total_hits, nav.capped)) " " (l10n.tr("search-results-for")) " " em { (query) }
                @if nav.total_pages > 1 {
                    ". " (page_of(l10n, nav))
                }
            }
        }
        @if !sidebar.active.is_empty() {
            div.active-filters {
                span.active-label { (l10n.tr("search-filters")) }
                @for f in &sidebar.active {
                    a.filter-chip href=(f.remove_href) {
                        span.chip-label { (f.label) ": " }
                        (f.value) " ✕"
                    }
                }
            }
        }
        (time_axis(timeline, l10n, &l10n.tr("search-timeline-label"), "search-results-count"))
        // An empty result says what to try next: each applied filter can be
        // dropped (a collection filter is how a scoped search arrives), then
        // the query itself, then browsing.
        @if nav.total_hits == 0 {
            ul.next-steps {
                @for f in &sidebar.active {
                    li {
                        a href=(f.remove_href) {
                            @if f.field == "collection" {
                                (l10n.tr("search-all-collections"))
                            } @else {
                                (l10n.tr_args(
                                    "search-without",
                                    &[("label", FluentArg::Str(&f.label)), ("value", FluentArg::Str(&f.value))],
                                ))
                            }
                        }
                    }
                }
                li { (l10n.tr("search-try-fewer")) }
                li { a href="/" { (l10n.tr("search-browse-link")) } }
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
                                                span.result-coll-badge { (l10n.tr("search-badge-collection")) }
                                            } @else if r.is_annotation {
                                                span.result-note-badge { (l10n.tr("search-badge-note")) }
                                                @if !r.author.is_empty() {
                                                    span.result-note-author { " " (l10n.tr_args("search-note-by", &[("author", FluentArg::Str(&r.author))])) }
                                                }
                                            } @else {
                                                div.result-url { (r.url) }
                                            }
                                            @if !r.is_collection && !r.timestamp_display.is_empty() {
                                                div.result-ts {
                                                    (r.timestamp_display)
                                                    @if r.capture_count > 1 {
                                                        span.capture-count { " " (counted(l10n, "search-captured-times", r.capture_count as u64)) }
                                                    }
                                                }
                                            }
                                            // Flag archived non-200 captures (404/500/…); 200 stays unmarked.
                                            @if let Some(code) = r.status {
                                                @if code != 200 {
                                                    span.result-status title=(http_status_label(l10n, code)) { "HTTP " (code) }
                                                }
                                            }
                                        }
                                        @if let Some(s) = &r.snippet_html {
                                            div.snippet { (PreEscaped(s)) }
                                        }
                                        div.result-coll {
                                            (l10n.tr("search-in-collection")) " " a href=(format!("/collection/{}", r.coll_href)) { em { (r.coll_display) } }
                                        }
                                    }
                                    td.replay-col {
                                        a.btn.small href=(r.href) { (l10n.tr("action-replay")) }
                                    }
                                }
                            }
                        }
                    }
                }
                @if nav.total_pages > 1 {
                    nav.pagination {
                        @if nav.page > 1 {
                            a.page-prev href=(page_href(nav.page - 1)) { (l10n.tr("search-previous")) }
                        } @else {
                            span.page-prev.disabled { (l10n.tr("search-previous")) }
                        }
                        span.page-info { (page_of(l10n, nav)) }
                        @if nav.page < nav.total_pages {
                            a.page-next href=(page_href(nav.page + 1)) { (l10n.tr("search-next")) }
                        } @else {
                            span.page-next.disabled { (l10n.tr("search-next")) }
                        }
                    }
                }
            }
            // After the results, so they come first on a phone and for a
            // keyboard; beside them on a wide screen. Each group shows its first
            // FACET_SHOWN values plus any applied one, and the rest behind a
            // disclosure: a site facet can run to hundreds of hosts.
            @if !sidebar.groups.is_empty() {
                aside.facets aria-label=(l10n.tr("search-refine")) {
                    h2.facets-title { (l10n.tr("search-refine")) }
                    @for g in &sidebar.groups {
                        @let (shown, rest) = split_facet_items(&g.items);
                        div.facet-group {
                            h3 { (g.label) }
                            ul {
                                @for it in &shown { (facet_item(it, l10n)) }
                            }
                            @if !rest.is_empty() {
                                details.facet-more {
                                    // "More", not "all": the index returns at most 50 values a facet.
                                    summary { (l10n.tr_args("search-show-more", &[("shown", FluentArg::Str(&l10n.number(rest.len() as u64)))])) }
                                    ul {
                                        @for it in &rest { (facet_item(it, l10n)) }
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
    layout(Some(query), chrome, Some(&search), body)
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
