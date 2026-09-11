//! The crawl detail page: one WACZ's provenance, fixity, and captures.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use crate::collections::Manifest;
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

    let source_enc = url_encode(&viewer_source(c));
    let name_enc = url_encode(&c.name);
    // Breadcrumb + replay params for the containing collection (name + id).
    let col = manifest.collection_by_id(&c.collection);
    let crumb = col.map(|col| (col.id.to_string(), col.name.clone()));
    let mut coll_q = col
        .map(|col| {
            format!(
                "&collection={}&collection_id={}",
                url_encode(&col.name),
                url_encode(&col.id)
            )
        })
        .unwrap_or_default();
    // The crawl id, so the viewer's crawl crumb links back to this page.
    coll_q.push_str(&format!("&crawl={}", url_encode(&c.id)));

    // Replay button: first seed page, else the collection root.
    let replay_href = match c.seed_pages.first() {
        Some(p) => format!(
            "/replay/viewer?source={source_enc}&url={}&ts={}&name={name_enc}{coll_q}",
            url_encode(&p.url),
            ts_to_14digit(&p.ts),
        ),
        None => format!("/replay/viewer?source={source_enc}&name={name_enc}{coll_q}"),
    };

    let pages: Vec<views::PageItem> = c
        .seed_pages
        .iter()
        .map(|p| views::PageItem {
            href: format!(
                "/replay/viewer?source={source_enc}&url={}&ts={}&name={name_enc}{coll_q}",
                url_encode(&p.url),
                ts_to_14digit(&p.ts),
            ),
            title: p.title.clone().unwrap_or_else(|| p.url.clone()),
            url: p.url.clone(),
        })
        .collect();

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

    let (manage, who) = admin_ctx(&state, &headers);
    let can_login = login_available(&state, &who);
    // Deaccession is an admin act; don't offer a curator a button that 403s.
    let can_delete =
        resolve_caller(&state, &headers).is_some_and(|(p, _)| p.role().can_administer());
    let page = views::CrawlPage {
        id: id.clone(),
        crumb,
        name: c.name.clone(),
        description: c.description.clone(),
        note: crate::collections::read_crawl_note(&state.home, &c.collection, &id)
            .map(|n| crate::markdown::render(&n)),
        thumb: thumb_href(&state.home, &state.index_dir, &c.collection, &id),
        replay_href,
        // Fetched from a remote host at replay time, not stored in <home>/archive.
        remote: c.source.is_remote(),
        provenance,
        source: c.source.location(),
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
        facets: scoped_facet_sections(
            &state
                .search
                .read()
                .unwrap()
                .facet_overview_scoped(crate::search::FacetScope::Crawl(&id))
                .unwrap_or_default(),
            &format!("crawl:{id}"),
        ),
        pages,
        management: manage,
        can_delete,
        signed_in: who,
        can_login,
    };

    views::crawl(&page).into_response()
}
