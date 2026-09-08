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
    pub description: Option<String>,
    /// The rendered (safe-HTML) Markdown curator note from
    /// `collections/<slug>/crawls/<id>.md`.
    pub note: Option<PreEscaped<String>>,
    /// `/thumb/{id}` for this crawl's representative image, if it has one.
    pub thumb: Option<String>,
    pub replay_href: String,
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
    /// Whether management mode is on (workroom chrome).
    pub management: bool,
    /// Signed-in user (forward-auth), shown in the workroom strip.
    pub signed_in: Option<String>,
    /// Forward-auth configured but this request anonymous — show a "Log in" link.
    pub can_login: bool,
}

/// The crawl detail page: provenance panel, file metadata, and seed-page list.
pub fn crawl(p: &CrawlPage) -> Markup {
    let body = html! {
        @if let Some((id, cname)) = &p.crumb {
            div.crumb { "in " a href=(format!("/collection/{}", id)) { (cname) } }
        }
        div.detail-thumb { (thumb_area(p.thumb.as_deref(), &p.name)) }
        div.crawl-title {
            (source_badge(p.remote))
            h1.page-title { (p.name) }
        }
        @if let Some(d) = &p.description { p.desc { (d) } }
        a.replay-btn href=(p.replay_href) { "Replay →" }

        @if let Some(n) = &p.note {
            section.about {
                h2 { "Curator's note" }
                div.narrative { (n) }
            }
        }

        @if !p.provenance.is_empty() {
            h2 { "Provenance" }
            (meta_table(&p.provenance))
        }

        h2 { "File" }
        table.meta {
            tr { th { "Source" } td.mono { (p.source) } }
            tr { th { "Size" } td { (p.size) } }
            tr { th { "SHA-256" } td.mono title=(p.sha_full) { (p.sha_short) "…" } }
            @if let Some(c) = &p.crawled { tr { th { "Crawled" } td { (c) } } }
            tr { th { "Indexed" } td { (p.indexed) } }
            tr {
                th { "Status" }
                td {
                    @if p.present { span.ok { "✓ present" } } @else { span.missing { "✗ missing" } }
                }
            }
        }

        (facet_browse(&p.facets))

        h2 { "Pages" }
        @if p.pages.is_empty() {
            p.muted { "No pages are listed in this crawl." }
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

        @if p.management {
            details.danger-zone {
                summary { "Delete this crawl" }
                form.confirm-delete method="post" action=(format!("/api/crawls/{}/delete", p.id)) {
                    p.muted { "Permanently removes this crawl: its pages from search, the local WACZ file, and its thumbnail. This can't be undone." }
                    button.btn.danger type="submit" { "Delete permanently" }
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
    layout(
        &format!("{} - indice", p.name),
        p.management,
        p.signed_in.as_deref(),
        p.can_login,
        Some(&search),
        body,
    )
}

// ── Management UI (serve --manage) ───────────────────────────────────────────
//
// Edit-in-place: the collections list is the homepage, and collections are
// edited from their own pages. Only the two multi-step accessions live on
// dedicated workroom pages — the finding-aid form and the "add crawls" desk.
