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
    /// The address that replays the whole collection (multi-WACZ), when it
    /// has a page to open on.
    pub replay_href: Option<String>,
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
pub fn home(cards: &[CollectionCard], browse: &HomeBrowse, chrome: &Chrome) -> Markup {
    let management = chrome.manage;
    let l10n = &chrome.l10n;
    let body = html! {
        // The archive's name and what it holds, from `site:` in config.yaml.
        h1.home-hero { (chrome.site.name) }
        div.home-tagline { (chrome.site.tagline(&chrome.l10n)) }
        form.search-form.home action="/search" method="get" {
            input type="search" name="q" placeholder=(l10n.tr("home-search-placeholder")) autofocus;
            button.btn type="submit" { (l10n.tr("layout-search-button")) }
        }
        (search_tips(l10n))
        @if !browse.years.is_empty() || !browse.sites.is_empty() {
            div.browse {
                @if !browse.years.is_empty() {
                    div.browse-group {
                        h3 { (l10n.tr("home-browse-year")) }
                        div.browse-links {
                            @for y in &browse.years {
                                a.browse-link href=(y.href) {
                                    (y.label) " " span.browse-count { (l10n.number(y.count)) }
                                }
                            }
                        }
                    }
                }
                @if !browse.sites.is_empty() {
                    div.browse-group {
                        h3 { (l10n.tr("home-top-sites")) }
                        div.browse-links {
                            @for s in &browse.sites {
                                a.browse-link href=(s.href) {
                                    (s.label) " " span.browse-count { (l10n.number(s.count)) }
                                }
                            }
                        }
                    }
                }
            }
        }
        div.section-head {
            h2 { (l10n.tr("home-collections")) }
            // Edit-in-place: the "new collection" action lives right on the list
            // it affects, only in workroom mode.
            @if management {
                a.btn.edit href="/manage/collections/new" { (l10n.tr("home-new-collection")) }
            }
        }
        @if cards.is_empty() {
            @if management {
                div.empty-cta {
                    p { (l10n.tr("home-empty-cta")) }
                    a.btn.edit href="/manage/add" { (l10n.tr("home-add-crawl")) }
                }
            } @else {
                p.muted {
                    (l10n.tr("home-no-collections")) " "
                    code { "indice index archive/*.wacz" } "."
                }
            }
        }
        div.cards {
            @for c in cards {
                div.card {
                    a.card-thumb href=(format!("/collection/{}", c.id)) {
                        (thumb_area(c.thumb.as_deref()))
                    }
                    // Per-card edit affordance (hover-revealed) — workroom only.
                    @if management {
                        a.card-edit href=(format!("/manage/edit/{}", c.id)) { (l10n.tr("action-edit")) }
                    }
                    div.card-body {
                        div.card-header {
                            span.card-title-wrap {
                                a.card-title href=(format!("/collection/{}", c.id)) { (c.name) }
                            }
                            span.status.muted {
                                (counted(l10n, "home-card-crawls", c.count as u64))
                            }
                        }
                        @if c.has_local || c.has_remote {
                            p.card-source {
                                @match (c.has_local, c.has_remote) {
                                    (true, true) => (l10n.tr("source-stored-and-streamed")),
                                    (false, true) => (l10n.tr("source-streamed")),
                                    _ => (l10n.tr("source-stored")),
                                }
                            }
                        }
                        @if let Some(d) = &c.description {
                            p.desc { (d) }
                        }
                        @if c.date_range.is_some() || c.replay_href.is_some() {
                            div.card-footer {
                                @if let Some(r) = &c.date_range {
                                    div.prov { (r) }
                                }
                                @if let Some(href) = &c.replay_href {
                                    a.btn href=(href) { (l10n.tr("action-replay")) }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    // No header search on the homepage — the hero below carries it.
    layout(None, chrome, None, body)
}
