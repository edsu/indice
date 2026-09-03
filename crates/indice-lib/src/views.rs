//! Server-rendered HTML views, built with [Maud]. Handlers in `server.rs` gather
//! data and hand it to these functions, which return a [`Markup`] response.
//! Shared page chrome lives in [`layout`]; styling lives in the served
//! `/assets/app.css` stylesheet (no inline `<style>`).
//!
//! [Maud]: https://maud.lambda.xyz/

use maud::{html, Markup, PreEscaped, DOCTYPE};

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

// ── Search results ─────────────────────────────────────────────────────────

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

/// One row of the search results table. The handler computes the replay `href`,
/// display strings, and the (pre-escaped) snippet HTML; the view just lays it out.
pub struct SearchResultRow {
    pub href: String,
    pub title: String,
    pub is_collection: bool,
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
// The trailing (management, signed_in, can_login) are the shared header-chrome
// flags every page threads; bundling them isn't worth a struct here.
#[allow(clippy::too_many_arguments)]
pub fn search_results(
    query: &str,
    nav: &PageNav,
    sidebar: &FacetSidebar,
    timeline: &[TimelineBar],
    rows: &[SearchResultRow],
    management: bool,
    signed_in: Option<&str>,
    can_login: bool,
) -> Markup {
    // Preserve the query when linking to another page.
    let page_href = |p: usize| format!("/search?q={}&page={}", nav.query_encoded, p);
    let body = html! {
        (search_tips())
        div.count {
            @if nav.total_hits == 0 {
                "No results for " em { (query) }
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
                @for b in timeline {
                    a.tl-bar.active[b.active] href=(b.href) title=(format!("{}: {} result{}", b.label, b.count, if b.count == 1 { "" } else { "s" })) {
                        span.tl-fill style=(format!("height:{}%", b.pct.max(3))) {}
                        span.tl-label { (b.label) }
                    }
                }
            }
        }
        div.results-layout {
            @if !sidebar.groups.is_empty() {
                aside.facets {
                    @for g in &sidebar.groups {
                        div.facet-group {
                            h3 { (g.label) }
                            ul {
                                @for it in &g.items {
                                    li.facet-item.active[it.active] {
                                        a href=(it.href) {
                                            span.facet-value { (it.value) }
                                            span.facet-count { (it.count) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
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
                                        a.result-replay href=(r.href) { "Replay →" }
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
        }
    };
    // Header search prefilled with the current query (the results box).
    let search = SearchBox {
        query: query.to_string(),
        ..Default::default()
    };
    layout(
        &format!("{query} - indice"),
        management,
        signed_in,
        can_login,
        Some(&search),
        body,
    )
}

// ── Shared metadata / provenance rows ────────────────────────────────────────

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

// ── Collection detail ────────────────────────────────────────────────────────

/// A member crawl (WACZ) as shown in a collection's grid.
pub struct MemberItem {
    pub id: String,
    pub name: String,
    pub present: bool,
    /// Whether this crawl is hosted remotely (streamed) rather than local.
    pub remote: bool,
    /// One-line provenance summary (plain text), if any is known.
    pub provenance: Option<String>,
    /// `/thumb/{id}` for this crawl's representative image, if it has one.
    pub thumb: Option<String>,
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

/// Everything the collection detail page renders. Read like a finding aid: the
/// curatorial front-matter (`narrative` prose + the structured `creator`/`dates`
/// /`rights`/`subjects`) leads, visually separated from the derived/technical
/// aggregates in `meta`. The handler resolves the data; the view lays it out.
pub struct CollectionPage {
    pub name: String,
    /// Short abstract/caption shown under the title.
    pub description: Option<String>,
    /// The rendered (safe-HTML) Markdown narrative — Scope & Content / Custodial
    /// history / Appraisal.
    pub narrative: Option<PreEscaped<String>>,
    pub creator: Option<String>,
    pub dates: Option<String>,
    pub rights: Option<String>,
    pub subjects: Vec<String>,
    /// Derived/technical aggregates (Crawls / Size / Software / Capture dates /
    /// Created).
    pub meta: Vec<MetaRow>,
    pub facets: Vec<FacetSection>,
    pub members: Vec<MemberItem>,
    /// Viewer URL that replays the whole collection (multi-WACZ).
    pub replay_href: String,
    /// Collection id (slug) — for the edit / add-crawls links.
    pub id: String,
    /// Whether management mode is on — gates the edit-in-place affordances.
    pub management: bool,
    /// Signed-in user (forward-auth), shown in the workroom strip.
    pub signed_in: Option<String>,
    /// Forward-auth configured but this request anonymous — show a "Log in" link.
    pub can_login: bool,
}

impl CollectionPage {
    /// Whether any curatorial finding-aid field is populated (drives the About
    /// block vs. the empty-state nudge).
    fn has_curatorial(&self) -> bool {
        self.narrative.is_some()
            || self.creator.is_some()
            || self.dates.is_some()
            || self.rights.is_some()
            || !self.subjects.is_empty()
    }

    /// The DACS single-level *minimum* curatorial elements that are still empty
    /// — Scope & Content (narrative), Name of Creator, Conditions Governing
    /// Access/Use (rights). Drives the "still needed" prompt when ingest seeded
    /// some fields but left these gaps (the fields no source fills reliably).
    fn missing_minimum(&self) -> Vec<&'static str> {
        let mut m = Vec::new();
        if self.narrative.is_none() {
            m.push("Scope & Content");
        }
        if self.creator.is_none() {
            m.push("Creator");
        }
        if self.rights.is_none() {
            m.push("Access & Use");
        }
        m
    }

    /// The curatorial (finding-aid) metadata table, with DACS-labelled rows.
    fn curatorial_rows(&self) -> Vec<MetaRow> {
        let mut rows = Vec::new();
        if let Some(v) = &self.creator {
            rows.push(MetaRow::new("Creator", v.clone()));
        }
        if let Some(v) = &self.dates {
            rows.push(MetaRow::new("Dates", v.clone()));
        }
        if let Some(v) = &self.rights {
            rows.push(MetaRow::new("Rights", v.clone()));
        }
        if !self.subjects.is_empty() {
            rows.push(MetaRow::new("Subjects", self.subjects.join(", ")));
        }
        rows
    }
}

/// The collection detail page: a finding-aid front-matter (narrative + curatorial
/// table) above the derived aggregates and facets, then a grid of the member
/// crawls, each with its own representative image (a collection spans multiple
/// crawls of multiple sites, so the grid conveys that breadth better than one
/// hero image would).
pub fn collection(p: &CollectionPage) -> Markup {
    let curatorial = p.curatorial_rows();
    let missing = p.missing_minimum();
    let body = html! {
        h1.page-title { (p.name) }
        @if let Some(d) = &p.description { p.desc { (d) } }
        // One action row: Replay plus, in workroom mode, the edit-in-place
        // curation actions — all the same button treatment.
        div.actions {
            @if !p.members.is_empty() {
                a.btn href=(p.replay_href) { "Replay collection →" }
            }
            @if p.management {
                a.btn href=(format!("/manage/edit/{}", p.id)) { "Edit collection" }
                a.btn href=(format!("/manage/add?collection={}", p.id)) { "+ Add crawls" }
            }
        }
        @if p.management {
            details.danger-zone {
                summary { "Delete this collection" }
                form.confirm-delete method="post" action=(format!("/api/collections/{}/delete", p.id)) {
                    @if p.members.is_empty() {
                        p.muted { "Removes this empty collection's finding aid. This can't be undone." }
                    } @else {
                        p.muted {
                            "This collection has " (p.members.len()) " crawl(s). Deleting the "
                            "grouping alone is refused; tick the box to delete its crawls too "
                            "(their pages, WACZ files, and thumbnails). This can't be undone."
                        }
                        label.confirm-with-crawls {
                            input type="checkbox" name="with_crawls" value="true";
                            span { "also delete all " (p.members.len()) " member crawl(s)" }
                        }
                    }
                    button.btn.danger type="submit" { "Delete permanently" }
                }
            }
        }

        section.about {
            h2 { "About this collection" }
            @if p.has_curatorial() {
                @if let Some(n) = &p.narrative { div.narrative { (n) } }
                @if !curatorial.is_empty() { (meta_table(&curatorial)) }
                // Even partly-filled, name the DACS-minimum elements still
                // missing — the fields ingest can't supply reliably (a real
                // creator, the scope rationale, use conditions).
                @if !missing.is_empty() {
                    p.muted.nudge {
                        "Still needed: " (missing.join(", "))
                        " (the finding-aid minimum). "
                        @if p.management {
                            a href=(format!("/manage/edit/{}", p.id)) { "Edit this collection" }
                            " to add them."
                        } @else {
                            "Add with "
                            code { "indice collection set \"" (p.name) "\" …" }
                            " or edit "
                            code { "collections/" (p.id) "/README.md" }
                            "."
                        }
                    }
                }
            } @else {
                // Empty-state nudge: name the DACS single-level minimum
                // curatorial elements that are missing, with archival authority.
                p.muted.nudge {
                    "No finding-aid description yet. Add the essentials a reader needs — "
                    "who gathered it (Creator), why it was archived (Scope & Content), and "
                    "who may use it (Access). "
                    @if p.management {
                        a href=(format!("/manage/edit/{}", p.id)) { "Edit this collection" }
                        " to describe it."
                    } @else {
                        "Add with "
                        code { "indice collection set \"" (p.name) "\" --creator \"…\"" }
                        " — or edit "
                        code { "collections/" (p.id) "/README.md" }
                        "."
                    }
                }
            }
        }

        @if !p.meta.is_empty() { (meta_table(&p.meta)) }
        (facet_browse(&p.facets))
        h2 { "Crawls" }
        @if p.members.is_empty() {
            p.muted { "No crawls in this collection." }
        } @else {
            div.cards {
                @for m in &p.members {
                    div.card {
                        a.card-thumb href=(format!("/crawl/{}", m.id)) {
                            (thumb_area(m.thumb.as_deref(), &m.name))
                        }
                        div.card-header {
                            span.card-title-wrap {
                                (source_badge(m.remote))
                                a.card-title href=(format!("/crawl/{}", m.id)) { (m.name) }
                            }
                            @if m.present {
                                span.status.ok { "✓" }
                            } @else {
                                span.status.missing { "✗" }
                            }
                        }
                        @if let Some(pr) = &m.provenance { div.prov { (pr) } }
                    }
                }
            }
        }
    };
    // Header search scoped to this collection (broaden via the results chip).
    let search = SearchBox {
        scope_query: format!("collection:{}", p.id),
        scope_label: p.name.clone(),
        ..Default::default()
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

// ── Crawl detail ─────────────────────────────────────────────────────────────

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

/// Values for the create/edit collection form. Empty strings render as blank
/// fields. `editing` locks the name (its slug is the collection's identity) and
/// switches the labels from "New/Create" to "Edit/Save".
#[derive(Default)]
pub struct CollectionFormData {
    /// Collection id (slug). Empty for a new collection; set when editing so the
    /// Cancel link goes back to the real collection page.
    pub id: String,
    pub name: String,
    pub description: String,
    pub curator: String,
    pub creator: String,
    pub dates: String,
    pub rights: String,
    pub subjects: String,
    pub narrative: String,
    pub editing: bool,
}

/// The finding-aid form (`/manage/collections/new` and `/manage/edit/{id}`):
/// create or edit a collection's curatorial description. POSTs to
/// `/api/collections`.
pub fn collection_form(form: &CollectionFormData, signed_in: Option<&str>) -> Markup {
    let back = if form.editing {
        format!("/collection/{}", form.id)
    } else {
        "/".to_string()
    };
    let body = html! {
        div.crumbs {
            a href="/" { "Home" }
            span.sep { " / " }
            @if form.editing { b { (form.name) } } @else { b { "New collection" } }
        }
        span.eyebrow { "Finding aid" }
        h1.page-title { @if form.editing { "Edit collection" } @else { "New collection" } }
        @if form.editing {
            p.muted { "The name is fixed (it's the collection's identity); edit the description below." }
        }
        form.manage-form method="post" action="/api/collections" {
            label {
                span { "Name" }
                input type="text" name="name" required value=(form.name) readonly[form.editing];
            }
            label {
                span { "Description " span.hint { "· a one-line summary, shown on cards and under the title" } }
                input type="text" name="description" value=(form.description);
            }
            div.grid-2 {
                label { span { "Creator" } input type="text" name="creator" value=(form.creator); }
                label { span { "Dates" } input type="text" name="dates" value=(form.dates); }
                label { span { "Curator" } input type="text" name="curator" value=(form.curator); }
                label { span { "Rights" } input type="text" name="rights" value=(form.rights); }
            }
            label {
                span { "Subjects " span.hint { "· comma-separated" } }
                input type="text" name="subjects" value=(form.subjects);
            }
            label {
                span { "Narrative " span.hint { "· the full finding-aid prose (Markdown): Scope & Content, custodial history, appraisal" } }
                textarea name="narrative" rows="8" { (form.narrative) }
            }
            div.form-actions {
                button.btn type="submit" { @if form.editing { "Save changes" } @else { "Create collection" } }
                a.cancel href=(back) { "Cancel" }
            }
        }
    };
    // When editing, scope the header search to this collection (as its page does).
    let search = if form.editing && !form.id.is_empty() {
        SearchBox {
            scope_query: format!("collection:{}", form.id),
            scope_label: form.name.clone(),
            ..Default::default()
        }
    } else {
        SearchBox::default()
    };
    layout(
        "Manage - indice",
        true,
        signed_in,
        false,
        Some(&search),
        body,
    )
}

/// Progressive-enhancement script for the accession desk: source tabs, the
/// add-archive submit (upload / path-URL / Browsertrix / Archive-It), the
/// Browsertrix and Archive-It browse wizards, and shared SSE progress. Small and
/// dependency-free.
const ADD_ARCHIVE_JS: &str = r#"
const f = document.getElementById('add-archive-form');
const out = document.getElementById('add-progress');

// Stream a job's SSE progress into the status area (shared by every source).
function stream(job, collectionName) {
  const es = new EventSource('/api/archives/' + job + '/events');
  const lines = [];
  const show = (m) => { lines.push(m); out.textContent = lines.join('\n'); };
  es.addEventListener('begin', (ev) => show('reading ' + JSON.parse(ev.data).label));
  es.addEventListener('phase', (ev) => show('… ' + JSON.parse(ev.data).phase));
  es.addEventListener('total', (ev) => show('records to index: ' + JSON.parse(ev.data).total));
  es.addEventListener('wacz_indexed', (ev) => {
    const d = JSON.parse(ev.data);
    show('indexed ' + d.label + ' (' + d.pages + ' pages)');
  });
  es.addEventListener('done', (ev) => {
    show('Done ✓');
    let d = {};
    try { d = JSON.parse(ev.data) || {}; } catch (e) {}
    const crawls = Array.isArray(d.crawls) ? d.crawls : [];
    const link = (href, text) => {
      const a = document.createElement('a'); a.href = href; a.textContent = text; return a;
    };
    const para = (child) => { const p = document.createElement('p'); p.appendChild(child); return p; };
    const collLink = () => link('/collection/' + encodeURIComponent(d.collection),
      'View ' + (collectionName ? '“' + collectionName + '”' : 'collection') + ' →');
    const wrap = document.createElement('div'); wrap.className = 'add-done-link';
    if (crawls.length === 1) {
      wrap.appendChild(para(link('/crawl/' + encodeURIComponent(crawls[0].id), 'View crawl →')));
    } else if (crawls.length > 1) {
      const head = document.createElement('p'); head.textContent = 'Added ' + crawls.length + ' crawls:';
      wrap.appendChild(head);
      const ul = document.createElement('ul'); ul.className = 'add-done-crawls';
      for (const c of crawls) {
        const li = document.createElement('li');
        li.appendChild(link('/crawl/' + encodeURIComponent(c.id), c.name || c.id));
        ul.appendChild(li);
      }
      wrap.appendChild(ul);
      if (d.collection) wrap.appendChild(para(collLink()));
    } else if (d.collection) {
      wrap.appendChild(para(collLink()));
    }
    if (wrap.childNodes.length) out.appendChild(wrap);
    es.close();
  });
  es.addEventListener('error', (ev) => { if (ev.data) show('Error: ' + JSON.parse(ev.data).message); es.close(); });
}

// Source tabs.
document.querySelectorAll('.src-tab').forEach(tab => tab.addEventListener('click', (e) => {
  e.preventDefault();
  document.querySelectorAll('.src-tab').forEach(t => t.setAttribute('aria-selected', t === tab));
  document.querySelectorAll('.src-panel').forEach(p => p.classList.toggle('active', p.id === 'src-' + tab.dataset.src));
  // Opening an import tab reaches out to the configured instance on its own.
  if (tab.dataset.src === 'bx' && !bxConnected) bxConnect();
  if (tab.dataset.src === 'ait' && !aitConnected) aitConnect();
}));

// ── Browsertrix browse (orgs → collections → crawls), using server creds. ──
async function bxGet(path) {
  const r = await fetch(path);
  if (!r.ok) throw new Error(await r.text());
  return r.json();
}
function fillSelect(sel, items, placeholder) {
  sel.innerHTML = '';
  if (placeholder != null) { const o = document.createElement('option'); o.value = ''; o.textContent = placeholder; sel.appendChild(o); }
  for (const it of items) { const o = document.createElement('option'); o.value = it.id; o.textContent = it.name; sel.appendChild(o); }
}
async function bxLoadCollections() {
  const org = document.getElementById('bx-org').value;
  if (!org) return;
  try {
    const colls = await bxGet('/api/browsertrix/collections?org=' + encodeURIComponent(org));
    fillSelect(document.getElementById('bx-collection'), colls, 'All crawls');
  } catch (e) { out.textContent = 'Error: ' + e.message; }
}
// Crawls load reactively (on connect / org / collection change), so a later
// request can overtake an earlier one — bxSeq drops any stale response.
let bxSeq = 0;
let bxItems = [];
async function bxLoadItems() {
  const org = document.getElementById('bx-org').value;
  if (!org) return;
  const coll = document.getElementById('bx-collection').value;
  const seq = ++bxSeq;
  out.textContent = 'Loading crawls…';
  try {
    const items = await bxGet('/api/browsertrix/items?org=' + encodeURIComponent(org)
      + '&collection=' + encodeURIComponent(coll));
    if (seq !== bxSeq) return;
    bxItems = items;
    out.textContent = '';
    bxRender();
  } catch (e) { if (seq === bxSeq) out.textContent = 'Error: ' + e.message; }
}
// Render bxItems into the list, applying the client-side QA-status filter.
function bxRender() {
  const box = document.getElementById('bx-items');
  const mode = (document.getElementById('bx-qa-filter') || {}).value || 'all';
  const hideImported = !!(document.getElementById('bx-hide-imported') || {}).checked;
  box.innerHTML = '';
  if (!bxItems.length) { box.textContent = 'No crawls found.'; return; }
  const items = bxItems.filter(it => (mode === 'reviewed' ? it.reviewed : mode === 'unreviewed' ? !it.reviewed : true)
    && !(hideImported && it.imported));
  if (!items.length) { box.textContent = 'No crawls match this filter.'; return; }
  for (const it of items) {
    const label = document.createElement('label');
    label.className = 'bx-item' + (it.imported ? ' imported' : '');
    const cb = document.createElement('input');
    cb.type = 'checkbox'; cb.dataset.id = it.id; cb.dataset.name = it.name || '';
    // Already in the library — can't be re-imported, so it's shown disabled.
    if (it.imported) cb.disabled = true;
    const nm = document.createElement('span'); nm.className = 'bx-name'; nm.textContent = it.name || it.id;
    const date = document.createElement('span'); date.className = 'bx-date'; date.textContent = it.date || '';
    const qa = document.createElement('span');
    qa.className = 'bx-qa' + (it.reviewed ? ' yes' : '');
    qa.textContent = it.reviewed ? ('QA’d' + (it.review_status ? ' ' + it.review_status + '/5' : '')) : 'not QA’d';
    const size = document.createElement('span'); size.className = 'bx-size';
    size.textContent = it.size_h || '';
    label.append(cb, nm);
    if (it.imported) { const b = document.createElement('span'); b.className = 'bx-badge'; b.textContent = 'in library'; label.append(b); }
    label.append(date, qa, size);
    box.appendChild(label);
  }
}
let bxConnected = false;
async function bxConnect() {
  out.textContent = 'Connecting to Browsertrix…';
  try {
    const orgs = await bxGet('/api/browsertrix/orgs');
    fillSelect(document.getElementById('bx-org'), orgs, null);
    document.getElementById('bx-browse').hidden = false;
    bxConnected = true;
    if (!orgs.length) { out.textContent = 'No organizations visible for these credentials.'; return; }
    out.textContent = '';
    await bxLoadCollections();
    bxLoadItems();
  } catch (e) { out.textContent = 'Error: ' + e.message; }
}
const bxRefresh = document.getElementById('bx-refresh');
if (bxRefresh) bxRefresh.addEventListener('click', bxLoadItems);
const bxQaFilter = document.getElementById('bx-qa-filter');
if (bxQaFilter) bxQaFilter.addEventListener('change', bxRender);
const bxHideImported = document.getElementById('bx-hide-imported');
if (bxHideImported) bxHideImported.addEventListener('change', bxRender);
const bxOrg = document.getElementById('bx-org');
if (bxOrg) bxOrg.addEventListener('change', async () => { await bxLoadCollections(); bxLoadItems(); });
const bxColl = document.getElementById('bx-collection');
if (bxColl) bxColl.addEventListener('change', bxLoadItems);

// ── Archive-It browse (collections → crawls), using server creds. ──
let aitConnected = false;
let aitCrawls = [];
// Crawls load reactively (on connect / collection change); aitSeq drops a stale
// response so a slower earlier request can't overwrite a newer list.
let aitSeq = 0;
async function aitConnect() {
  out.textContent = 'Connecting to Archive-It…';
  try {
    const colls = await bxGet('/api/archiveit/collections');
    const sel = document.getElementById('ait-collection');
    sel.innerHTML = '';
    for (const c of colls) {
      const o = document.createElement('option');
      o.value = c.id;
      o.textContent = c.name + (c.state && c.state !== 'ACTIVE' ? ' (' + c.state.toLowerCase() + ')' : '');
      sel.appendChild(o);
    }
    document.getElementById('ait-browse').hidden = false;
    aitConnected = true;
    if (!colls.length) { out.textContent = 'No Archive-It collections visible for these credentials.'; return; }
    out.textContent = '';
    aitLoadCrawls();
  } catch (e) { out.textContent = 'Error: ' + e.message; }
}
async function aitLoadCrawls() {
  const coll = document.getElementById('ait-collection').value;
  if (!coll) return;
  const seq = ++aitSeq;
  out.textContent = 'Loading crawls…';
  try {
    const crawls = await bxGet('/api/archiveit/crawls?collection=' + encodeURIComponent(coll));
    if (seq !== aitSeq) return;
    aitCrawls = crawls;
    out.textContent = '';
    aitRender();
  } catch (e) { if (seq === aitSeq) out.textContent = 'Error: ' + e.message; }
}
function aitRender() {
  const box = document.getElementById('ait-crawls');
  const hideImported = !!(document.getElementById('ait-hide-imported') || {}).checked;
  box.innerHTML = '';
  if (!aitCrawls.length) { box.textContent = 'No importable crawls found.'; return; }
  const crawls = aitCrawls.filter(c => !(hideImported && c.imported));
  if (!crawls.length) { box.textContent = 'No crawls match this filter.'; return; }
  for (const c of crawls) {
    const label = document.createElement('label');
    label.className = 'bx-item' + (c.imported ? ' imported' : '');
    const cb = document.createElement('input');
    cb.type = 'checkbox'; cb.dataset.id = c.id;
    // Already in the library — can't be re-imported, so it's shown disabled.
    if (c.imported) cb.disabled = true;
    const nm = document.createElement('span'); nm.className = 'bx-name'; nm.textContent = 'crawl ' + c.id;
    // A single (start) date keeps the column within its width; the crawl page
    // shows the full capture-date range.
    const date = document.createElement('span'); date.className = 'bx-date';
    date.textContent = (c.start || c.end || '').slice(0, 10);
    const size = document.createElement('span'); size.className = 'bx-size'; size.textContent = c.size_h || '';
    label.append(cb, nm);
    if (c.imported) { const b = document.createElement('span'); b.className = 'bx-badge'; b.textContent = 'in library'; label.append(b); }
    label.append(date, size);
    box.appendChild(label);
  }
}
const aitColl = document.getElementById('ait-collection');
if (aitColl) aitColl.addEventListener('change', aitLoadCrawls);
const aitRefresh = document.getElementById('ait-refresh');
if (aitRefresh) aitRefresh.addEventListener('click', aitLoadCrawls);
const aitHideImported = document.getElementById('ait-hide-imported');
if (aitHideImported) aitHideImported.addEventListener('change', aitRender);

// Submit: dispatch on the active source tab.
f.addEventListener('submit', async (e) => {
  e.preventDefault();
  const collection = f.collection.value.trim();
  const name = f.name.value.trim();
  if (!collection) { out.textContent = 'Please name the collection.'; return; }
  const active = document.querySelector('.src-tab[aria-selected="true"]');
  const src = active ? active.dataset.src : 'upload';
  let res;
  try {
    if (src === 'upload') {
      const file = f.file.files[0];
      if (!file) { out.textContent = 'Choose a .wacz file to upload.'; return; }
      out.textContent = 'Uploading…';
      const fd = new FormData();
      fd.append('collection', collection);
      if (name) fd.append('name', name);
      fd.append('file', file);
      res = await fetch('/api/archives/upload', { method: 'POST', body: fd });
    } else if (src === 'url') {
      const location = f.location.value.trim();
      if (!location) { out.textContent = 'Enter a path or an http(s):// URL.'; return; }
      out.textContent = 'Starting…';
      const body = { path: location, collection };
      if (name) body.name = name;
      res = await fetch('/api/archives', {
        method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
      });
    } else if (src === 'bx') {
      const checked = [...document.querySelectorAll('#bx-items input:checked')];
      if (!checked.length) { out.textContent = 'Select at least one crawl to import.'; return; }
      out.textContent = 'Importing…';
      const mode = (document.querySelector('input[name="bx-mode"]:checked') || {}).value;
      const body = {
        org: document.getElementById('bx-org').value,
        collection,
        download: mode !== 'stream',
        items: checked.map(cb => {
          const m = bxItems.find(x => x.id === cb.dataset.id) || {};
          return { id: cb.dataset.id, name: cb.dataset.name, review_status: m.review_status ?? null };
        }),
      };
      res = await fetch('/api/browsertrix/import', {
        method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
      });
    } else if (src === 'ait') {
      const checked = [...document.querySelectorAll('#ait-crawls input:checked')];
      if (!checked.length) { out.textContent = 'Select at least one crawl to import.'; return; }
      out.textContent = 'Importing…';
      const body = {
        collection_id: parseInt(document.getElementById('ait-collection').value, 10),
        collection,
        crawls: checked.map(cb => parseInt(cb.dataset.id, 10)),
      };
      res = await fetch('/api/archiveit/import', {
        method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
      });
    } else {
      out.textContent = 'That source isn’t available yet.';
      return;
    }
  } catch (err) { out.textContent = 'Request failed: ' + err; return; }
  if (!res.ok) { out.textContent = 'Error: ' + (await res.text()); return; }
  const { job } = await res.json();
  stream(job, collection);
});
"#;

/// The accession desk (`/manage/add`): add crawls to a collection from a source.
/// `collection` prefills the target when arriving from a collection page. All
/// four sources are live: Upload, Path/URL, and the Browsertrix and Archive-It
/// browse-and-import wizards (each uses the server's configured credentials).
pub fn accession_desk(
    collection_id: &str,
    collection_name: &str,
    signed_in: Option<&str>,
) -> Markup {
    let body = html! {
        div.crumbs {
            a href="/" { "Home" }
            span.sep { " / " }
            b { "Add crawls" }
        }
        span.eyebrow { "Accession" }
        h1.page-title { "Add crawls" }

        form #add-archive-form.manage-form {
            label {
                span { "Collection" }
                input type="text" name="collection" required value=(collection_name)
                    placeholder="which collection this belongs to";
            }
            label {
                span { "Display name " span.hint { "· optional" } }
                input type="text" name="name" placeholder="override the collection's display name";
            }

            div.sources role="tablist" {
                button.src-tab type="button" role="tab" aria-selected="true" data-src="upload" { "Upload" }
                button.src-tab type="button" role="tab" aria-selected="false" data-src="url" { "Path / URL" }
                button.src-tab type="button" role="tab" aria-selected="false" data-src="bx" { "Browsertrix" }
                button.src-tab type="button" role="tab" aria-selected="false" data-src="ait" { "Archive-It" }
            }
            div.src-panel.active #src-upload {
                label {
                    span { "Upload a " code { ".wacz" } " file" }
                    input type="file" name="file" accept=".wacz";
                }
            }
            div.src-panel #src-url {
                label {
                    span { "Location " span.hint { "· a local path or an http(s):// URL" } }
                    input type="text" name="location"
                        placeholder="/path/to/crawl.wacz or https://example.org/crawl.wacz";
                }
            }
            div.src-panel #src-bx {
                p.muted { "Pick crawls to import into the collection above, from the Browsertrix instance this server is configured for (its credentials + host)." }
                div #bx-browse.bx-browse hidden {
                    div.grid-2 {
                        label { span { "Organization" } select #bx-org {} }
                        label {
                            span { "Browsertrix collection " span.hint { "· optional filter" } }
                            select #bx-collection { option value="" { "All crawls" } }
                        }
                    }
                    div.bx-toolbar {
                        label.bx-filter {
                            span { "Show" }
                            select #bx-qa-filter {
                                option value="all" { "All crawls" }
                                option value="reviewed" { "QA’d only" }
                                option value="unreviewed" { "Not QA’d" }
                            }
                        }
                        label.bx-check {
                            input type="checkbox" #bx-hide-imported;
                            span { "Hide already-imported" }
                        }
                        button.btn.ghost type="button" #bx-refresh { "Refresh list" }
                    }
                }
                div #bx-items.bx-items {}
                fieldset.bx-mode {
                    legend { "On import" }
                    label { input type="radio" name="bx-mode" value="download" checked; span { "Download a durable copy " span.hint { "· stored locally, replays offline" } } }
                    label { input type="radio" name="bx-mode" value="stream"; span { "Stream in place " span.hint { "· no local copy; replay re-resolves via this server's credentials" } } }
                }
            }
            div.src-panel #src-ait {
                p.muted { "Pick crawls to import into the collection above, from the Archive-It account this server is configured for. Each selected crawl is downloaded and packaged into a WACZ." }
                div #ait-browse.bx-browse hidden {
                    label { span { "Archive-It collection" } select #ait-collection {} }
                    div.bx-toolbar {
                        label.bx-check {
                            input type="checkbox" #ait-hide-imported;
                            span { "Hide already-imported" }
                        }
                        button.btn.ghost type="button" #ait-refresh { "Refresh list" }
                    }
                }
                div #ait-crawls.bx-items {}
            }

            div.form-actions {
                button.btn type="submit" { "Add" }
                a.cancel href="/" { "Cancel" }
            }
        }
        pre #add-progress.progress {}
        script { (PreEscaped(ADD_ARCHIVE_JS)) }
    };
    // Scope the header search to the target collection, when known.
    let search = if collection_id.is_empty() {
        SearchBox::default()
    } else {
        SearchBox {
            scope_query: format!("collection:{collection_id}"),
            scope_label: collection_name.to_string(),
            ..Default::default()
        }
    };
    layout(
        "Add crawls - indice",
        true,
        signed_in,
        false,
        Some(&search),
        body,
    )
}
