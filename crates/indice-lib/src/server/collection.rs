//! Collection routes: the finding-aid page, its annotations index, and the
//! machine-readable replay manifest + page list wabac.js consumes.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::annotations::{self};
use crate::collections::{Manifest, Wacz};
use crate::i18n::FluentArg;
use crate::search::PagesScope;
use crate::views;

use super::*;

pub(super) async fn collection_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let l10n = super::i18n::negotiate_headers(&state, &headers);
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(&l10n, e).into_response(),
    };
    let Some(c) = manifest.collection_by_id(&id) else {
        return not_found(&l10n, "error-collection-not-found");
    };
    let members: Vec<&Wacz> = manifest.members_of(&id).collect();

    // Aggregates derived from members.
    let total_size: u64 = members.iter().map(|w| w.file_size).sum();
    let software = collection_software(&members);
    let chrome = chrome(&state, &headers);
    let l10n = &chrome.l10n;
    let range = members_capture_range(l10n, &members);

    let mut meta = Vec::new();
    if let Some(cur) = &c.curator {
        meta.push(views::MetaRow::new(&l10n.tr("meta-curator"), cur.clone()));
    }
    meta.push(views::MetaRow::new(
        &l10n.tr("meta-crawls"),
        l10n.number(members.len() as u64),
    ));
    meta.push(views::MetaRow::new(
        &l10n.tr("meta-size"),
        l10n.size(total_size),
    ));
    if !software.is_empty() {
        meta.push(views::MetaRow::new(
            &l10n.tr("meta-software"),
            software.join(", "),
        ));
    }
    if let Some(r) = &range {
        meta.push(views::MetaRow::new(
            &l10n.tr("meta-capture-dates"),
            r.clone(),
        ));
    }
    if let Some(q) = capture_quality(l10n, &merged_status_counts(&members)) {
        meta.push(views::MetaRow::new(&l10n.tr("meta-capture-quality"), q));
    }
    meta.push(views::MetaRow::new(
        &l10n.tr("meta-created"),
        day_in_words(l10n, &c.created),
    ));
    // Custody, resolved to a display name through the roster — this page is
    // public, so the stored SubjectId must not be printed.
    if let Some(subject) = &c.created_by {
        let name = state
            .users
            .current()
            .resolve(subject.clone())
            .display_name()
            .to_string();
        meta.push(views::MetaRow::new(&l10n.tr("meta-created-by"), name));
    }

    // Newest first. A crawl with no recorded date sorts last.
    let mut newest_first = members.clone();
    newest_first.sort_by(|a, b| b.capture_start.cmp(&a.capture_start));
    let member_items: Vec<views::MemberItem> = newest_first
        .iter()
        .map(|w| views::MemberItem {
            id: w.id.clone(),
            name: w.display_name_in(l10n),
            when: w.capture_start.as_deref().and_then(|ts| l10n.long_date(ts)),
            present: w.is_present(&state.home),
            remote: w.source.is_remote(),
            provenance: provenance_summary(l10n, w),
            thumb: thumb_href(&state.home, &state.index_dir, &w.collection, &w.id),
        })
        .collect();

    // Scoped facet overview: what's *in* this collection, each value a search
    // scoped to it. Turns the page into a faceted entry point, not just a list.
    let scope_id = id.clone();
    let overview = state
        .read_index(move |s| {
            s.facet_overview_scoped(crate::search::FacetScope::Collection(&scope_id))
        })
        .await
        .unwrap_or_default();
    let facets = scoped_facet_sections(l10n, &overview.facets, &format!("collection:{id}"));

    // Deaccession is an admin act; don't offer a curator a button that 403s.
    let can_delete =
        resolve_caller(&state, &headers).is_some_and(|(p, _)| p.role().can_administer());
    // Built before the page takes `chrome`, whose language it is worded in.
    let drafted_note = c.drafted.as_ref().map(|d| {
        // Reviewed by whoever last saved the finding aid, resolved through
        // the roster: this page is public, so never the stored identity.
        let reviewer = c.updated_by.as_ref().map(|s| {
            state
                .users
                .current()
                .resolve(s.clone())
                .display_name()
                .to_string()
        });
        drafted_note(l10n, d, reviewer.as_deref())
    });
    let page = views::CollectionPage {
        name: c.name.clone(),
        description: c.description.clone(),
        narrative: c.narrative.as_deref().map(crate::markdown::render),
        creator: c.creator.clone(),
        dates: c.dates.clone(),
        rights: c.rights.clone(),
        subjects: c.subjects.clone(),
        meta,
        facets,
        members: member_items,
        // From the index, like the search timeline, so each bar counts what
        // its `month:` link finds.
        crawl_months: month_axis(
            l10n,
            &overview
                .months
                .iter()
                .map(|t| (t.ym, t.count))
                .collect::<Vec<_>>(),
            |ym| {
                format!(
                    "/search?q={}",
                    url_encode(&format!("collection:{id} month:{ym}"))
                )
            },
            |_| false,
        ),
        replay_href: collection_replay_href(&id, collection_default_page(&members)),
        id: id.clone(),
        can_delete,
        chrome,
        annotation_count: annotations::load(&state.home, &c.id)
            .map(|v| v.len())
            .unwrap_or(0),
        drafted_note,
    };
    views::collection(&page).into_response()
}

/// "Description and subjects drafted with M (S), reviewed by R."
pub(super) fn drafted_note(
    l10n: &crate::i18n::L10n,
    d: &crate::collections::Drafted,
    reviewer: Option<&str>,
) -> String {
    let names: Vec<String> = d
        .fields
        .iter()
        .map(|f| {
            let key = format!("draft-field-{f}");
            // A field this catalog has no word for is shown as recorded.
            if l10n.has_message(&key) {
                l10n.tr(&key)
            } else {
                f.to_string()
            }
        })
        .collect();
    let list = if names.is_empty() {
        l10n.tr("draft-fields-unknown")
    } else {
        list_and(l10n, &names)
    };
    // For a message that opens with the list: the same words, first letter up.
    let mut capitalized = list.chars();
    let capitalized: String = match capitalized.next() {
        Some(first) => first.to_uppercase().chain(capitalized).collect(),
        None => String::new(),
    };
    let service = if d.service.is_empty() {
        String::new()
    } else {
        format!(" ({})", d.service)
    };
    let mut args = vec![
        ("fields", FluentArg::Str(&list)),
        ("Fields", FluentArg::Str(&capitalized)),
        ("model", FluentArg::Str(&d.model)),
        ("service", FluentArg::Str(&service)),
    ];
    match reviewer {
        Some(r) => {
            args.push(("reviewer", FluentArg::Str(r)));
            l10n.tr_args("drafted-note-reviewed", &args)
        }
        // Saved without a known editor (`collection draft --apply` from the
        // command line): claim no review that may not have happened.
        None => l10n.tr_args("drafted-note", &args),
    }
}

/// GET /collection/{id}/annotations — a public browse of every annotation in the
/// collection, each linking into the collection replay (where it re-anchors).
pub(super) async fn collection_annotations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let l10n = super::i18n::negotiate_headers(&state, &headers);
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(&l10n, e),
    };
    let Some(c) = manifest.collection_by_id(&id) else {
        return not_found(&l10n, "error-collection-not-found");
    };
    let chrome = chrome(&state, &headers);
    // Reads and parses the whole store, so off the async workers.
    let anns = {
        let (home, cid) = (state.home.clone(), c.id.clone());
        tokio::task::spawn_blocking(move || annotations::load(&home, &cid))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    };
    let items = anns.iter().map(|a| anno_link(a, &id)).collect();
    let page = views::AnnotationsIndexPage {
        collection_name: c.name.clone(),
        collection_id: id.clone(),
        items,
        chrome,
    };
    views::annotations_index(&page).into_response()
}

/// A note as the public pages show it, in collection `collection`.
fn anno_link(a: &annotations::Annotation, collection: &str) -> views::AnnoLink {
    let region = a.target.selector.as_ref().map(|s| match s {
        annotations::Selector::TextQuoteSelector { exact, .. } => exact.clone(),
    });
    views::AnnoLink {
        // These pages are public, so the sanitized name — never the stored
        // login identity (see `Creator::public_name`).
        author: a.creator.public_name().unwrap_or("anonymous").to_string(),
        date: a
            .modified
            .clone()
            .unwrap_or_else(|| a.created.clone())
            .chars()
            .take(10)
            .collect(),
        note_html: crate::markdown::render(&a.body.value),
        page_url: a.target.source.clone(),
        replay_href: note_replay_href(collection, &a.target.timestamp, &a.target.source, &a.id)
            // A page replay can't open: the collection instead.
            .unwrap_or_else(|| format!("/collection/{}", url_encode(collection))),
        permalink: annotation_path(collection, &a.id),
        key: annotations::key(&a.id).to_string(),
        region,
    }
}

/// A note's page in collection replay, naming the note (`id`) so the viewer
/// opens on it. `None` for a page replay can't open. The one place the
/// viewer's `#note=` form is written: the Annotations page, a note's page and
/// search results all link this way.
pub(super) fn note_replay_href(collection: &str, ts: &str, url: &str, id: &str) -> Option<String> {
    replay_href(ReplayScope::Collection(collection), ts, url)
        .map(|href| format!("{href}#note={}", url_encode(id)))
}

/// A note's own address, `/collection/{collection}/annotations/{key}`, or
/// `None` for a note whose id indice did not mint (a hand-edited store, say),
/// which that address could not find: no link is better than a dead one.
pub(super) fn annotation_path(collection: &str, id: &str) -> Option<String> {
    let key = annotations::key(id);
    annotations::id_for_key(key)?;
    Some(format!(
        "/collection/{}/annotations/{key}",
        url_encode(collection)
    ))
}

/// The media type of an annotation as the Web Annotation Protocol serves it.
const ANNO_MEDIA_TYPE: &str = r#"application/ld+json; profile="http://www.w3.org/ns/anno.jsonld""#;

/// Whether a request asks for an annotation as JSON-LD rather than as a page:
/// it ranks JSON-LD or JSON above HTML, by the q-values in `Accept`. A tie,
/// a wildcard (`*/*` from curl, beside `text/html` from a browser) or no
/// `Accept` at all is the page; `q=0` refuses a type.
pub(super) fn wants_json_ld(headers: &HeaderMap) -> bool {
    let accept = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let (mut json, mut html) = (0.0_f32, 0.0_f32);
    for range in accept.split(',') {
        let mut parts = range.split(';');
        let media = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
        let q = parts
            .filter_map(|p| p.trim().strip_prefix("q="))
            .find_map(|q| q.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        match media.as_str() {
            "application/ld+json" | "application/json" => json = json.max(q),
            "text/html" | "application/xhtml+xml" => html = html.max(q),
            _ => {}
        }
    }
    json > 0.0 && json > html
}

/// A note as a W3C Web Annotation, for anyone to read: built field by field
/// rather than serialized from the stored record, whose `creator.id` may be a
/// login address. Its `id` is its URL under `origin`. The capture it is on is
/// a `TimeState`: when the page was captured, and where its archived copy
/// replays.
fn annotation_json_ld(
    a: &annotations::Annotation,
    collection: &str,
    origin: &str,
) -> serde_json::Value {
    let mut state = serde_json::json!({ "type": "TimeState" });
    if let Some(date) = ts_iso(&a.target.timestamp) {
        state["sourceDate"] = date.into();
    }
    if let Some(href) = note_replay_href(collection, &a.target.timestamp, &a.target.source, &a.id) {
        // The fragment names the note to the viewer; the copy is the page.
        let page = href.split('#').next().unwrap_or(&href);
        state["cached"] = format!("{origin}{page}").into();
    }
    let mut target = serde_json::json!({ "source": a.target.source, "state": state });
    if let Some(s) = &a.target.selector {
        target["selector"] = serde_json::to_value(s).unwrap_or_default();
    }
    let mut ld = serde_json::json!({
        "@context": annotations::ANNO_CONTEXT,
        "type": "Annotation",
        "created": a.created,
        "body": {
            "type": "TextualBody",
            "format": a.body.format,
            "value": a.body.value,
        },
        "target": target,
    });
    // Every note served has a path, since it was found by it.
    if let Some(path) = annotation_path(collection, &a.id) {
        ld["id"] = format!("{origin}{path}").into();
    }
    if let Some(m) = &a.modified {
        ld["modified"] = m.clone().into();
    }
    if let Some(name) = a.creator.public_name() {
        ld["creator"] = serde_json::json!({ "type": "Person", "name": name });
    }
    ld
}

/// What a note's JSON-LD is also served at, after its URL: the same document
/// without content negotiation, for a link or a browser.
const JSON_LD_SUFFIX: &str = ".jsonld";

/// GET /collection/{id}/annotations/{key} — one note, public: a page for a
/// browser, or the note as a W3C Web Annotation in JSON-LD for a request that
/// asks for it. Its JSON-LD names this URL as its `id`, and the page carries
/// the same JSON-LD, so a reader of either finds the other.
///
/// GET /collection/{id}/annotations/{key}.jsonld — the JSON-LD, whatever the
/// request asks for. The note's id stays the URL without the suffix: this is
/// one way to fetch the note, not another name for it.
pub(super) async fn collection_annotation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path((id, key)): axum::extract::Path<(String, String)>,
) -> Response {
    let (key, json_ld_url) = match key.strip_suffix(JSON_LD_SUFFIX) {
        Some(k) => (k.to_string(), true),
        None => (key, false),
    };
    let not_found = || (StatusCode::NOT_FOUND, "annotation not found").into_response();
    let Some(note_id) = annotations::id_for_key(&key) else {
        return not_found();
    };
    // The manifest and the whole store are read and parsed for one note, so
    // off the async workers: note pages are open to crawlers.
    let found = {
        let (state, id) = (state.clone(), id.clone());
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let manifest = Manifest::open(&state.index_dir)?;
            let Some(c) = manifest.collection_by_id(&id) else {
                return Ok(None);
            };
            Ok(annotations::get(&state.home, &c.id, &note_id)?.map(|a| (c.name.clone(), a)))
        })
        .await
    };
    let (collection_name, a) = match found {
        Ok(Ok(Some(found))) => found,
        Ok(Ok(None)) => return not_found(),
        Ok(Err(e)) => return error_response(&L10n::default(), e),
        Err(e) => {
            return error_response(
                &L10n::default(),
                anyhow::anyhow!("annotation task panicked: {e}"),
            )
        }
    };
    let ld = annotation_json_ld(&a, &id, &state.origin);
    // Public, and free of anything that identifies a login, so any site's
    // annotation client may read it, as any site may read a public WACZ; on a
    // server only, as for the WACZ (`AppState::public_cors`).
    let mut ld_headers = HeaderMap::new();
    ld_headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(ANNO_MEDIA_TYPE),
    );
    if let Some(origin) = state.public_cors() {
        ld_headers.insert(
            axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
            axum::http::HeaderValue::from_static(origin),
        );
    }
    if json_ld_url {
        return (ld_headers, ld.to_string()).into_response();
    }
    // One URL, two representations: a cache must keep them apart.
    let vary = [(axum::http::header::VARY, "Accept")];
    if wants_json_ld(&headers) {
        return (vary, ld_headers, ld.to_string()).into_response();
    }
    let Some(path) = annotation_path(&id, &a.id) else {
        return not_found();
    };
    let json_ld_href = format!("{path}{JSON_LD_SUFFIX}");
    // Where the JSON-LD is, for a client that reads headers and not pages.
    let link = [(
        axum::http::header::LINK,
        format!(r#"<{json_ld_href}>; rel="alternate"; type="application/ld+json""#),
    )];
    let page = views::AnnotationPage {
        json_ld_href,
        collection_name,
        collection_id: id.clone(),
        note: anno_link(&a, &id),
        // In a `<script>`, so `<` is escaped and no note can close it.
        json_ld: ld.to_string().replace('<', "\\u003c"),
        chrome: chrome(&state, &headers),
    };
    (vary, link, views::annotation_page(&page)).into_response()
}

/// A wabac (ReplayWeb.page) multi-WACZ collection manifest for a collection: the
/// JSON that `<replay-web-page source="…/replay.json">` loads to replay every
/// member crawl as one collection (`replay_manifest`). A streamed Browsertrix
/// member's path is its presigned URL when one is cached, and every other
/// member's is its `/files/{id}`: the collection may span many Browsertrix
/// items, a lookup each, so it resolves none (`presigned::cached_urls`).
///
pub(super) async fn collection_replay_json(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    // The replay manifest is read by wabac, not a person: English, like the
    // other documented read endpoints.
    let en = L10n::default();
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(&en, e),
    };
    let Some(c) = manifest.collection_by_id(&id) else {
        return (StatusCode::NOT_FOUND, "collection not found").into_response();
    };
    let files: Vec<(String, &Wacz)> = manifest
        .members_of(&id)
        .map(|w| (w.id.clone(), w))
        .collect();
    let waczs: Vec<&Wacz> = files.iter().map(|(_, w)| *w).collect();
    replay_manifest(
        &files,
        &super::presigned::cached_urls(&state.presigned, &waczs),
        format!("/collection/{}/pages", url_encode(&c.id)),
        &c.name,
        c.description.as_deref(),
    )
}

/// A wabac multi-WACZ manifest of `files`, for a collection's replay or for
/// one crawl written into several WACZs (`crawl_replay_json`).
///
/// Each file is listed under the name paired with it, which the pages
/// endpoint at `pages_url` returns as `filename`, so wabac can match a
/// looked-up page to its member: a collection names a file by its WACZ id, a
/// crawl by its place in the crawl (`crawl_file_names`). `hash` carries the
/// `sha256:` prefix wabac expects.
///
/// A streamed Browsertrix file's `path` is its URL in `presigned`, by source
/// location, so wabac reads its bytes directly instead of through
/// `/files/{id}`, which would add a redirect to every one of a page's hundreds
/// of range reads. A file not in `presigned` goes by `/files/{id}`. A
/// presigned URL expires, so the manifest is not to be cached: each visit
/// fetches it again, and wabac takes the fresh URLs from it.
pub(super) fn replay_manifest(
    files: &[(String, &Wacz)],
    presigned: &HashMap<String, String>,
    pages_url: String,
    title: &str,
    desc: Option<&str>,
) -> Response {
    let resources: Vec<serde_json::Value> = files
        .iter()
        .map(|(name, w)| {
            let path = presigned
                .get(&w.source.location())
                .cloned()
                .unwrap_or_else(|| viewer_source(w));
            let mut res = serde_json::json!({
                "name": name,
                "path": path,
                "crawlId": name,
            });
            // The member hash. wabac uses it as the member's identity, so every
            // member MUST have a distinct one (or none) — giving several members
            // the same hash collapses them in wabac's loader and breaks
            // multi-WACZ replay ("Archived Page Not Found", no member requests).
            //   - Local/downloaded WACZ: our computed whole-file sha256.
            //   - Streamed remote WACZ: no locally-computed sha256, but a
            //     Browsertrix import kept the file hash from its replay.json
            //     (already `sha256:…`) — use it, so streamed members keep
            //     distinct, real, verifiable hashes.
            //   - Otherwise (e.g. a plain remote URL): omit it; wabac then treats
            //     the member as unverified. See rustyweb-streamed-wacz-fixity-zle5.
            let hash = if !w.sha256.is_empty() {
                Some(format!("sha256:{}", w.sha256))
            } else {
                w.browsertrix
                    .as_ref()
                    .map(|b| b.resource_hash.trim())
                    .filter(|h| !h.is_empty())
                    // Normalize to the `algo:hash` form wabac expects. Browsertrix
                    // hashes are stored bare (64-hex sha256); older/test data may
                    // already carry a `sha256:` prefix.
                    .map(|h| {
                        if h.contains(':') {
                            h.to_string()
                        } else {
                            format!("sha256:{h}")
                        }
                    })
            };
            if let Some(h) = hash {
                res["hash"] = serde_json::Value::String(h);
            }
            res
        })
        .collect();
    if resources.is_empty() {
        return (StatusCode::NOT_FOUND, "nothing to replay").into_response();
    }
    let body = serde_json::json!({
        "resources": resources,
        // Where wabac asks which member holds a page (`collection_pages`).
        // Without it, wabac opens and indexes every member WACZ before it can
        // show any page: for a collection of streamed crawls, hundreds of
        // remote reads. With it, wabac opens only the member that holds the
        // page being read. Top-level, not under `metadata`, because that is
        // where wabac's `loadFromJSON` reads it. Relative, so it resolves on
        // whichever origin serves replay.
        "pagesQueryUrl": pages_url,
        "metadata": {
            "title": title,
            "desc": desc,
        },
    });
    (
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        axum::Json(body),
    )
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct PagesParams {
    /// Exact-URL resolution (wabac's on-demand URL→WACZ lookup).
    url: Option<String>,
    /// Free-text page-list search (the viewer's Pages sidebar search box).
    search: Option<String>,
    /// 1-based page number.
    page: Option<usize>,
    #[serde(rename = "pageSize")]
    page_size: Option<usize>,
}

/// wabac `pagesQueryUrl` endpoint for a collection: the page list / search and
/// on-demand URL→WACZ resolution that back multi-WACZ replay, answered from the
/// Tantivy index. Response shape is wabac's: `{ total, items: [{ url, ts, title,
/// filename }] }`, where `filename` is the member WACZ id (== the manifest's
/// `resources[].name`). `ts` is emitted as ISO 8601 so wabac's `new Date(ts)`
/// parses it (the index stores a 14-digit timestamp).
pub(super) async fn collection_pages(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(params): Query<PagesParams>,
) -> Response {
    pages_response(&state, PagesFor::Collection(id), params).await
}

/// What a pages endpoint answers for: [`crate::search::PagesScope`], owned, so
/// it can move to the thread that reads the index.
pub(super) enum PagesFor {
    Collection(String),
    /// Each file's WACZ id, and the name its crawl's manifest lists it under.
    Crawls(Vec<(String, String)>),
}

/// The body of a wabac `pagesQueryUrl` endpoint, for a collection's replay or
/// for one crawl's (`crawl_pages`).
pub(super) async fn pages_response(
    state: &AppState,
    scope: PagesFor,
    params: PagesParams,
) -> Response {
    if let Some(refusal) = params
        .search
        .as_deref()
        .and_then(|q| query_too_long(&L10n::default(), q))
    {
        return refusal;
    }
    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(25).clamp(1, 200);
    // Saturating, so an absurd `page` can't overflow; the index caps how
    // deep it will go (`MAX_PAGES_OFFSET`).
    let offset = (page - 1).saturating_mul(page_size);
    let PagesParams { url, search, .. } = params;
    // What each hit's file is called in the manifest: its id, unless renamed.
    let names: HashMap<String, String> = match &scope {
        PagesFor::Collection(_) => HashMap::new(),
        PagesFor::Crawls(files) => files.iter().cloned().collect(),
    };
    match state
        .read_index(move |s| match &scope {
            PagesFor::Collection(id) => s.pages(
                PagesScope::Collection(id),
                url.as_deref(),
                search.as_deref(),
                offset,
                page_size,
            ),
            PagesFor::Crawls(files) => {
                let ids: Vec<String> = files.iter().map(|(id, _)| id.clone()).collect();
                s.pages(
                    PagesScope::Crawls(&ids),
                    url.as_deref(),
                    search.as_deref(),
                    offset,
                    page_size,
                )
            }
        })
        .await
    {
        Ok((total, hits)) => {
            let items: Vec<serde_json::Value> = hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "url": h.url,
                        "ts": ts_to_iso(&h.timestamp),
                        "title": h.title,
                        "filename": names.get(&h.crawl_id).unwrap_or(&h.crawl_id),
                    })
                })
                .collect();
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({ "total": total, "items": items })),
            )
                .into_response()
        }
        Err(e) => error_response(&L10n::default(), e),
    }
}

/// Build the scoped facet sections for a detail page. Each dimension becomes a
/// labeled group whose links run a search within `scope` (e.g. `collection:slug`)
/// further filtered by that value. The Collection dimension is skipped (moot on a
/// scoped page), and empty dimensions are dropped.
pub(super) fn scoped_facet_sections(
    l10n: &crate::i18n::L10n,
    overview: &[crate::search::FacetGroup],
    scope: &str,
) -> Vec<views::FacetSection> {
    // (facet field == filter field, heading message, sort by value desc, max shown)
    const DIMS: [(&str, &str, bool, usize); 4] = [
        ("site", "facet-top-sites", false, 10),
        ("year", "facet-by-year", true, 12),
        ("type", "facet-types", false, 6),
        ("lang", "facet-languages", false, 8),
    ];
    DIMS.iter()
        .filter_map(|(field, label, by_value_desc, max)| {
            let group = overview.iter().find(|g| g.field == *field)?;
            let mut buckets: Vec<&crate::search::FacetBucket> = group.buckets.iter().collect();
            if buckets.is_empty() {
                return None;
            }
            if *by_value_desc {
                buckets.sort_by(|a, b| b.value.cmp(&a.value));
            }
            let links = buckets
                .into_iter()
                .take(*max)
                .map(|b| views::BrowseLink {
                    label: b.value.clone(),
                    count: b.count,
                    href: format!(
                        "/search?q={}",
                        url_encode(&format!("{scope} {field}:{}", b.value))
                    ),
                })
                .collect();
            Some(views::FacetSection {
                label: l10n.tr(label),
                links,
            })
        })
        .collect()
}
