//! Server-rendered HTML views, built with [Maud]. Handlers in `server.rs` gather
//! data and hand it to these functions, which return a [`Markup`] response.
//! Shared page chrome lives in [`layout`]; styling lives in the served
//! `/assets/app.css` stylesheet (no inline `<style>`).
//!
//! [Maud]: https://maud.lambda.xyz/

use maud::{html, Markup, DOCTYPE};

mod collection;
mod crawl;
mod home;
mod manage;
mod search;

pub use collection::*;
pub use crawl::*;
pub use home::*;
pub use manage::*;
pub use search::*;

/// Header search-box configuration. Default (all empty) is a global search box;
/// `scope_query`/`scope_label` scope it to the page's context (e.g. the
/// collection being viewed).
#[derive(Default)]
pub struct SearchBox {
    /// Prefill for the box (the current query, on the results page).
    pub query: String,
    /// A `field:value` token ANDed into the query server-side and carried as a
    /// hidden field, e.g. `collection:ukraine-…` — scopes the search.
    pub scope_query: String,
    /// Human label for the scoped placeholder, e.g. `Ukraine Cultural Heritage`.
    pub scope_label: String,
}

/// The shared page shell. Renders one full-bleed app header (`.appbar`) on every
/// page — wordmark + (optional) search — then the page `body` centered in
/// `.wrap`. `manage` puts the page in "workroom" mode: the `.mode-manage` accent
/// flip plus the header's clay treatment, Manage chip, and signed-in name.
/// `can_login` (forward-auth configured but this request anonymous) shows a
/// "Log in" link in place of the signed-in name. `search` is the header search
/// box (`None` omits it — the homepage, whose hero carries the search instead).
pub fn layout(
    title: &str,
    manage: bool,
    signed_in: Option<&str>,
    can_login: bool,
    search: Option<&SearchBox>,
    body: Markup,
) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="UTF-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                title { (title) }
                link rel="stylesheet" href="/assets/app.css";
            }
            body class=[manage.then_some("mode-manage")] {
                header.appbar {
                    a.wordmark href="/" { "indice" }
                    @if manage { span.chip { "Manage" } }
                    @if let Some(s) = search {
                        @let placeholder = if s.scope_label.is_empty() {
                            "Search all collections…".to_string()
                        } else {
                            format!("Search {}…", s.scope_label)
                        };
                        form.search-form action="/search" method="get" {
                            @if !s.scope_query.is_empty() {
                                input type="hidden" name="scope" value=(s.scope_query);
                            }
                            input type="search" name="q" value=(s.query) placeholder=(placeholder);
                            button type="submit" { "Search" }
                        }
                    }
                    @if let Some(u) = signed_in {
                        span.who { "signed in as " b { (u) } }
                        a.logout href="/logout" { "Log out" }
                    } @else if can_login {
                        // Forward-auth is configured but this request is anonymous:
                        // offer a login. /manage/login is gated, so following it
                        // trips the proxy's login (a Basic-auth prompt, or an SSO
                        // redirect) and bounces back to the current page.
                        a.login href="/manage/login" { "Log in" }
                    }
                }
                main.wrap { (body) }
            }
        }
    }
}

/// The "Search tips" disclosure shown on the homepage and results page. The
/// examples must stay in sync with how `SearchIndex::search` configures the
/// query parser (AND-by-default, default fields, and the `field:` filters).
pub fn search_tips() -> Markup {
    html! {
        details.tips {
            summary { "Search tips" }
            div.tips-body {
                p {
                    "Type words to search page titles, headings, body text, descriptions, "
                    "keywords, author, and URLs. "
                    strong { "All words must match" } " - " code { "climate policy" }
                    " finds pages containing both."
                }
                ul {
                    li { code { "\"climate policy\"" } " - an exact phrase (use quotes)" }
                    li { code { "climate OR weather" } " - either word" }
                    li { code { "climate -policy" } " - has \"climate\", excludes \"policy\"" }
                    li { code { "(climate OR weather) risk" } " - group with parentheses" }
                    li { code { "title:climate" } " - match only in the page title" }
                    li { code { "author:hopper" } " - match the page author" }
                    li { code { "site:example.com" } " - a whole site, across subdomains" }
                    li { code { "domain:www.example.com" } " - only that exact host" }
                    li { code { "collection:demo" } " - only pages in that collection" }
                    li { code { "year:2021" } " or " code { "year:[2020 TO 2023]" } " - filter by crawl year" }
                    li { code { "month:202103" } " or " code { "month:[202101 TO 202106]" } " - filter by crawl month" }
                    li { code { "modified:2015" } " - filter by Last-Modified year" }
                    li { code { "type:pdf" } " - only PDFs (or " code { "type:html" } ")" }
                    li { code { "lang:en" } " - only pages in that language" }
                    li { code { "status:200" } " - filter by HTTP status (or " code { "status:[200 TO 299]" } ")" }
                    li { code { "climate^2 change" } " - rank \"climate\" matches higher" }
                }
                p.tips-note {
                    "Searches are case-insensitive. Title matches rank above body matches. "
                    code { "domain:" } " needs the exact host (e.g. " code { "www.example.com" }
                    "); to match host words loosely, just type them (e.g. " code { "example" } ")."
                }
            }
        }
    }
}

/// A card/detail representative image. Shows the cached thumbnail if present,
/// otherwise a CSS placeholder tinted by a hash of `seed` (so cards vary a bit).
fn thumb_area(thumb: Option<&str>, seed: &str) -> Markup {
    html! {
        @match thumb {
            Some(src) => div.thumb { img src=(src) alt="" loading="lazy"; },
            None => div.thumb.placeholder style=(placeholder_style(seed)) {},
        }
    }
}

/// A deterministic gradient for a placeholder, its hue derived from `seed` so
/// each collection/crawl gets a stable, distinct tint.
fn placeholder_style(seed: &str) -> String {
    let hue = seed
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32))
        % 360;
    format!("background:linear-gradient(135deg,hsl({hue},45%,72%),hsl({hue},38%,55%))")
}

/// One labeled group of browse-links in a detail page's scoped facet overview
/// (e.g. "Top sites" on a collection page), each link a search within that scope.
pub struct FacetSection {
    pub label: String,
    pub links: Vec<BrowseLink>,
}

/// Render a `.browse` block from facet sections (reused on detail pages). Empty
/// sections render nothing.
fn facet_browse(facets: &[FacetSection]) -> Markup {
    html! {
        @if !facets.is_empty() {
            div.browse {
                @for f in facets {
                    div.browse-group {
                        h3 { (f.label) }
                        div.browse-links {
                            @for l in &f.links {
                                a.browse-link href=(l.href) {
                                    (l.label) " " span.browse-count { (l.count) }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A single `<th>/<td>` row in a metadata table. `mono` renders the value in a
/// monospace cell (for URLs, user-agents, hashes).
pub struct MetaRow {
    pub label: String,
    pub value: String,
    pub mono: bool,
}

impl MetaRow {
    pub fn new(label: &str, value: impl Into<String>) -> Self {
        MetaRow {
            label: label.to_string(),
            value: value.into(),
            mono: false,
        }
    }
    pub fn mono(label: &str, value: impl Into<String>) -> Self {
        MetaRow {
            label: label.to_string(),
            value: value.into(),
            mono: true,
        }
    }
}

fn meta_table(rows: &[MetaRow]) -> Markup {
    html! {
        table.meta {
            @for r in rows {
                tr {
                    th { (r.label) }
                    @if r.mono { td.mono { (r.value) } } @else { td { (r.value) } }
                }
            }
        }
    }
}

/// A pill labelling where a crawl's WACZ lives: `💾 Local` (stored in this
/// home's `archive/`) or `🌐 Remote` (fetched from a remote host at replay time).
fn source_badge(remote: bool) -> Markup {
    // Icon only, but with role="img" + aria-label so a screen reader announces
    // "Local"/"Remote" (not the emoji's Unicode name); `title` is the mouse
    // tooltip. `title` alone would not be accessible.
    if remote {
        html! {
            span.source-badge.remote role="img" aria-label="Remote"
                title="Hosted remotely — indice streams this at replay time and doesn't keep a local copy" {
                "🌐"
            }
        }
    } else {
        html! {
            span.source-badge.local role="img" aria-label="Local"
                title="Stored locally in this home's archive folder" {
                "💾"
            }
        }
    }
}
