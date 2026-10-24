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

/// Who is looking at a page, as far as the shared header needs to know. Every
/// view takes one and hands it to [`layout`].
#[derive(Clone, Debug, Default)]
pub struct Chrome {
    /// The caller may change the archive: the page is in "workroom" mode, with
    /// the `.mode-manage` accent, the Manage chip and the edit-in-place
    /// affordances.
    pub manage: bool,
    /// The signed-in user's display name, behind an authenticating proxy.
    pub signed_in: Option<String>,
    /// Forward-auth is configured but this request is anonymous: show a
    /// "Log in" link in place of the signed-in name.
    pub can_login: bool,
    /// The archive's name, tagline and banner, the same on every page.
    pub site: std::sync::Arc<Site>,
}

/// What the archive calls itself, resolved once at startup from `site:` in
/// `config.yaml`.
#[derive(Debug)]
pub struct Site {
    pub name: String,
    pub tagline: String,
    /// The banner, already rendered from Markdown.
    pub banner: Option<maud::PreEscaped<String>>,
}

impl Site {
    /// Fill in the defaults and render the banner. A blank value counts as
    /// unset, so `name: ""` can't leave the header with no way home.
    pub fn new(cfg: &crate::config::SiteConfig) -> Site {
        let given = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let default = Site::default();
        Site {
            name: given(&cfg.name).unwrap_or(default.name),
            tagline: given(&cfg.tagline).unwrap_or(default.tagline),
            banner: given(&cfg.banner).map(|md| crate::markdown::render(&md)),
        }
    }
}

impl Default for Site {
    fn default() -> Site {
        Site {
            name: "indice".to_string(),
            tagline: "Web archive search and replay".to_string(),
            banner: None,
        }
    }
}

/// The shared page shell. Renders one full-bleed app header (`.appbar`) on every
/// page — wordmark + (optional) search — then the site banner, if any, then the
/// page `body` centered in `.wrap`, dressed for `chrome`. `title` is the page's
/// own title, which gets the site name appended (`None` is the homepage, titled
/// by the name alone). `search` is the header search box (`None` omits it — the
/// homepage, whose hero carries the search instead).
pub fn layout(
    title: Option<&str>,
    chrome: &Chrome,
    search: Option<&SearchBox>,
    body: Markup,
) -> Markup {
    let manage = chrome.manage;
    let site = &chrome.site;
    let title = match title {
        Some(t) => format!("{t} - {}", site.name),
        None => site.name.clone(),
    };
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="UTF-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                title { (title) }
                link rel="stylesheet" href="/assets/tokens.css";
                link rel="stylesheet" href="/assets/app.css";
            }
            body class=[manage.then_some("mode-manage")] {
                header.appbar {
                    a.wordmark href="/" { (site.name) }
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
                            button.btn type="submit" { "Search" }
                        }
                    }
                    @if let Some(u) = &chrome.signed_in {
                        span.who { "signed in as " b { (u) } }
                        // A form, not a link: see `server::auth::logout`.
                        form.logout-form method="post" action="/logout" {
                            button.logout type="submit" { "Log out" }
                        }
                    } @else if chrome.can_login {
                        // Forward-auth is configured but this request is anonymous:
                        // offer a login. /manage/login is gated, so following it
                        // trips the proxy's login and bounces back to the
                        // current page.
                        a.login href="/manage/login" { "Log in" }
                    }
                }
                @if let Some(b) = &site.banner {
                    div.site-banner { (b) }
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

/// A card's representative image: the cached thumbnail, or a plain panel the
/// same size, so a card without one still lines up with its neighbours. Not a
/// coloured placeholder: a colour that means nothing reads as if it did.
fn thumb_area(thumb: Option<&str>) -> Markup {
    html! {
        @match thumb {
            Some(src) => div.thumb { img src=(src) alt="" loading="lazy"; },
            None => div.thumb.blank {},
        }
    }
}

/// One month on a time axis: the results, or the pages crawled, in that month.
/// An axis has a bar for every month in its span, empty ones included, so the
/// gaps in an archive show as gaps.
pub struct TimelineBar {
    /// The month in words, "May 2021".
    pub label: String,
    /// The year, on the first month shown of each year: where the axis marks it.
    pub year: Option<String>,
    pub count: u64,
    /// Height in tenths of the busiest month, 0 to 10; at least 1 when the
    /// month has anything, so a small month is never invisible.
    pub step: u8,
    /// Where clicking the month goes (a filter toggle); `None` for an empty month.
    pub href: Option<String>,
    /// Whether the page is already filtered to this month.
    pub active: bool,
}

/// A time axis: one bar per month, proportional across the span, with years
/// marked. Each bar says its month and count to a screen reader, and shows
/// them on hover and on keyboard focus. Nothing renders for fewer than two
/// months, where an axis says nothing a sentence wouldn't.
/// `unit` names what is counted, singular and plural.
fn time_axis(bars: &[TimelineBar], label: &str, unit: (&str, &str)) -> Markup {
    html! {
        @if bars.len() >= 2 {
            div.timeline role="group" aria-label=(label) {
                @for b in bars {
                    @let said = format!(
                        "{}: {} {}",
                        b.label,
                        b.count,
                        if b.count == 1 { unit.0 } else { unit.1 }
                    );
                    @match &b.href {
                        Some(h) => {
                            a.tl-bar.active[b.active].tl-year[b.year.is_some()] href=(h) aria-label=(said) aria-current=[b.active.then_some("true")] {
                                span class=(format!("tl-fill h{}", b.step)) {}
                                span.tl-tip aria-hidden="true" { (said) }
                                @if let Some(y) = &b.year { span.tl-yr aria-hidden="true" { (y) } }
                            }
                        }
                        None => {
                            span.tl-bar.empty.tl-year[b.year.is_some()] aria-hidden="true" {
                                @if let Some(y) = &b.year { span.tl-yr { (y) } }
                            }
                        }
                    }
                }
            }
        }
    }
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

/// Where a crawl's WACZ lives, in words: stored in this archive's own
/// folder, or streamed from a remote host at replay time.
fn source_badge(remote: bool) -> Markup {
    if remote {
        html! {
            span.source-badge
                title="Hosted remotely: indice streams this at replay time and keeps no copy" {
                "Streamed"
            }
        }
    } else {
        html! {
            span.source-badge title="Stored in this archive's own folder" { "Stored here" }
        }
    }
}
