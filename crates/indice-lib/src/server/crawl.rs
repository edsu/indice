//! The crawl detail page: one WACZ's provenance, fixity, and captures; and
//! the replay manifest and page list for a crawl written into several WACZs.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::collections::{Manifest, Wacz};
use crate::views;

use super::*;

pub(super) async fn crawl_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e).into_response(),
    };
    let Some(c) = manifest.wacz_by_id(&id) else {
        return (StatusCode::NOT_FOUND, "Crawl not found").into_response();
    };

    let display_name = c.display_name();
    // Breadcrumb for the containing collection (name + id).
    let col = manifest.collection_by_id(&c.collection);
    let crumb = col.map(|col| (col.id.to_string(), col.name.clone()));

    // Each seed page replays at its own address; the Replay button opens the
    // first one.
    let scope = ReplayScope::Crawl(&c.id);
    let pages: Vec<views::PageItem> = c
        .seed_pages
        .iter()
        .filter_map(|p| {
            Some(views::PageItem {
                href: replay_href(scope, &p.ts, &p.url)?,
                title: p.title.clone().unwrap_or_else(|| p.url.clone()),
                url: p.url.clone(),
            })
        })
        .collect();
    let replay = pages.first().map(|p| p.href.clone());

    // Provenance panel: how this crawl was produced. Only rows with data show.
    let mut provenance = Vec::new();
    if let Some(bt) = &c.browsertrix {
        // Attribution for content pulled in via `indice import browsertrix`.
        let host = bt
            .host
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        provenance.push(views::MetaRow::new(
            "Source",
            format!("Browsertrix ({host})"),
        ));
        if let Some(rating) = bt.review_status {
            provenance.push(views::MetaRow::new("Review", review_label(rating)));
        }
        if !bt.item_id.is_empty() {
            provenance.push(views::MetaRow::mono("Browsertrix item", bt.item_id.clone()));
        }
    }
    if let Some(ait) = &c.archive_it {
        // Attribution for content pulled in via `indice import archive-it`.
        let host = ait
            .host
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        provenance.push(views::MetaRow::new(
            "Source",
            format!("Archive-It ({host})"),
        ));
        if !ait.collection_title.is_empty() {
            provenance.push(views::MetaRow::new(
                "Collection",
                ait.collection_title.clone(),
            ));
        }
        if ait.crawl_id != 0 {
            provenance.push(views::MetaRow::mono("Crawl", ait.crawl_id.to_string()));
        }
        if ait.warc_count != 0 {
            provenance.push(views::MetaRow::new(
                "WARC files",
                ait.warc_count.to_string(),
            ));
        }
    }
    if !c.software.is_empty() {
        provenance.push(views::MetaRow::new("Software", c.software.join(", ")));
    }
    if let Some(op) = &c.operator {
        provenance.push(views::MetaRow::new("Operator", op.clone()));
    }
    if let Some(ua) = &c.user_agent {
        provenance.push(views::MetaRow::mono("User-Agent", ua.clone()));
    }
    if let Some(rb) = &c.robots {
        provenance.push(views::MetaRow::new("Robots", rb.clone()));
    }
    if let Some(h) = &c.hostname {
        provenance.push(views::MetaRow::mono("Crawl host", h.clone()));
    }
    if let Some(p) = &c.is_part_of {
        provenance.push(views::MetaRow::new("Part of", p.clone()));
    }
    if let Some(ct) = &c.conforms_to {
        provenance.push(views::MetaRow::mono("Conforms to", ct.clone()));
    }
    if !c.keywords.is_empty() {
        provenance.push(views::MetaRow::new("Keywords", c.keywords.join(", ")));
    }
    if !c.licenses.is_empty() {
        provenance.push(views::MetaRow::new("License", c.licenses.join(", ")));
    }
    if let Some(n) = c.nested_waczs {
        provenance.push(views::MetaRow::new(
            "Multi-WACZ",
            format!(
                "{n} crawl{} bundled in one file",
                if n == 1 { "" } else { "s" }
            ),
        ));
    }
    if let Some(n) = c.page_count {
        provenance.push(views::MetaRow::new("Pages", n.to_string()));
    }
    if let Some(q) = capture_quality(&c.status_counts) {
        provenance.push(views::MetaRow::new("Capture quality", q));
    }
    if let Some(range) = capture_range(c) {
        provenance.push(views::MetaRow::new("Capture dates", range));
    }
    if let Some(m) = &c.modified {
        let m = m.get(..10).unwrap_or(m);
        provenance.push(views::MetaRow::new("WACZ modified", m.to_string()));
    }
    // Custody. This page is public, so resolve the stored SubjectId to a
    // display name through the roster rather than printing the identity.
    if let Some(subject) = &c.added_by {
        let name = state
            .users
            .current()
            .resolve(subject.clone())
            .display_name()
            .to_string();
        provenance.push(views::MetaRow::new("Added by", name));
    }

    let chrome = chrome(&state, &headers);
    // Match the handler's rule exactly: an admin may remove any crawl, a
    // curator only one they accessioned.
    let scope_id = id.clone();
    let crawl_overview = state
        .read_index(move |s| s.facet_overview_scoped(crate::search::FacetScope::Crawl(&scope_id)))
        .await
        .unwrap_or_default();
    let added_by = c.added_by.as_ref().map(|s| s.as_str());
    let can_delete =
        resolve_caller(&state, &headers).is_some_and(|(p, _)| p.may_delete_crawl(added_by));
    let page = views::CrawlPage {
        id: id.clone(),
        crumb,
        original_name: (display_name != c.name).then(|| c.name.clone()),
        name: display_name,
        description: c.description.clone(),
        note: crate::collections::read_crawl_note(&state.home, &c.collection, &id)
            .map(|n| crate::markdown::render(&n)),
        thumb: thumb_href(&state.home, &state.index_dir, &c.collection, &id),
        replay_href: replay,
        // Fetched from a remote host at replay time, not stored in <home>/archive.
        remote: c.source.is_remote(),
        provenance,
        source: c.source.describe(),
        size: human_size(c.file_size),
        sha_short: c.sha256.get(..16).unwrap_or(&c.sha256).to_string(),
        sha_full: c.sha256.clone(),
        crawled: c
            .crawl_date
            .as_deref()
            .map(|d| d.get(..10).unwrap_or(d).to_string()),
        indexed: c
            .date_indexed
            .get(..10)
            .unwrap_or(&c.date_indexed)
            .to_string(),
        present: c.is_present(&state.home),
        facets: scoped_facet_sections(&crawl_overview.facets, &format!("crawl:{id}")),
        pages,
        can_delete,
        chrome,
    };

    views::crawl(&page).into_response()
}

/// `GET /crawl/{id}/replay.json`: a wabac multi-WACZ manifest of every WACZ
/// the crawl was written into (`Manifest::crawl_files`), for replaying a page
/// of a Browsertrix crawl whose stylesheets and scripts sit in another of its
/// files. A crawl in one WACZ replays its `/files/{id}` directly instead
/// (`crawl_replay_source`), but is answered here too.
pub(super) async fn crawl_replay_json(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e),
    };
    let Some(w) = manifest.wacz_by_id(&id) else {
        return (StatusCode::NOT_FOUND, "crawl not found").into_response();
    };
    replay_manifest(
        &crawl_file_names(&manifest.crawl_files(w)),
        format!("/crawl/{}/pages", url_encode(&w.id)),
        &w.display_name(),
        w.description.as_deref(),
    )
}

/// `GET /crawl/{id}/pages`: where wabac asks which of a crawl's WACZs holds a
/// page, for `crawl_replay_json`. Answers only for the crawl's own files.
pub(super) async fn crawl_pages(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(params): Query<PagesParams>,
) -> Response {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e),
    };
    let Some(w) = manifest.wacz_by_id(&id) else {
        return (StatusCode::NOT_FOUND, "crawl not found").into_response();
    };
    let files = crawl_file_names(&manifest.crawl_files(w))
        .into_iter()
        .map(|(name, f)| (f.id.clone(), name))
        .collect();
    pages_response(&state, PagesFor::Crawls(files), params).await
}

/// The names a crawl's files go by in its manifest: each file's place in
/// `crawl_files`, zero-padded, then its id (`00-6bdd2cb1`, `01-…`).
///
/// For a resource that isn't in its page's file, wabac tries the first three
/// files by name, in sorted order, which is how it reads them back from its
/// cache whatever order the manifest gives; and it puts a name that looks like
/// an integer before all the others. A file's bare id is eight hex digits,
/// sometimes all digits. Named by place, the files sort in the crawl's own
/// order, earliest first, so the first three are the crawl's first files,
/// where a crawl fetches the stylesheets and scripts every page shares.
pub(super) fn crawl_file_names<'a>(files: &[&'a Wacz]) -> Vec<(String, &'a Wacz)> {
    let width = files.len().saturating_sub(1).to_string().len();
    files
        .iter()
        .enumerate()
        .map(|(i, w)| (format!("{i:0width$}-{}", w.id), *w))
        .collect()
}

/// What replay loads for a page of crawl `w`: its own `/files/{id}` when the
/// crawl is in that one WACZ, else the crawl's `replay.json`, which lists
/// every file the crawl was written into.
pub(super) fn crawl_replay_source(manifest: &Manifest, w: &Wacz) -> String {
    if manifest.crawl_files(w).len() > 1 {
        format!("/crawl/{}/replay.json", url_encode(&w.id))
    } else {
        viewer_source(w)
    }
}
