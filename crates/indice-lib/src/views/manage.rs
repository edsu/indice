//! Management UI: the finding-aid form and the accession
//! desk. The desk's behaviour lives in `static/assets/manage.js`.

use maud::{html, Markup};

use super::*;

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
    /// The model the Draft button uses, when one is configured.
    pub drafting: Option<DraftingInfo>,
    /// A draft to show, after the Draft button was pressed.
    pub draft: Option<FormDraft>,
    /// Why a draft couldn't be made, shown above the form.
    pub error: Option<String>,
}

/// Which model drafts, and at which service, for the line beside the button.
pub struct DraftingInfo {
    pub model: String,
    pub service: String,
}

/// A model's draft as the editor shows it. Fields that were empty already
/// hold the drafted text in their inputs; for fields the curator had written,
/// the draft is offered beside them, to use only if asked.
pub struct FormDraft {
    pub model: String,
    pub service: String,
    pub at: String,
    /// Fields the draft filled because they were empty.
    pub filled: Vec<String>,
    /// `(field, drafted text)` for fields already written.
    pub suggestions: Vec<(String, String)>,
}

impl FormDraft {
    fn filled(&self, field: &str) -> bool {
        self.filled.iter().any(|f| f == field)
    }

    fn suggestion(&self, field: &str) -> Option<&str> {
        self.suggestions
            .iter()
            .find(|(f, _)| f == field)
            .map(|(_, v)| v.as_str())
    }
}

/// The marker on a field a draft filled: say so, so the curator reviews it.
fn drafted_mark(draft: Option<&FormDraft>, field: &str) -> Markup {
    html! {
        @if draft.is_some_and(|d| d.filled(field)) {
            " " span.drafted-mark { "drafted, review before saving" }
        }
    }
}

/// For a field already written, the draft's alternative, used only if the
/// curator ticks the box.
fn draft_suggestion(draft: Option<&FormDraft>, field: &str) -> Markup {
    html! {
        @if let Some(text) = draft.and_then(|d| d.suggestion(field)) {
            div.draft-suggestion {
                span.draft-label { "The draft suggests:" }
                div.draft-text { (text) }
                label.use-draft {
                    input type="checkbox" name=(format!("use_draft_{field}"));
                    " Use the draft instead"
                }
                input type="hidden" name=(format!("draft_{field}")) value=(text);
            }
        }
    }
}

/// The finding-aid form (`/manage/collections/new` and `/manage/edit/{id}`):
/// create or edit a collection's curatorial description. POSTs to
/// `/api/collections`.
pub fn collection_form(form: &CollectionFormData, chrome: &Chrome) -> Markup {
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
        @if let Some(e) = &form.error {
            p.form-error { (e) }
        }
        @if let Some(d) = &form.draft {
            p.draft-note {
                "Drafted with " b { (d.model) } " at " (d.service) ". "
                "Fields marked “drafted” were empty and now hold the draft; where you had already written something, the draft is shown below it, to use only if you choose. Nothing is saved until you press Save."
            }
        }
        form.manage-form method="post" action="/api/collections" {
            @if let Some(d) = &form.draft {
                input type="hidden" name="draft_model" value=(d.model);
                input type="hidden" name="draft_service" value=(d.service);
                input type="hidden" name="draft_at" value=(d.at);
                input type="hidden" name="drafted" value=(d.filled.join(","));
            }
            label {
                span { "Name" }
                input type="text" name="name" required value=(form.name) readonly[form.editing];
            }
            label {
                span { "Description" (drafted_mark(form.draft.as_ref(), "description")) span.hint { "a one-line summary, shown on cards and under the title" } }
                input type="text" name="description" value=(form.description);
            }
            (draft_suggestion(form.draft.as_ref(), "description"))
            div.grid-2 {
                label { span { "Creator" } input type="text" name="creator" value=(form.creator); }
                label { span { "Dates" (drafted_mark(form.draft.as_ref(), "dates")) } input type="text" name="dates" value=(form.dates); }
                label { span { "Curator" } input type="text" name="curator" value=(form.curator); }
                label { span { "Rights" } input type="text" name="rights" value=(form.rights); }
            }
            (draft_suggestion(form.draft.as_ref(), "dates"))
            label {
                span { "Subjects" (drafted_mark(form.draft.as_ref(), "subjects")) span.hint { "comma-separated" } }
                input type="text" name="subjects" value=(form.subjects);
            }
            (draft_suggestion(form.draft.as_ref(), "subjects"))
            label {
                span { "Narrative" (drafted_mark(form.draft.as_ref(), "narrative")) span.hint { "the full finding-aid prose (Markdown): Scope & Content, custodial history, appraisal" } }
                textarea name="narrative" rows="8" { (form.narrative) }
            }
            (draft_suggestion(form.draft.as_ref(), "narrative"))
            div.form-actions {
                button.btn type="submit" { @if form.editing { "Save changes" } @else { "Create collection" } }
                @if let (true, Some(m)) = (form.editing, &form.drafting) {
                    // Posts the form as it stands, so unsaved edits count, to
                    // the draft route instead of saving.
                    button.btn.draft type="submit" formaction=(format!("/manage/edit/{}/draft", form.id)) formnovalidate {
                        @if form.draft.is_some() { "Draft again" } @else { "Draft a description" }
                    }
                    span.hint.draft-with { "with " (m.model) " at " (m.service) ", which is sent a sample of this collection's archived text" }
                }
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
    layout(Some("Manage"), chrome, Some(&search), body)
}

/// Which import wizards the accession desk offers.
#[derive(Default)]
pub struct ImportTabs {
    pub browsertrix: bool,
    pub archive_it: bool,
}

/// The accession desk (`/manage/add`): add crawls to a collection from a source.
/// `collection` prefills the target when arriving from a collection page. The
/// sources are Upload and URL, plus whichever of the Browsertrix and Archive-It
/// browse-and-import wizards `imports` allows: they act through the server's
/// configured credentials, so the caller decides by role and configuration.
pub fn accession_desk(
    collection_id: &str,
    collection_name: &str,
    chrome: &Chrome,
    // How much room an accession has, as a sentence; `None` when no limit is
    // configured, so an unlimited archive shows nothing rather than "∞".
    room: Option<&str>,
    // The limits again, as bytes and the server's own refusal, so the page can
    // refuse a file too large *before sending it*. See `manage.js`.
    max_upload: Option<&(u64, String)>,
    room_left: Option<&(u64, String)>,
    imports: ImportTabs,
) -> Markup {
    let body = html! {
        div.crumbs {
            a href="/" { "Home" }
            span.sep { " / " }
            b { "Add crawls" }
        }
        span.eyebrow { "Accession" }
        h1.page-title { "Add crawls" }
        @if let Some(room) = room {
            p.hint.room { (room) }
        }

        // Carried on the form rather than checked only by the server, because a
        // server refusing a body the browser is still sending closes the
        // connection, and a browser may then report a network error instead of
        // the refusal. Checking here means the common case never sends at all;
        // the server still enforces both, for every other client and for a room
        // that shrank after this page loaded.
        form #add-archive-form.manage-form
            data-max-upload=[max_upload.map(|m| m.0)]
            data-max-upload-msg=[max_upload.map(|m| m.1.as_str())]
            data-room=[room_left.map(|r| r.0)]
            data-room-msg=[room_left.map(|r| r.1.as_str())] {
            label {
                span { "Collection" }
                input type="text" name="collection" required value=(collection_name)
                    placeholder="which collection this belongs to";
            }
            label {
                span { "Display name " span.hint { "optional" } }
                input type="text" name="name" placeholder="override the collection's display name";
            }

            div.sources role="tablist" {
                button.src-tab type="button" role="tab" aria-selected="true" data-src="upload" { "Upload" }
                button.src-tab type="button" role="tab" aria-selected="false" data-src="url" { "URL" }
                @if imports.browsertrix {
                    button.src-tab type="button" role="tab" aria-selected="false" data-src="bx" { "Browsertrix" }
                }
                @if imports.archive_it {
                    button.src-tab type="button" role="tab" aria-selected="false" data-src="ait" { "Archive-It" }
                }
            }
            div.src-panel.active #src-upload {
                label {
                    span { "Upload a " code { ".wacz" } " file" }
                    input type="file" name="file" accept=".wacz";
                }
            }
            div.src-panel #src-url {
                label {
                    span { "Location " span.hint { "an http(s):// URL" } }
                    input type="text" name="location"
                        placeholder="https://example.org/crawl.wacz";
                }
            }
            @if imports.browsertrix {
                div.src-panel #src-bx {
                    p.muted { "Pick crawls to import into the collection above, from the Browsertrix instance this server is configured for (its credentials + host)." }
                    div #bx-browse.bx-browse hidden {
                        div.grid-2 {
                            label { span { "Organization" } select #bx-org {} }
                            label {
                                span { "Browsertrix collection " span.hint { "optional filter" } }
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
                        label { input type="radio" name="bx-mode" value="download" checked; span { "Download a durable copy " span.hint { "stored locally, replays offline" } } }
                        label { input type="radio" name="bx-mode" value="stream"; span { "Stream in place " span.hint { "no local copy; replay re-resolves via this server's credentials" } } }
                    }
                }
            }
            @if imports.archive_it {
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
            }

            div.form-actions {
                button.btn type="submit" { "Add" }
                a.cancel href="/" { "Cancel" }
            }
        }
        pre #add-progress.progress {}
        // Progressive enhancement (source tabs, submit, the Browsertrix /
        // Archive-It browse wizards, SSE progress). Served from static/assets so
        // it stays lintable JavaScript rather than a Rust string literal.
        script src="/assets/manage.js" {}
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
    layout(Some("Add crawls"), chrome, Some(&search), body)
}
