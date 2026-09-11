//! The JSON APIs: full-text search, and the page-annotations read/write
//! endpoints.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::annotations::{self, EditOutcome, UpdateResult};
use crate::collections::{CollectionId, Manifest};
use crate::identity::SubjectId;

use super::*;

#[derive(Deserialize)]
pub(super) struct SearchParams {
    q: String,
    limit: Option<usize>,
}

pub(super) async fn search_api(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(20).min(200);
    match state
        .search
        .read()
        .unwrap()
        .search_faceted(&params.q, limit, 0)
    {
        Ok(response) => {
            let body = serde_json::json!({
                "total": response.total_hits,
                "capped": response.capped,
                "results": response.results.iter().map(|r| serde_json::json!({
                    "doc_type": r.doc_type,
                    "url": r.url,
                    "domain": r.domain,
                    "timestamp": r.timestamp,
                    "title": r.title,
                    "author": r.author,
                    "crawl_id": r.crawl_id,
                    "crawl_name": r.crawl_name,
                    "collection": r.collection,
                    "snippet": r.snippet,
                    "capture_count": r.capture_count,
                    "status": r.status,
                })).collect::<Vec<_>>(),
                "facets": response.facets.iter().map(|g| serde_json::json!({
                    "field": g.field,
                    "label": g.label,
                    "buckets": g.buckets.iter().map(|b| serde_json::json!({
                        "value": b.value,
                        "count": b.count,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            });
            (StatusCode::OK, axum::Json(body)).into_response()
        }
        Err(e) => error_response(e),
    }
}

/// A `TextQuoteSelector` on the wire (used in both requests and responses).
#[derive(Serialize, Deserialize)]
struct SelectorDto {
    exact: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    suffix: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AnnotationListQuery {
    collection: CollectionId,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    ts: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AnnotationCreateReq {
    collection: CollectionId,
    url: String,
    timestamp: String,
    note: String,
    #[serde(default)]
    selector: Option<SelectorDto>,
}

#[derive(Deserialize)]
pub(super) struct AnnotationUpdateReq {
    collection: CollectionId,
    note: String,
}

#[derive(Deserialize)]
pub(super) struct AnnotationDeleteReq {
    collection: CollectionId,
}

#[derive(Serialize)]
struct AnnotationView {
    id: String,
    created: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    url: String,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    selector: Option<SelectorDto>,
    note_md: String,
    note_html: String,
    /// Whether the current request's author may edit/delete this note.
    editable: bool,
}

#[derive(Serialize)]
struct AnnotationListResp {
    /// Whether the current request may create annotations (signed in / local admin).
    can_annotate: bool,
    annotations: Vec<AnnotationView>,
}

/// Who is writing, as a normalized [`SubjectId`]: the signed-in identity, or the
/// local operator on a loopback `--manage` instance (which has no authentication
/// and so no distinct identities). `None` when the request may not annotate, or
/// when the proxy forwarded an identity we refuse to record.
fn annotation_author(state: &AppState, headers: &HeaderMap) -> Option<SubjectId> {
    let (can, who) = admin_ctx(state, headers);
    if !can {
        return None;
    }
    match who {
        // `parse_remote`, not `parse`: a proxy identity must never resolve to
        // the loopback operator, whose notes it would then inherit.
        Some(raw) => SubjectId::parse_remote(&raw),
        None => Some(SubjectId::local()),
    }
}

fn annotation_view(a: &annotations::Annotation, author: Option<&SubjectId>) -> AnnotationView {
    let selector = a.target.selector.as_ref().map(|s| match s {
        annotations::Selector::TextQuoteSelector {
            exact,
            prefix,
            suffix,
        } => SelectorDto {
            exact: exact.clone(),
            prefix: prefix.clone(),
            suffix: suffix.clone(),
        },
    });
    // `matches` canonicalizes the stored key, so a note written before
    // identities were normalized is still recognized as its author's.
    let editable = author.is_some_and(|s| s.matches(a.creator.id.as_deref()));
    AnnotationView {
        id: a.id.clone(),
        created: a.created.clone(),
        modified: a.modified.clone(),
        // Never the raw stored name: it may be a login address (see
        // `Creator::public_name`), and this endpoint is public.
        author: a.creator.public_name().map(str::to_string),
        url: a.target.source.clone(),
        timestamp: a.target.timestamp.clone(),
        selector,
        note_md: a.body.value.clone(),
        note_html: crate::markdown::render(&a.body.value).0,
        editable,
    }
}

/// GET /api/annotations — public. A capture's notes (with `url` + `ts`), or all
/// notes in the collection when they're omitted.
pub(super) async fn list_annotations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<AnnotationListQuery>,
) -> Response {
    let author = annotation_author(&state, &headers);
    // url and ts pin a capture; require both, or neither (whole collection).
    match (q.url.is_some(), q.ts.is_some()) {
        (true, true) | (false, false) => {}
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "url and ts must be provided together",
            )
                .into_response();
        }
    }
    let st = state.clone();
    let AnnotationListQuery {
        collection,
        url,
        ts,
    } = q;
    let loaded = tokio::task::spawn_blocking(move || match (url.as_deref(), ts.as_deref()) {
        (Some(u), Some(t)) => annotations::list_by_page(&st.home, &collection, u, t),
        _ => annotations::load(&st.home, &collection),
    })
    .await;
    match loaded {
        Ok(Ok(list)) => {
            let views = list
                .iter()
                .map(|a| annotation_view(a, author.as_ref()))
                .collect();
            Json(AnnotationListResp {
                can_annotate: author.is_some(),
                annotations: views,
            })
            .into_response()
        }
        Ok(Err(e)) => error_response(e),
        Err(e) => error_response(anyhow::anyhow!("annotation task panicked: {e}")),
    }
}

/// POST /api/annotations — create (management-gated).
pub(super) async fn create_annotation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<AnnotationCreateReq>,
) -> Response {
    if req.note.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "note is empty").into_response();
    }
    if req.url.trim().is_empty() || req.timestamp.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "url and timestamp are required").into_response();
    }
    // A note must attach to a collection the manifest knows about; otherwise a
    // well-formed but unknown slug would create an orphan collections/<slug>/ dir
    // and an orphan search doc for a collection that isn't listed anywhere.
    match Manifest::open(&state.index_dir) {
        Ok(m) if m.collection_by_id(&req.collection).is_some() => {}
        Ok(_) => return (StatusCode::NOT_FOUND, "collection not found").into_response(),
        Err(e) => return error_response(e),
    }
    // No invented identity: if we can't say who is writing, we don't write.
    // (Unreachable while the route is gated, but it means this handler no longer
    // depends on the middleware for that guarantee.)
    let Some(author) = annotation_author(&state, &headers) else {
        return (StatusCode::FORBIDDEN, "not signed in").into_response();
    };
    // The id is compared and never shown; the name is shown and never compared.
    let creator = annotations::Creator::person(author.as_str(), author.display_name());
    let ann = match req.selector {
        Some(s) => annotations::Annotation::region(
            req.url,
            req.timestamp,
            annotations::Selector::TextQuoteSelector {
                exact: s.exact,
                prefix: s.prefix,
                suffix: s.suffix,
            },
            req.note,
            creator,
        ),
        None => annotations::Annotation::page(req.url, req.timestamp, req.note, creator),
    };
    let view = annotation_view(&ann, Some(&author));
    let st = state.clone();
    let collection = req.collection;
    let saved = tokio::task::spawn_blocking(move || {
        let _guard = st.write_lock.lock().expect("write lock poisoned");
        annotations::create(&st.home, &collection, &ann)?;
        // Keep full-text search in step with the new note, then publish it.
        crate::index::index_annotation_upsert(&st.home, &collection, &ann)?;
        st.reload_searcher()?;
        Ok::<_, anyhow::Error>(())
    })
    .await;
    match saved {
        Ok(Ok(())) => (StatusCode::CREATED, Json(view)).into_response(),
        Ok(Err(e)) => error_response(e),
        Err(e) => error_response(anyhow::anyhow!("annotation task panicked: {e}")),
    }
}

/// POST /api/annotations/{id} — update the note text (author only).
pub(super) async fn update_annotation(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    Json(req): Json<AnnotationUpdateReq>,
) -> Response {
    if req.note.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "note is empty").into_response();
    }
    let Some(author) = annotation_author(&state, &headers) else {
        return (StatusCode::FORBIDDEN, "not signed in").into_response();
    };
    let st = state.clone();
    let AnnotationUpdateReq { collection, note } = req;
    let author_key = author.clone();
    let done = tokio::task::spawn_blocking(move || {
        let _guard = st.write_lock.lock().expect("write lock poisoned");
        let res =
            annotations::update(&st.home, &collection, &id, &note, |k| author_key.matches(k))?;
        // Re-index the edited note (upsert by id) and publish, when it changed.
        if let UpdateResult::Updated(a) = &res {
            crate::index::index_annotation_upsert(&st.home, &collection, a)?;
            st.reload_searcher()?;
        }
        Ok::<_, anyhow::Error>(res)
    })
    .await;
    match done {
        Ok(Ok(UpdateResult::Updated(a))) => {
            Json(annotation_view(&a, Some(&author))).into_response()
        }
        Ok(Ok(UpdateResult::NotFound)) => {
            (StatusCode::NOT_FOUND, "no such annotation").into_response()
        }
        Ok(Ok(UpdateResult::Forbidden)) => {
            (StatusCode::FORBIDDEN, "not your annotation").into_response()
        }
        Ok(Err(e)) => error_response(e),
        Err(e) => error_response(anyhow::anyhow!("annotation task panicked: {e}")),
    }
}

/// POST /api/annotations/{id}/delete — delete (author only).
pub(super) async fn delete_annotation(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    Json(req): Json<AnnotationDeleteReq>,
) -> Response {
    let Some(author) = annotation_author(&state, &headers) else {
        return (StatusCode::FORBIDDEN, "not signed in").into_response();
    };
    let st = state.clone();
    let AnnotationDeleteReq { collection } = req;
    let done = tokio::task::spawn_blocking(move || {
        let _guard = st.write_lock.lock().expect("write lock poisoned");
        let outcome = annotations::delete(&st.home, &collection, &id, |k| author.matches(k))?;
        // Drop the note from search and publish, when it was actually removed.
        if let EditOutcome::Done = outcome {
            crate::index::delete_annotation_from_index(&st.home, &id)?;
            st.reload_searcher()?;
        }
        Ok::<_, anyhow::Error>(outcome)
    })
    .await;
    match done {
        Ok(Ok(EditOutcome::Done)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Ok(EditOutcome::NotFound)) => {
            (StatusCode::NOT_FOUND, "no such annotation").into_response()
        }
        Ok(Ok(EditOutcome::Forbidden)) => {
            (StatusCode::FORBIDDEN, "not your annotation").into_response()
        }
        Ok(Err(e)) => error_response(e),
        Err(e) => error_response(anyhow::anyhow!("annotation task panicked: {e}")),
    }
}
