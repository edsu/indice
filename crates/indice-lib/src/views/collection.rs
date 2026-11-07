//! Collection detail — the finding aid — and the per-collection annotations
//! index it links to.

use maud::{html, Markup, PreEscaped};

use super::*;

/// A member crawl (WACZ) as shown in a collection's grid.
pub struct MemberItem {
    pub id: String,
    pub name: String,
    /// When the crawl ran, in words, if known.
    pub when: Option<String>,
    pub present: bool,
    /// Whether this crawl is hosted remotely (streamed) rather than local.
    pub remote: bool,
    /// One-line provenance summary (plain text), if any is known.
    pub provenance: Option<String>,
    /// `/thumb/{id}` for this crawl's representative image, if it has one.
    pub thumb: Option<String>,
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
    /// The member crawls, newest first.
    pub members: Vec<MemberItem>,
    /// Pages crawled per month across the collection's span.
    pub crawl_months: Vec<TimelineBar>,
    /// The address that replays the whole collection (multi-WACZ), when it
    /// has a page to open on.
    pub replay_href: Option<String>,
    /// Collection id (slug) — for the edit / add-crawls links.
    pub id: String,
    /// Whether to render the danger zone. Deaccession is an admin act, so a
    /// curator must not be shown a button that would 403.
    pub can_delete: bool,
    /// Who is looking, for the shared header.
    pub chrome: Chrome,
    /// How many page annotations this collection has (drives the summary link).
    pub annotation_count: usize,
    /// Which fields began as a language model's draft, and who reviewed them,
    /// already phrased, e.g. "Description and subjects drafted with
    /// llama3.1:8b (localhost:11434), reviewed by Ed."
    pub drafted_note: Option<String>,
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
    /// Returns message keys, which the view words in the page language.
    fn missing_minimum(&self) -> Vec<&'static str> {
        let mut m = Vec::new();
        if self.narrative.is_none() {
            m.push("collection-field-scope");
        }
        if self.creator.is_none() {
            m.push("collection-field-creator");
        }
        if self.rights.is_none() {
            m.push("collection-field-access");
        }
        m
    }

    /// The curatorial (finding-aid) metadata table, with DACS-labelled rows.
    fn curatorial_rows(&self) -> Vec<MetaRow> {
        let t = |key: &str| self.chrome.l10n.tr(key);
        let mut rows = Vec::new();
        if let Some(v) = &self.creator {
            rows.push(MetaRow::new(&t("collection-field-creator"), v.clone()));
        }
        if let Some(v) = &self.dates {
            rows.push(MetaRow::new(&t("collection-field-dates"), v.clone()));
        }
        if let Some(v) = &self.rights {
            rows.push(MetaRow::new(&t("collection-field-rights"), v.clone()));
        }
        if !self.subjects.is_empty() {
            rows.push(MetaRow::new(
                &t("collection-field-subjects"),
                self.subjects.join(", "),
            ));
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
    let l10n = &p.chrome.l10n;
    let t = |key: &str| l10n.tr(key);
    let body = html! {
        h1.page-title { (p.name) }
        @if let Some(d) = &p.description { p.desc { (d) } }
        // One action row: Replay plus, in workroom mode, the edit-in-place
        // curation actions — all the same button treatment.
        div.actions {
            @if let Some(href) = &p.replay_href {
                a.btn href=(href) { (t("collection-replay")) }
            }
            @if p.chrome.manage {
                a.btn.edit href=(format!("/manage/edit/{}", p.id)) { (t("collection-edit")) }
                a.btn.edit href=(format!("/manage/add?collection={}", p.id)) { (t("collection-add-crawls")) }
            }
        }
        section.about {
            h2 { (t("collection-about")) }
            @if p.has_curatorial() {
                @if let Some(n) = &p.narrative { div.narrative { (n) } }
                @if !curatorial.is_empty() { (meta_table(&curatorial)) }
                @if let Some(note) = &p.drafted_note { p.muted.drafted-note { (note) } }
                // Even partly-filled, name the DACS-minimum elements still
                // missing — the fields ingest can't supply reliably (a real
                // creator, the scope rationale, use conditions).
                @if !missing.is_empty() {
                    p.muted.nudge {
                        @let list = missing.iter().map(|k| t(k)).collect::<Vec<_>>().join(", ");
                        (l10n.tr_args("collection-needed", &[("list", FluentArg::Str(&list))])) " "
                        @if p.chrome.manage {
                            a href=(format!("/manage/edit/{}", p.id)) { (t("collection-edit-link")) }
                            " " (t("collection-edit-to-add"))
                        } @else {
                            (t("collection-add-with")) " "
                            code { "indice collection set \"" (p.name) "\" …" }
                            " " (t("collection-or-edit")) " "
                            code { "collections/" (p.id) "/README.md" }
                            "."
                        }
                    }
                }
            } @else {
                // Empty-state nudge: name the DACS single-level minimum
                // curatorial elements that are missing, with archival authority.
                p.muted.nudge {
                    (t("collection-empty-nudge")) " "
                    @if p.chrome.manage {
                        a href=(format!("/manage/edit/{}", p.id)) { (t("collection-edit-link")) }
                        " " (t("collection-edit-to-describe"))
                    } @else {
                        (t("collection-add-with")) " "
                        code { "indice collection set \"" (p.name) "\" --creator \"…\"" }
                        " " (t("collection-or-edit")) " "
                        code { "collections/" (p.id) "/README.md" }
                        "."
                    }
                }
            }
        }

        @if p.annotation_count > 0 {
            section.annotations-summary {
                h2 { (t("collection-annotations-heading")) }
                p.muted {
                    (counted(l10n, "collection-annotation-count", p.annotation_count as u64)) " "
                    a href=(format!("/collection/{}/annotations", p.id)) { (t("collection-browse-annotations")) }
                }
            }
        }

        @if !p.meta.is_empty() { (meta_table(&p.meta)) }
        (facet_browse(&p.facets, l10n))
        h2 { (t("collection-crawls-heading")) }
        // When this collection was crawled, then its crawls, newest first: a
        // collection grows over time, and the list reads as that history.
        (time_axis(&p.crawl_months, l10n, &t("collection-timeline-label"), "timeline-pages-count"))
        @if p.members.is_empty() {
            p.muted { (t("collection-no-crawls")) }
        } @else {
            ol.crawl-list {
                @for m in &p.members {
                    li.crawl-item {
                        a.crawl-thumb href=(format!("/crawl/{}", m.id)) tabindex="-1" aria-hidden="true" {
                            (thumb_area(m.thumb.as_deref()))
                        }
                        div.crawl-text {
                            @if let Some(w) = &m.when { div.crawl-when { (w) } }
                            a.card-title href=(format!("/crawl/{}", m.id)) { (m.name) }
                            p.card-source {
                                @if m.remote { (t("source-streamed")) } @else { (t("source-stored")) }
                                @if !m.present { span.missing { ". " (t("crawl-file-missing")) } }
                            }
                            @if let Some(pr) = &m.provenance { p.crawl-prov { (pr) } }
                        }
                    }
                }
            }
        }
        // Last on the page, as on a crawl page: deleting is the rarest thing
        // anyone comes here to do, and it should never sit beside the actions.
        @if p.can_delete {
            details.danger-zone {
                summary { (t("collection-delete-summary")) }
                form.confirm-delete method="post" action=(format!("/api/collections/{}/delete", p.id)) {
                    @if p.members.is_empty() {
                        p.muted { (t("collection-delete-empty-note")) }
                    } @else {
                        p.muted { (counted(l10n, "collection-delete-note", p.members.len() as u64)) }
                        label.confirm-with-crawls {
                            input type="checkbox" name="with_crawls" value="true";
                            span { (counted(l10n, "collection-delete-also", p.members.len() as u64)) }
                        }
                    }
                    button.btn.danger type="submit" { (t("action-delete-permanently")) }
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
    layout(Some(&p.name), &p.chrome, Some(&search), body)
}

/// One annotation as shown on the per-collection index, with a deep link that
/// opens its page in the collection replay (where it re-anchors + highlights).
pub struct AnnoLink {
    pub author: String,
    pub date: String,
    pub note_html: PreEscaped<String>,
    pub page_url: String,
    pub replay_href: String,
    /// The note's own page, `/collection/{id}/annotations/{key}`; `None` for
    /// a note whose id indice did not mint, which has none.
    pub permalink: Option<String>,
    /// The note's key, which also anchors it on the Annotations page.
    pub key: String,
    /// The quoted passage, for a region note; `None` for a whole-page note.
    pub region: Option<String>,
}

/// One note on a page of its own.
pub struct AnnotationPage {
    pub collection_name: String,
    pub collection_id: String,
    pub note: AnnoLink,
    /// Where the note's JSON-LD is served on its own (`….jsonld`).
    pub json_ld_href: String,
    /// The note as a W3C Web Annotation, already escaped for a `<script>`.
    pub json_ld: String,
    /// Who is looking, for the shared header.
    pub chrome: Chrome,
}

pub struct AnnotationsIndexPage {
    pub collection_name: String,
    pub collection_id: String,
    pub items: Vec<AnnoLink>,
    /// Who is looking, for the shared header.
    pub chrome: Chrome,
}

/// A public browse of every annotation in a collection.
pub fn annotations_index(p: &AnnotationsIndexPage) -> Markup {
    let plural = if p.items.len() == 1 { "note" } else { "notes" };
    let body = html! {
        p.crumb {
            a href=(format!("/collection/{}", p.collection_id)) { (p.collection_name) }
            " / Annotations"
        }
        h1.page-title { "Annotations" }
        p.desc { (p.items.len()) " " (plural) " on pages in this collection." }
        @if p.items.is_empty() {
            p.muted { "No annotations yet." }
        } @else {
            ul.anno-index {
                @for a in &p.items {
                    li.anno-index-item id=(a.key) { (anno_entry(a, true)) }
                }
            }
        }
    };
    layout(
        Some(&format!("Annotations - {}", p.collection_name)),
        &p.chrome,
        None,
        body,
    )
}

/// A note as the Annotations page and its own page show it; `linked` adds the
/// link to its own page, which that page doesn't need.
fn anno_entry(a: &AnnoLink, linked: bool) -> Markup {
    html! {
        div.anno-index-meta {
            span.anno-author { (a.author) } ", " (a.date)
        }
        @if let Some(q) = &a.region {
            p.anno-quote { "“" (q) "”" }
        }
        div.anno-index-body { (a.note_html) }
        p.anno-index-loc {
            a href=(a.replay_href) { "Open page" }
            " " span.muted { (a.page_url) }
        }
        @if let (true, Some(href)) = (linked, &a.permalink) {
            p.anno-index-loc { a href=(href) { "Link to this note" } }
        }
    }
}

/// One note on a page of its own, at the URL its JSON-LD names as its id.
/// The page carries that JSON-LD too, for whatever reads pages rather than
/// asking for JSON.
pub fn annotation_page(p: &AnnotationPage) -> Markup {
    let body = html! {
        p.crumb {
            a href=(format!("/collection/{}", p.collection_id)) { (p.collection_name) }
            " / "
            // Back to this note's place in the list.
            a href=(format!("/collection/{}/annotations#{}", p.collection_id, p.note.key)) { "Annotations" }
        }
        h1.page-title { "Note" }
        div.anno-index-item { (anno_entry(&p.note, false)) }
        p.anno-index-loc {
            "This note as a W3C Web Annotation: "
            a href=(p.json_ld_href) rel="alternate" type="application/ld+json" { "JSON-LD" }
        }
        // Data, not script: a type the browser does not run, so the policy
        // refusing inline script does not apply.
        script type="application/ld+json" { (PreEscaped(&p.json_ld)) }
    };
    layout(
        Some(&format!("Note - {}", p.collection_name)),
        &p.chrome,
        None,
        body,
    )
}
