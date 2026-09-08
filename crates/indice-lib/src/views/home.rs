//! The homepage: the collection cards, the browse-by-year/site entry
//! points, and the empty state.

use maud::{html, Markup};

use super::*;

/// A collection as shown on a homepage card.
pub struct CollectionCard {
    pub id: String,
    pub name: String,
    pub count: usize,
    pub description: Option<String>,
    pub date_range: Option<String>,
    /// Viewer URL that replays the whole collection (multi-WACZ).
    pub replay_href: String,
    /// `/thumb/{id}` for a representative member crawl, if any has one.
    pub thumb: Option<String>,
    /// Whether the collection has any locally-stored / any remote member — both
    /// true means a mixed collection (show both pills).
    pub has_local: bool,
    pub has_remote: bool,
}

/// A single browse entry point on the homepage: a label, its count, and the
/// search link it leads to (e.g. a year or a site).
pub struct BrowseLink {
    pub label: String,
    pub count: u64,
    pub href: String,
}

/// Archive-wide browse entry points shown on the homepage.
pub struct HomeBrowse {
    pub years: Vec<BrowseLink>,
    pub sites: Vec<BrowseLink>,
}

/// The homepage: search box, tips, browse-by entry points, and a card per
/// collection.
pub fn home(
    cards: &[CollectionCard],
    browse: &HomeBrowse,
    management: bool,
    signed_in: Option<&str>,
    can_login: bool,
) -> Markup {
    let body = html! {
        // The brand "indice" lives in the header now; the hero leads with what
        // the tool does + the search, rather than repeating the name.
        h1.home-hero { "Web archive search and replay" }
        form.search-form.home action="/search" method="get" {
            input type="search" name="q" placeholder="Search archived pages…" autofocus;
            button type="submit" { "Search" }
        }
        (search_tips())
        @if !browse.years.is_empty() || !browse.sites.is_empty() {
            div.browse {
                @if !browse.years.is_empty() {
                    div.browse-group {
                        h3 { "Browse by year" }
                        div.browse-links {
                            @for y in &browse.years {
                                a.browse-link href=(y.href) {
                                    (y.label) " " span.browse-count { (y.count) }
                                }
                            }
                        }
                    }
                }
                @if !browse.sites.is_empty() {
                    div.browse-group {
                        h3 { "Top sites" }
                        div.browse-links {
                            @for s in &browse.sites {
                                a.browse-link href=(s.href) {
                                    (s.label) " " span.browse-count { (s.count) }
                                }
                            }
                        }
                    }
                }
            }
        }
        div.section-head {
            h2 { "Collections" }
            // Edit-in-place: the "new collection" action lives right on the list
            // it affects, only in workroom mode.
            @if management {
                a.btn href="/manage/collections/new" { "+ New collection" }
            }
        }
        @if cards.is_empty() {
            @if management {
                div.empty-cta {
                    p { "No collections yet — create one, or add an archive to start." }
                    a.btn href="/manage/add" { "Add your first archive →" }
                }
            } @else {
                p.muted {
                    "No collections indexed yet. Run "
                    code { "indice index archive/*.wacz" } " to get started."
                }
            }
        }
        div.cards {
            @for c in cards {
                div.card {
                    a.card-thumb href=(format!("/collection/{}", c.id)) {
                        (thumb_area(c.thumb.as_deref(), &c.name))
                    }
                    // Per-card edit affordance (hover-revealed) — workroom only.
                    @if management {
                        a.card-edit href=(format!("/manage/edit/{}", c.id)) { "Edit" }
                    }
                    div.card-body {
                        div.card-header {
                            span.card-title-wrap {
                                @if c.has_local { (source_badge(false)) }
                                @if c.has_remote { (source_badge(true)) }
                                a.card-title href=(format!("/collection/{}", c.id)) { (c.name) }
                            }
                            span.status.muted {
                                (c.count) " crawl" @if c.count != 1 { "s" }
                            }
                        }
                        @if let Some(d) = &c.description {
                            p.desc { (d) }
                        }
                        @if c.date_range.is_some() || c.count > 0 {
                            div.card-footer {
                                @if let Some(r) = &c.date_range {
                                    div.prov { (r) }
                                }
                                @if c.count > 0 {
                                    a.replay-btn href=(c.replay_href) { "Replay →" }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    // No header search on the homepage — the hero below carries it.
    layout("indice", management, signed_in, can_login, None, body)
}
