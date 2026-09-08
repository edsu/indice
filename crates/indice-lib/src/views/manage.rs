//! Management UI (`serve --manage`): the finding-aid form and the accession
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
    layout(
        "Add crawls - indice",
        true,
        signed_in,
        false,
        Some(&search),
        body,
    )
}
