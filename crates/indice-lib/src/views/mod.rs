//! Server-rendered HTML views, built with [Maud]. Handlers in `server.rs` gather
//! data and hand it to these functions, which return a [`Markup`] response.
//! Shared page chrome lives in [`layout`]; styling lives in the served
//! `/assets/app.css` stylesheet (no inline `<style>`).
//!
//! [Maud]: https://maud.lambda.xyz/

use maud::{html, Markup, DOCTYPE};

use crate::i18n::{FluentArg, L10n};

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
    /// The archive's name and tagline, the same on every page.
    pub site: std::sync::Arc<Site>,
    /// The interface language for this request. English by default.
    pub l10n: crate::i18n::L10n,
}

/// What the archive calls itself, resolved once at startup from `site:` in
/// `config.yaml`.
#[derive(Debug)]
pub struct Site {
    pub name: String,
    /// The operator's tagline, already rendered from Markdown. `None` means
    /// unset: the page shows the default in the reader's language
    /// ([`Site::tagline`]).
    pub tagline: Option<maud::PreEscaped<String>>,
    /// The operator's analytics snippet (`analytics.head`), for every page's
    /// `<head>`, as they wrote it.
    pub head: Option<maud::PreEscaped<String>>,
    /// The language for a reader whose browser asks for none indice has
    /// (`site.language`).
    pub language: unic_langid::LanguageIdentifier,
}

impl Site {
    /// The line under the archive's name: the operator's own words as written,
    /// never translated, or the default message in the reader's language. Both go
    /// through the Markdown renderer, which escapes raw HTML, so a translation
    /// reaches the page only as escaped text.
    pub fn tagline(&self, l10n: &crate::i18n::L10n) -> maud::PreEscaped<String> {
        match &self.tagline {
            Some(own) => maud::PreEscaped(own.0.clone()),
            None => crate::markdown::render(&l10n.tr("site-tagline-default")),
        }
    }

    /// Fill in the defaults and render the tagline. A blank value counts as
    /// unset, so `name: ""` can't leave the header with no way home.
    pub fn new(cfg: &crate::config::SiteConfig) -> Site {
        let given = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        Site {
            name: given(&cfg.name).unwrap_or_else(|| Site::default().name),
            tagline: given(&cfg.tagline).map(|md| crate::markdown::render(&md)),
            head: None,
            // `site.language` is checked at startup (`SiteConfig::language`),
            // so English here only covers callers that skipped the check.
            language: cfg.language().unwrap_or_else(|_| Site::default().language),
        }
    }
}

impl Default for Site {
    fn default() -> Site {
        Site {
            name: "indice".to_string(),
            tagline: None,
            head: None,
            language: "en".parse().expect("en is a language tag"),
        }
    }
}

/// The shared page shell. Renders one full-bleed app header (`.appbar`) on every
/// page — wordmark + (optional) search — then the page `body` centered in `.wrap`, dressed for `chrome`. `title` is the page's
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
    let l10n = &chrome.l10n;
    let title = match title {
        Some(t) => format!("{t} - {}", site.name),
        None => site.name.clone(),
    };
    html! {
        (DOCTYPE)
        html lang=(chrome.l10n.lang()) dir=(chrome.l10n.dir()) {
            head {
                meta charset="UTF-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                title { (title) }
                // Named explicitly so browsers fetch this public asset rather
                // than guess /favicon.ico, which behind the auth proxy is an
                // unknown path and refused.
                link rel="icon" type="image/svg+xml" href="/assets/favicon.svg";
                link rel="stylesheet" href="/assets/tokens.css";
                link rel="stylesheet" href="/assets/app.css";
                @if let Some(h) = &site.head { (h) }
            }
            body class=[manage.then_some("mode-manage")] {
                header.appbar {
                    a.wordmark href="/" { (site.name) }
                    @if manage { span.chip { (l10n.tr("layout-manage")) } }
                    @if let Some(s) = search {
                        @let placeholder = if s.scope_label.is_empty() {
                            l10n.tr("search-placeholder-all")
                        } else {
                            l10n.tr_args(
                                "search-placeholder-scoped",
                                &[("scope", FluentArg::Str(&s.scope_label))],
                            )
                        };
                        form.search-form action="/search" method="get" {
                            @if !s.scope_query.is_empty() {
                                input type="hidden" name="scope" value=(s.scope_query);
                            }
                            input type="search" name="q" value=(s.query) placeholder=(placeholder);
                            button.btn type="submit" { (l10n.tr("layout-search-button")) }
                        }
                    }
                    @if let Some(u) = &chrome.signed_in {
                        span.who { (l10n.tr("layout-signed-in-as")) " " b { (u) } }
                        // A form, not a link: see `server::auth::logout`.
                        form.logout-form method="post" action="/logout" {
                            button.logout type="submit" { (l10n.tr("layout-logout")) }
                        }
                    } @else if chrome.can_login {
                        // Forward-auth is configured but this request is anonymous:
                        // offer a login. /manage/login is gated, so following it
                        // trips the proxy's login and bounces back to the
                        // current page.
                        a.login href="/manage/login" { (l10n.tr("layout-login")) }
                    }
                }
                main.wrap { (body) }
            }
        }
    }
}

/// The page a signed-in reader gets when they open a workroom page: a 403
/// that explains itself. `what_key` is the message naming what the page
/// needed, e.g. `denied-need-curating`, which completes the sentence.
pub fn access_denied(chrome: &Chrome, what_key: &str) -> Markup {
    let l10n = &chrome.l10n;
    let what = l10n.tr(what_key);
    let body = html! {
        h1.page-title { (l10n.tr("denied-heading")) }
        p { (l10n.tr_args("denied-page-insufficient", &[("what", FluentArg::Str(&what))])) }
        p { a href="/" { (l10n.tr("denied-back")) } }
    };
    layout(Some(&l10n.tr("denied-title")), chrome, None, body)
}

/// The "Search tips" disclosure shown on the homepage and results page. The
/// examples must stay in sync with how `SearchIndex::search` configures the
/// query parser (AND-by-default, default fields, and the `field:` filters).
/// The examples are query syntax and stay as written; each sentence around one
/// is its own message, so no translated text carries markup.
pub fn search_tips(l10n: &L10n) -> Markup {
    let t = |key: &str| l10n.tr(key);
    html! {
        details.tips {
            summary { (t("search-tips-summary")) }
            div.tips-body {
                p {
                    (t("search-tips-intro")) " "
                    strong { (t("search-tips-all-words")) } " - " code { "climate policy" }
                    " " (t("search-tips-all-words-after"))
                }
                ul {
                    li { code { "\"climate policy\"" } " - " (t("search-tips-phrase")) }
                    li { code { "climate OR weather" } " - " (t("search-tips-either")) }
                    li { code { "climate -policy" } " - " (t("search-tips-exclude")) }
                    li { code { "(climate OR weather) risk" } " - " (t("search-tips-group")) }
                    li { code { "title:climate" } " - " (t("search-tips-title")) }
                    li { code { "author:hopper" } " - " (t("search-tips-author")) }
                    li { code { "site:example.com" } " - " (t("search-tips-site")) }
                    li { code { "domain:www.example.com" } " - " (t("search-tips-domain")) }
                    li { code { "collection:demo" } " - " (t("search-tips-collection")) }
                    li {
                        code { "year:2021" } " " (t("search-tips-or")) " " code { "year:[2020 TO 2023]" }
                        " - " (t("search-tips-year"))
                    }
                    li {
                        code { "month:202103" } " " (t("search-tips-or")) " " code { "month:[202101 TO 202106]" }
                        " - " (t("search-tips-month"))
                    }
                    li { code { "modified:2015" } " - " (t("search-tips-modified")) }
                    li { code { "type:pdf" } " - " (t("search-tips-type")) " " code { "type:html" } ")" }
                    li { code { "lang:en" } " - " (t("search-tips-lang")) }
                    li {
                        code { "status:200" } " - " (t("search-tips-status")) " "
                        code { "status:[200 TO 299]" } ")"
                    }
                    li { code { "climate^2 change" } " - " (t("search-tips-boost")) }
                }
                p.tips-note {
                    (t("search-tips-note-intro")) " "
                    code { "domain:" } " " (t("search-tips-note-needs")) " " code { "www.example.com" }
                    (t("search-tips-note-loose")) " " code { "example" } ")."
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
/// `label` is the axis' name, already translated. `count_key` is a plural message
/// (`$count`, `$shown`) naming what is counted, so each language chooses its own
/// forms for "1 result" and "3 results".
fn time_axis(bars: &[TimelineBar], l10n: &L10n, label: &str, count_key: &str) -> Markup {
    html! {
        @if bars.len() >= 2 {
            div.timeline role="group" aria-label=(label) {
                @for b in bars {
                    @let said = format!("{}: {}", b.label, counted(l10n, count_key, b.count));
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

/// `n` as the plural message `key` words it, "3 results". The number is shown
/// in the language's own grouping and selects the plural form.
fn counted(l10n: &L10n, key: &str, n: u64) -> String {
    counted_text(l10n, key, n, &l10n.number(n))
}

/// Like [`counted`], with the displayed number given (`1,000+` for a floor).
fn counted_text(l10n: &L10n, key: &str, n: u64, shown: &str) -> String {
    l10n.tr_args(
        key,
        &[
            ("count", FluentArg::Num(n.try_into().unwrap_or(i64::MAX))),
            ("shown", FluentArg::Str(shown)),
        ],
    )
}

/// One labeled group of browse-links in a detail page's scoped facet overview
/// (e.g. "Top sites" on a collection page), each link a search within that scope.
pub struct FacetSection {
    pub label: String,
    pub links: Vec<BrowseLink>,
}

/// Render a `.browse` block from facet sections (reused on detail pages). Empty
/// sections render nothing.
fn facet_browse(facets: &[FacetSection], l10n: &L10n) -> Markup {
    html! {
        @if !facets.is_empty() {
            div.browse {
                @for f in facets {
                    div.browse-group {
                        h3 { (f.label) }
                        div.browse-links {
                            @for l in &f.links {
                                a.browse-link href=(l.href) {
                                    (l.label) " " span.browse-count { (l10n.number(l.count)) }
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
fn source_badge(remote: bool, l10n: &L10n) -> Markup {
    if remote {
        html! {
            span.source-badge title=(l10n.tr("source-badge-remote-title")) {
                (l10n.tr("source-streamed"))
            }
        }
    } else {
        html! {
            span.source-badge title=(l10n.tr("source-badge-local-title")) {
                (l10n.tr("source-stored"))
            }
        }
    }
}
