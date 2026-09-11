//! Collection detail — the finding aid — and the per-collection annotations
//! index it links to.

use maud::{html, Markup, PreEscaped};

use super::*;

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
    /// Whether to render the danger zone. Deaccession is an admin act, so a
    /// curator must not be shown a button that would 403.
    pub can_delete: bool,
    /// Signed-in user (forward-auth), shown in the workroom strip.
    pub signed_in: Option<String>,
    /// Forward-auth configured but this request anonymous — show a "Log in" link.
    pub can_login: bool,
    /// How many page annotations this collection has (drives the summary link).
    pub annotation_count: usize,
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
        @if p.can_delete {
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

        @if p.annotation_count > 0 {
            @let plural = if p.annotation_count == 1 { "note" } else { "notes" };
            section.annotations-summary {
                h2 { "Annotations" }
                p.muted {
                    (p.annotation_count) " page " (plural) " in this collection. "
                    a href=(format!("/collection/{}/annotations", p.id)) { "Browse annotations →" }
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

/// One annotation as shown on the per-collection index, with a deep link that
/// opens its page in the collection replay (where it re-anchors + highlights).
pub struct AnnoLink {
    pub author: String,
    pub date: String,
    pub note_html: PreEscaped<String>,
    pub page_url: String,
    pub replay_href: String,
    /// The quoted passage, for a region note; `None` for a whole-page note.
    pub region: Option<String>,
}

pub struct AnnotationsIndexPage {
    pub collection_name: String,
    pub collection_id: String,
    pub items: Vec<AnnoLink>,
    pub management: bool,
    pub signed_in: Option<String>,
    pub can_login: bool,
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
                    li.anno-index-item {
                        div.anno-index-meta {
                            span.anno-author { (a.author) } " · " (a.date)
                        }
                        @if let Some(q) = &a.region {
                            p.anno-quote { "“" (q) "”" }
                        }
                        div.anno-index-body { (a.note_html) }
                        p.anno-index-loc {
                            a href=(a.replay_href) { "Open page →" }
                            " " span.muted { (a.page_url) }
                        }
                    }
                }
            }
        }
    };
    layout(
        &format!("Annotations - {} - indice", p.collection_name),
        p.management,
        p.signed_in.as_deref(),
        p.can_login,
        None,
        body,
    )
}
