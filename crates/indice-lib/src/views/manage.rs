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

/// Which model drafts, at which service, what the operator says about it,
/// and what would be sent: shown beside the Draft button, before anything is.
pub struct DraftingInfo {
    pub model: String,
    pub service: String,
    /// The operator's note about the service (`llm.notice`), rendered.
    pub notice: Option<maud::PreEscaped<String>>,
    /// How long the editor waits for an answer, for the waiting message.
    pub timeout_secs: u64,
    /// What a draft would send now, when it could be worked out.
    pub preview: Option<DraftPreview>,
}

/// What pressing Draft would send.
pub struct DraftPreview {
    /// `(title, url)` of each page included.
    pub pages: Vec<(String, String)>,
    /// How much would be sent, in characters.
    pub chars: usize,
    /// The whole prompt: the instructions, then the material.
    pub prompt: String,
}

/// A model's draft as the editor shows it. Fields that were empty already
/// hold the drafted text in their inputs; for fields the curator had written,
/// the draft is offered beside them, to use only if asked.
pub struct FormDraft {
    pub model: String,
    pub service: String,
    pub at: String,
    /// Fields holding drafted text: filled because they were empty, or still
    /// holding an earlier draft's text.
    pub filled: Vec<String>,
    /// `(field, drafted text)` for fields already written.
    pub suggestions: Vec<(String, String)>,
    /// `(field, drafted text)` for every field the draft covers, carried in
    /// hidden inputs so Save can tell which the curator left as drafted.
    pub texts: Vec<(String, String)>,
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

/// Below the Draft button, before anything is sent: which model at which
/// service, the operator's note about it, and what would be sent, down to the
/// pages and the full prompt.
fn draft_panel(m: &DraftingInfo, l10n: &crate::i18n::L10n) -> Markup {
    html! {
        div.draft-panel {
            p.draft-who {
                (l10n.tr("draft-panel-with")) " " b { (m.model) } " " (l10n.tr("draft-panel-at")) " " b { (m.service) }
                ", " (l10n.tr("draft-panel-sent"))
            }
            @if let Some(n) = &m.notice { div.draft-notice { (n) } }
            @if let Some(p) = &m.preview {
                details.draft-preview {
                    summary {
                        (l10n.tr_args("draft-panel-summary", &[
                            ("pages", FluentArg::Str(&counted(l10n, "draft-panel-pages", p.pages.len() as u64))),
                            ("chars", FluentArg::Str(&counted(l10n, "draft-panel-chars", p.chars as u64))),
                        ]))
                    }
                    ol.draft-pages {
                        @for (title, url) in &p.pages {
                            li {
                                @if title.trim().is_empty() { (url) } @else { (title) " " span.url { (url) } }
                            }
                        }
                    }
                    details.draft-prompt {
                        summary { (l10n.tr("draft-panel-prompt")) }
                        pre { (p.prompt) }
                    }
                }
            }
            // Filled in by editor.js while a draft is being made.
            p #draft-status.draft-status hidden {}
        }
        script src="/assets/editor.js" defer {}
    }
}

/// The marker on a field a draft filled: say so, so the curator reviews it.
fn drafted_mark(draft: Option<&FormDraft>, field: &str, l10n: &crate::i18n::L10n) -> Markup {
    html! {
        @if draft.is_some_and(|d| d.filled(field)) {
            " " span.drafted-mark { (l10n.tr("form-drafted-mark")) }
        }
    }
}

/// For a field already written, the draft's alternative, used only if the
/// curator ticks the box.
fn draft_suggestion(draft: Option<&FormDraft>, field: &str, l10n: &crate::i18n::L10n) -> Markup {
    html! {
        @if let Some(text) = draft.and_then(|d| d.suggestion(field)) {
            div.draft-suggestion {
                span.draft-label { (l10n.tr("form-draft-suggests")) }
                div.draft-text { (text) }
                label.use-draft {
                    input type="checkbox" name=(format!("use_draft_{field}"));
                    " " (l10n.tr("form-use-draft"))
                }
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
    let l10n = &chrome.l10n;
    let t = |key: &str| l10n.tr(key);
    let body = html! {
        div.crumbs {
            a href="/" { (t("crumb-home")) }
            span.sep { " / " }
            @if form.editing { b { (form.name) } } @else { b { (t("form-new-heading")) } }
        }
        span.eyebrow { (t("form-eyebrow")) }
        h1.page-title { @if form.editing { (t("form-edit-heading")) } @else { (t("form-new-heading")) } }
        @if form.editing {
            p.muted { (t("form-name-fixed")) }
        }
        @if let Some(e) = &form.error {
            p.form-error { (e) }
        }
        @if let Some(d) = &form.draft {
            p.draft-note {
                (t("form-drafted-with")) " " b { (d.model) } " " (t("form-drafted-at")) " " (d.service) ". "
                (t("form-drafted-explain"))
            }
        }
        form.manage-form method="post" action="/api/collections" {
            @if let Some(d) = &form.draft {
                input type="hidden" name="draft_model" value=(d.model);
                input type="hidden" name="draft_service" value=(d.service);
                input type="hidden" name="draft_at" value=(d.at);
                @for (field, text) in &d.texts {
                    input type="hidden" name=(format!("draft_{field}")) value=(text);
                }
            }
            label {
                span { (t("form-label-name")) }
                input type="text" name="name" required value=(form.name) readonly[form.editing];
            }
            label {
                span { (t("form-label-description")) (drafted_mark(form.draft.as_ref(), "description", l10n)) span.hint { (t("form-hint-description")) } }
                input type="text" name="description" value=(form.description);
            }
            (draft_suggestion(form.draft.as_ref(), "description", l10n))
            div.grid-2 {
                label { span { (t("collection-field-creator")) } input type="text" name="creator" value=(form.creator); }
                label { span { (t("collection-field-dates")) (drafted_mark(form.draft.as_ref(), "dates", l10n)) } input type="text" name="dates" value=(form.dates); }
                label { span { (t("meta-curator")) } input type="text" name="curator" value=(form.curator); }
                label { span { (t("collection-field-rights")) } input type="text" name="rights" value=(form.rights); }
            }
            (draft_suggestion(form.draft.as_ref(), "dates", l10n))
            label {
                span { (t("collection-field-subjects")) (drafted_mark(form.draft.as_ref(), "subjects", l10n)) span.hint { (t("form-hint-subjects")) } }
                input type="text" name="subjects" value=(form.subjects);
            }
            (draft_suggestion(form.draft.as_ref(), "subjects", l10n))
            label {
                span { (t("form-label-narrative")) (drafted_mark(form.draft.as_ref(), "narrative", l10n)) span.hint { (t("form-hint-narrative")) } }
                textarea name="narrative" rows="8" { (form.narrative) }
            }
            (draft_suggestion(form.draft.as_ref(), "narrative", l10n))
            div.form-actions {
                button.btn type="submit" { @if form.editing { (t("form-save")) } @else { (t("form-create")) } }
                @if let (true, Some(m)) = (form.editing, &form.drafting) {
                    // Posts the form as it stands, so unsaved edits count, to
                    // the draft route instead of saving.
                    button.btn.draft type="submit" formaction=(format!("/manage/edit/{}/draft", form.id)) formnovalidate
                        data-service=(m.service) data-timeout=(m.timeout_secs) {
                        @if form.draft.is_some() { (t("form-draft-again")) } @else { (t("form-draft-new")) }
                    }
                }
                a.cancel href=(back) { (t("action-cancel")) }
            }
            @if let (true, Some(m)) = (form.editing, &form.drafting) {
                (draft_panel(m, l10n))
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
    layout(Some(&t("layout-manage")), chrome, Some(&search), body)
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
    let l10n = &chrome.l10n;
    let t = |key: &str| l10n.tr(key);
    let body = html! {
        div.crumbs {
            a href="/" { (t("crumb-home")) }
            span.sep { " / " }
            b { (t("desk-heading")) }
        }
        span.eyebrow { (t("desk-eyebrow")) }
        h1.page-title { (t("desk-heading")) }
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
                span { (t("desk-collection")) }
                input type="text" name="collection" required value=(collection_name)
                    placeholder=(t("desk-collection-placeholder"));
            }
            label {
                span { (t("desk-name")) " " span.hint { (t("desk-optional")) } }
                input type="text" name="name" placeholder=(t("desk-name-placeholder"));
            }

            div.sources role="tablist" {
                button.src-tab type="button" role="tab" aria-selected="true" data-src="upload" { (t("desk-tab-upload")) }
                button.src-tab type="button" role="tab" aria-selected="false" data-src="url" { (t("desk-tab-url")) }
                @if imports.browsertrix {
                    button.src-tab type="button" role="tab" aria-selected="false" data-src="bx" { "Browsertrix" }
                }
                @if imports.archive_it {
                    button.src-tab type="button" role="tab" aria-selected="false" data-src="ait" { "Archive-It" }
                }
            }
            div.src-panel.active #src-upload {
                label {
                    span { (t("desk-upload-before")) " " code { ".wacz" } " " (t("desk-upload-after")) }
                    input type="file" name="file" accept=".wacz";
                }
            }
            div.src-panel #src-url {
                label {
                    span { (t("desk-location")) " " span.hint { (t("desk-location-hint")) } }
                    input type="text" name="location"
                        placeholder="https://example.org/crawl.wacz";
                }
            }
            @if imports.browsertrix {
                div.src-panel #src-bx {
                    p.muted { (t("desk-bx-intro")) }
                    div #bx-browse.bx-browse hidden {
                        div.grid-2 {
                            label { span { (t("desk-bx-org")) } select #bx-org {} }
                            label {
                                span { (t("desk-bx-collection")) " " span.hint { (t("desk-bx-optional-filter")) } }
                                select #bx-collection { option value="" { (t("desk-bx-all")) } }
                            }
                        }
                        div.bx-toolbar {
                            label.bx-filter {
                                span { (t("desk-bx-show")) }
                                select #bx-qa-filter {
                                    option value="all" { (t("desk-bx-all")) }
                                    option value="reviewed" { (t("desk-bx-reviewed")) }
                                    option value="unreviewed" { (t("desk-bx-unreviewed")) }
                                }
                            }
                            label.bx-check {
                                input type="checkbox" #bx-hide-imported;
                                span { (t("desk-hide-imported")) }
                            }
                            button.btn.ghost type="button" #bx-refresh { (t("desk-refresh")) }
                        }
                    }
                    div #bx-items.bx-items {}
                    fieldset.bx-mode {
                        legend { (t("desk-on-import")) }
                        label { input type="radio" name="bx-mode" value="download" checked; span { (t("desk-mode-download")) " " span.hint { (t("desk-mode-download-hint")) } } }
                        label { input type="radio" name="bx-mode" value="stream"; span { (t("desk-mode-stream")) " " span.hint { (t("desk-mode-stream-hint")) } } }
                    }
                }
            }
            @if imports.archive_it {
                div.src-panel #src-ait {
                    p.muted { (t("desk-ait-intro")) }
                    div #ait-browse.bx-browse hidden {
                        label { span { (t("desk-ait-collection")) } select #ait-collection {} }
                        div.bx-toolbar {
                            label.bx-check {
                                input type="checkbox" #ait-hide-imported;
                                span { (t("desk-hide-imported")) }
                            }
                            button.btn.ghost type="button" #ait-refresh { (t("desk-refresh")) }
                        }
                    }
                    div #ait-crawls.bx-items {}
                }
            }

            div.form-actions {
                button.btn type="submit" { (t("desk-submit")) }
                a.cancel href="/" { (t("action-cancel")) }
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
    layout(Some(&t("desk-heading")), chrome, Some(&search), body)
}
