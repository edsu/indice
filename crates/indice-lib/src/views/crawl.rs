//! Crawl detail: one WACZ's provenance, capture range, and page list.

use maud::{html, Markup, PreEscaped};

use super::*;

/// A seed page listed on a crawl detail page.
pub struct PageItem {
    pub href: String,
    pub title: String,
    pub url: String,
}

/// All the data the crawl detail page renders. The handler resolves links,
/// formats sizes/dates, and gathers provenance/file rows; the view lays them out.
pub struct CrawlPage {
    /// The crawl's own id (for the management delete action).
    pub id: String,
    /// `(collection_id, collection_name)` breadcrumb, if the crawl has one.
    pub crumb: Option<(String, String)>,
    pub name: String,
    /// The crawl's own name, when [`name`](Self::name) is the date-based one
    /// shown in its place.
    pub original_name: Option<String>,
    pub description: Option<String>,
    /// The rendered (safe-HTML) Markdown curator note from
    /// `collections/<slug>/crawls/<id>.md`.
    pub note: Option<PreEscaped<String>>,
    /// `/thumb/{id}` for this crawl's representative image, if it has one.
    pub thumb: Option<String>,
    /// The address that replays the crawl's first seed page; `None` when it
    /// recorded none, as replay would have no page to open.
    pub replay_href: Option<String>,
    /// Whether the crawl is hosted remotely (a URL or a streamed Browsertrix
    /// source) rather than stored in `<home>/archive`.
    pub remote: bool,
    pub provenance: Vec<MetaRow>,
    pub source: String,
    pub size: String,
    pub sha_short: String,
    pub sha_full: String,
    pub crawled: Option<String>,
    pub indexed: String,
    pub present: bool,
    /// Scoped facet overview of what this crawl captured (sites/years/types/…).
    pub facets: Vec<FacetSection>,
    pub pages: Vec<PageItem>,
    /// Whether to render the danger zone (admins only — see CollectionPage).
    pub can_delete: bool,
    /// Who is looking, for the shared header.
    pub chrome: Chrome,
}

/// The crawl detail page: provenance panel, file metadata, and seed-page list.
pub fn crawl(p: &CrawlPage) -> Markup {
    let l10n = &p.chrome.l10n;
    let t = |key: &str| l10n.tr(key);
    let body = html! {
        @if let Some((id, cname)) = &p.crumb {
            div.crumb { (t("search-in-collection")) " " a href=(format!("/collection/{}", id)) { (cname) } }
        }
        // A crawl's own page shows its thumbnail only when it has one.
        @if let Some(t) = &p.thumb {
            div.detail-thumb { (thumb_area(Some(t))) }
        }
        div.crawl-title {
            (source_badge(p.remote, l10n))
            h1.page-title { (p.name) }
        }
        @if let Some(d) = &p.description { p.desc { (d) } }
        @if let Some(href) = &p.replay_href {
            div.actions { a.btn href=(href) { (t("action-replay")) } }
        }

        @if let Some(n) = &p.note {
            section.about {
                h2 { (t("crawl-curator-note")) }
                div.narrative { (n) }
            }
        }

        @if !p.provenance.is_empty() {
            h2 { (t("crawl-provenance")) }
            (meta_table(&p.provenance))
        }

        h2 { (t("crawl-file")) }
        table.meta {
            tr { th { (t("meta-source")) } td { (p.source) } }
            @if let Some(n) = &p.original_name {
                tr { th { (t("meta-named")) } td.mono { (n) } }
            }
            tr { th { (t("meta-size")) } td { (p.size) } }
            // Not every source records a hash; a missing one is left out, not
            // shown as an empty value.
            @if !p.sha_full.is_empty() {
                tr { th { (t("meta-sha256")) } td.mono title=(p.sha_full) { (p.sha_short) "…" } }
            }
            @if let Some(c) = &p.crawled { tr { th { (t("meta-crawled")) } td { (c) } } }
            tr { th { (t("meta-indexed")) } td { (p.indexed) } }
            tr {
                th { (t("meta-status")) }
                td {
                    @if p.present { span.ok { (t("crawl-status-present")) } } @else { span.missing { (t("crawl-status-missing")) } }
                }
            }
        }

        (facet_browse(&p.facets, l10n))

        h2 { (t("crawl-pages-heading")) }
        @if p.pages.is_empty() {
            p.muted { (t("crawl-no-pages")) }
        } @else {
            ul.pages {
                @for pg in &p.pages {
                    li {
                        a href=(pg.href) { (pg.title) }
                        div.result-url { (pg.url) }
                    }
                }
            }
        }

        @if p.can_delete {
            details.danger-zone {
                summary { (t("crawl-delete-summary")) }
                form.confirm-delete method="post" action=(format!("/api/crawls/{}/delete", p.id)) {
                    p.muted { (t("crawl-delete-note")) }
                    button.btn.danger type="submit" { (t("action-delete-permanently")) }
                }
            }
        }
    };
    // Header search scoped to the crawl's collection when it has one.
    let search = match &p.crumb {
        Some((id, cname)) => SearchBox {
            scope_query: format!("collection:{id}"),
            scope_label: cname.clone(),
            ..Default::default()
        },
        None => SearchBox::default(),
    };
    layout(Some(&p.name), &p.chrome, Some(&search), body)
}

// ── Management UI ───────────────────────────────────────────
//
// Edit-in-place: the collections list is the homepage, and collections are
// edited from their own pages. Only the two multi-step accessions live on
// dedicated workroom pages — the finding-aid form and the "add crawls" desk.
