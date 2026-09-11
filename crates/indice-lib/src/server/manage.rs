//! Management write surface: adding archives (upload / path / URL) with SSE
//! progress, the finding-aid form, the accession desk, and deletes.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio_stream::StreamExt;

use anyhow::Result;
use axum::extract::{Multipart, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Json};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::collections::Manifest;
use crate::events::Action;
use crate::views;

use super::*;
use crate::collections::CollectionId;

/// Progress events streamed to the management UI over SSE while an add-archive
/// job runs. The first six mirror [`crate::index::IndexProgress`]; `done`/`error`
/// are the terminal outcomes. Serialized as a tagged JSON object, e.g.
/// `{"type":"total","total":1234}`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ProgressEvent {
    Begin {
        label: String,
    },
    Phase {
        phase: String,
    },
    Total {
        total: u64,
    },
    Records {
        done: u64,
    },
    WaczIndexed {
        label: String,
        pages: u64,
    },
    Finish,
    /// The whole job succeeded and the searcher was reloaded. `collection` is the
    /// target collection's slug; `crawls` is each indexed crawl as `{id, name}`,
    /// so the UI can link straight to each crawl (or to the collection).
    Done {
        collection: String,
        crawls: Vec<serde_json::Value>,
    },
    /// The job failed; `message` is the (chained) error.
    Error {
        message: String,
    },
}

impl ProgressEvent {
    /// SSE `event:` name, so a browser can `addEventListener` per variant.
    fn name(&self) -> &'static str {
        match self {
            ProgressEvent::Begin { .. } => "begin",
            ProgressEvent::Phase { .. } => "phase",
            ProgressEvent::Total { .. } => "total",
            ProgressEvent::Records { .. } => "records",
            ProgressEvent::WaczIndexed { .. } => "wacz_indexed",
            ProgressEvent::Finish => "finish",
            ProgressEvent::Done { .. } => "done",
            ProgressEvent::Error { .. } => "error",
        }
    }
}

/// An [`IndexProgress`](crate::index::IndexProgress) that forwards each callback
/// into an unbounded channel, so the SSE endpoint can relay it to the browser.
/// The channel is unbounded (and thus buffers) so events emitted before the
/// client connects to the SSE stream are not lost. Sends are non-blocking and
/// ignore a dropped receiver (client disconnected mid-job).
pub(super) struct ChannelProgress {
    pub(super) tx: mpsc::UnboundedSender<ProgressEvent>,
}

impl ChannelProgress {
    fn send(&self, ev: ProgressEvent) {
        let _ = self.tx.send(ev);
    }
}

impl crate::index::IndexProgress for ChannelProgress {
    fn begin(&self, label: &str) {
        self.send(ProgressEvent::Begin {
            label: label.to_string(),
        });
    }
    fn phase(&self, phase: &str) {
        self.send(ProgressEvent::Phase {
            phase: phase.to_string(),
        });
    }
    fn set_total(&self, total: u64) {
        self.send(ProgressEvent::Total { total });
    }
    fn set_records(&self, done: u64) {
        self.send(ProgressEvent::Records { done });
    }
    fn wacz_indexed(&self, label: &str, pages: u64) {
        self.send(ProgressEvent::WaczIndexed {
            label: label.to_string(),
            pages,
        });
    }
    fn finish(&self) {
        self.send(ProgressEvent::Finish);
    }
}

/// Body of `POST /api/archives` — add a crawl by reference. Browser byte-upload
/// uses [`upload_archive`] (`/api/archives/upload`) instead.
#[derive(Deserialize)]
pub(super) struct AddArchiveRequest {
    /// Local filesystem path to a `.wacz` (or an `http(s)://` URL — both are
    /// accepted by `index_location`).
    path: String,
    /// Collection this crawl belongs to; created if it doesn't exist yet.
    collection: String,
    /// Optional display-name override for the collection.
    #[serde(default)]
    name: Option<String>,
}

#[derive(Serialize)]
pub(super) struct AddArchiveResponse {
    /// Id to stream progress from at `/api/archives/{job}/events`.
    pub(super) job: u64,
}

/// Register an ingest job and run `index_location` for it on a blocking thread,
/// returning the job id immediately. Shared by the JSON add ([`add_archive`]) and
/// the multipart upload ([`upload_archive`]). `keepalive` holds a temp dir (the
/// uploaded file) alive until indexing finishes, then drops it — `None` for the
/// path/URL case, which owns no temp file.
/// Acquire the single indexing write lock. If another import already holds it,
/// tell the user via `progress` first (so a queued job doesn't look hung), then
/// block. Tolerates poisoning — the guarded unit carries no state to corrupt, so
/// one panic mid-index must not wedge every future import.
pub(super) fn acquire_write_lock<'a>(
    write_lock: &'a std::sync::Mutex<()>,
    progress: &ChannelProgress,
) -> std::sync::MutexGuard<'a, ()> {
    match write_lock.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            crate::index::IndexProgress::phase(progress, "waiting for another import to finish…");
            write_lock.lock().unwrap_or_else(|e| e.into_inner())
        }
    }
}

/// Start a background ingest job.
///
/// Takes a `&Curator` it never reads. That is the point: `Curator`'s field is
/// private to `auth.rs`, so one cannot be fabricated — a caller must have
/// obtained it from the extractor, i.e. must have passed the check. This makes
/// "I added a handler and forgot to gate it" a compile error rather than a
/// silent exposure. (It will also be where the actor comes from once crawls
/// record who added them — see bead rustyweb-manifest-provenance-sfu7.)
fn start_index_job(
    curator: &Curator,
    state: &Arc<AppState>,
    location: String,
    collection: String,
    name: Option<String>,
    keepalive: Option<tempfile::TempDir>,
) -> u64 {
    let id = state.job_counter.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::unbounded_channel::<ProgressEvent>();
    state.jobs.lock().unwrap().insert(id, rx);

    let job_state = state.clone();
    // Who to credit for whatever this ingest creates.
    let actor = curator.principal().id().clone();
    // `index_location` blocks (file IO, network range reads, the Tantivy commit),
    // so run it off the async runtime — never on a request-handling thread.
    tokio::task::spawn_blocking(move || {
        // Held until the job ends; dropping it deletes any uploaded temp file
        // (after `index_location` has copied it into `archive/`).
        let _keepalive = keepalive;
        let progress = ChannelProgress { tx: tx.clone() };
        let result = {
            // Snapshot, ingest, and attribute all under ONE hold of the write
            // lock. `Manifest::save` rewrites waczs.json wholesale from an
            // in-memory vec, so attribution is a read-modify-write: doing it
            // after releasing the lock let a queued job's ingest land between
            // our open and our save, and our save then erased that job's
            // manifest entry while its documents stayed in Tantivy.
            let _guard = acquire_write_lock(&job_state.write_lock, &progress);
            // One location can yield several crawls (a directory, nested
            // WACZs), so diff against a snapshot rather than guessing.
            let before = crate::index::crawl_ids(&job_state.home);
            let result = crate::index::index_location(
                &location,
                &job_state.home,
                name.as_deref(),
                &collection,
                false, // download
                false, // force
                None,
                Some(&progress),
            );
            // Deliberately not gated on `result`: a multi-WACZ add can fail
            // part way with earlier crawls already committed, and those are
            // exactly the ones their curator needs to be able to undo.
            match before {
                Ok(before) => {
                    let fresh: std::collections::HashSet<String> =
                        crate::index::crawl_ids(&job_state.home)
                            .unwrap_or_default()
                            .difference(&before)
                            .cloned()
                            .collect();
                    // Best-effort: a crawl in the archive without its custody
                    // line is a provenance gap, not a reason to fail the add.
                    if let Err(e) = crate::index::set_added_by(&job_state.home, &fresh, &actor) {
                        tracing::warn!(
                            "recording who added {} crawl(s) failed: {e:#}",
                            fresh.len()
                        );
                    }
                }
                // Fail CLOSED. An unreadable snapshot used to become an empty
                // one, which made every pre-existing unattributed crawl look
                // new and handed this curator ownership of all of them.
                Err(e) => tracing::warn!(
                    "could not read the manifest before indexing, so this add is \
                     recorded without custody: {e:#}"
                ),
            }
            result
        };
        match result {
            Ok(()) => match job_state.reload_searcher() {
                Ok(()) => tx
                    .send(ProgressEvent::Done {
                        collection: crate::collections::slugify(&collection),
                        crawls: Vec::new(),
                    })
                    .ok(),
                Err(e) => tx
                    .send(ProgressEvent::Error {
                        message: format!("indexed, but reloading the searcher failed: {e:#}"),
                    })
                    .ok(),
            },
            Err(e) => tx
                .send(ProgressEvent::Error {
                    message: format!("{e:#}"),
                })
                .ok(),
        };
        // `tx` (and the `progress` clone) drop here → the SSE stream ends once the
        // client has read the terminal event.
    });

    id
}

/// `POST /api/archives` — add a crawl by local path or `http(s)://` URL. Starts
/// an ingest job and returns its id (202 Accepted).
pub(super) async fn add_archive(
    State(state): State<Arc<AppState>>,
    curator: Curator,
    Json(req): Json<AddArchiveRequest>,
) -> Response {
    // Mirror the CLI's "every crawl belongs to a collection" guard.
    if req.collection.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "collection is required").into_response();
    }
    audit_detail(
        &state,
        curator.principal(),
        Action::CrawlAdd,
        // The slug, not the typed name: the crawl has no id yet, so the
        // collection is the target, and a log you grep by collection wants the
        // same spelling whichever endpoint wrote the record.
        &crate::collections::slugify(&req.collection),
        Some(serde_json::json!({ "collection_name": req.collection })),
    );
    let id = start_index_job(&curator, &state, req.path, req.collection, req.name, None);
    (StatusCode::ACCEPTED, Json(AddArchiveResponse { job: id })).into_response()
}

/// `POST /api/archives/upload` — add a crawl by uploading the `.wacz` bytes
/// (multipart/form-data: `collection`, optional `name`, and the `file`). The
/// upload is streamed to a temp file, then indexed exactly like a local path
/// (`index_location` copies it into `archive/`); the temp file is deleted when
/// the job finishes. Returns a job id (202) to stream progress from.
pub(super) async fn upload_archive(
    State(state): State<Arc<AppState>>,
    // Before `Multipart`: an extractor that reads the body must come last, and
    // this way the 403 fires before a multi-gigabyte WACZ is streamed in.
    curator: Curator,
    mut multipart: Multipart,
) -> Response {
    let mut collection: Option<String> = None;
    let mut name: Option<String> = None;
    let mut tmpdir: Option<tempfile::TempDir> = None;
    let mut file_path: Option<PathBuf> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("malformed upload: {e}")).into_response()
            }
        };
        match field.name() {
            Some("collection") => collection = field.text().await.ok(),
            Some("name") => name = field.text().await.ok(),
            Some("file") => {
                // Keep just the basename of the client filename, defaulting the
                // extension so `index_location`'s `.wacz` check passes.
                let raw = field.file_name().unwrap_or("upload.wacz").to_string();
                let fname = Path::new(&raw)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "upload.wacz".to_string());
                let dir = match tempfile::TempDir::new() {
                    Ok(d) => d,
                    Err(e) => return error_response(anyhow::anyhow!(e)).into_response(),
                };
                let path = dir.path().join(&fname);
                if let Err(e) = stream_field_to_file(field, &path).await {
                    return error_response(e).into_response();
                }
                file_path = Some(path);
                tmpdir = Some(dir);
            }
            _ => {}
        }
    }

    let collection = match collection {
        Some(c) if !c.trim().is_empty() => c,
        _ => return (StatusCode::BAD_REQUEST, "collection is required").into_response(),
    };
    let Some(path) = file_path else {
        return (StatusCode::BAD_REQUEST, "a file is required").into_response();
    };
    let name = name.filter(|n| !n.trim().is_empty());
    let location = path.to_string_lossy().to_string();
    audit_detail(
        &state,
        curator.principal(),
        Action::CrawlUpload,
        &crate::collections::slugify(&collection),
        Some(serde_json::json!({ "collection_name": collection })),
    );
    let id = start_index_job(&curator, &state, location, collection, name, tmpdir);
    (StatusCode::ACCEPTED, Json(AddArchiveResponse { job: id })).into_response()
}

/// Stream one multipart field's bytes to `path`, chunk by chunk (never buffering
/// the whole upload in memory).
async fn stream_field_to_file(
    mut field: axum::extract::multipart::Field<'_>,
    path: &Path,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(path).await?;
    while let Some(chunk) = field.chunk().await? {
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(())
}

/// `GET /api/archives/{id}/events` — stream one job's progress as SSE. The
/// receiver is taken from the registry on first connect (a job's progress is
/// consumed once); reconnecting after that yields 404.
pub(super) async fn add_archive_events(
    State(state): State<Arc<AppState>>,
    _curator: Curator,
    axum::extract::Path(id): axum::extract::Path<u64>,
) -> Response {
    let Some(rx) = state.jobs.lock().unwrap().remove(&id) else {
        return (StatusCode::NOT_FOUND, "unknown or already-consumed job").into_response();
    };

    let stream = UnboundedReceiverStream::new(rx).map(|ev| {
        let event = Event::default()
            .event(ev.name())
            .data(serde_json::to_string(&ev).unwrap_or_default());
        Ok::<Event, std::convert::Infallible>(event)
    });

    Sse::new(stream).into_response()
}

// ── Management mode: collection form + accession desk ───────────────────────
//
// Edit-in-place: the collections list is the homepage and collections are edited
// from their own pages, so the only dedicated workroom pages are the two
// multi-step accessions — the finding-aid form and the add-crawls desk.

/// `GET /manage/collections/new` — the empty finding-aid form.
pub(super) async fn new_collection_form(
    State(state): State<Arc<AppState>>,
    // Same rule as the danger zone: don't render a form whose every
    // control would 403. Without this an authenticated Reader reaches
    // the upload form and the pre-filled finding-aid editor.
    _curator: Curator,
    headers: HeaderMap,
) -> Response {
    let (_, who) = admin_ctx(&state, &headers);
    views::collection_form(&views::CollectionFormData::default(), who.as_deref()).into_response()
}

/// `GET /manage/edit/{id}` — the finding-aid form pre-filled for an existing
/// collection (name locked, since the slug is its identity).
pub(super) async fn edit_collection_form(
    State(state): State<Arc<AppState>>,
    // Same rule as the danger zone: don't render a form whose every
    // control would 403. Without this an authenticated Reader reaches
    // the upload form and the pre-filled finding-aid editor.
    _curator: Curator,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e).into_response(),
    };
    let Some(c) = manifest.collections.iter().find(|c| c.id == id) else {
        return (StatusCode::NOT_FOUND, "unknown collection").into_response();
    };
    let form = views::CollectionFormData {
        id: c.id.to_string(),
        name: c.name.clone(),
        description: c.description.clone().unwrap_or_default(),
        curator: c.curator.clone().unwrap_or_default(),
        creator: c.creator.clone().unwrap_or_default(),
        dates: c.dates.clone().unwrap_or_default(),
        rights: c.rights.clone().unwrap_or_default(),
        subjects: c.subjects.join(", "),
        narrative: c.narrative.clone().unwrap_or_default(),
        editing: true,
    };
    let (_, who) = admin_ctx(&state, &headers);
    views::collection_form(&form, who.as_deref()).into_response()
}

/// Query for the accession desk: which collection to add to (prefilled).
#[derive(Deserialize)]
pub(super) struct AddQuery {
    #[serde(default)]
    collection: String,
}

/// `GET /manage/add` — the add-crawls accession desk, with the target collection
/// prefilled when arriving from a collection page.
pub(super) async fn accession_desk_page(
    State(state): State<Arc<AppState>>,
    // Same rule as the danger zone: don't render a form whose every
    // control would 403. Without this an authenticated Reader reaches
    // the upload form and the pre-filled finding-aid editor.
    _curator: Curator,
    headers: HeaderMap,
    Query(q): Query<AddQuery>,
) -> Response {
    // The `?collection=` is a slug (id); resolve its display name if we know it
    // (falling back to the raw value for a not-yet-created collection).
    let id = q.collection.trim().to_string();
    let name = Manifest::open(&state.index_dir)
        .ok()
        .and_then(|m| {
            m.collections
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.name.clone())
        })
        .unwrap_or_else(|| id.clone());
    let (_, who) = admin_ctx(&state, &headers);
    views::accession_desk(&id, &name, who.as_deref()).into_response()
}

/// Form body for create/edit collection (`application/x-www-form-urlencoded`).
/// All finding-aid fields are optional; `subjects` is a comma-separated list.
#[derive(Deserialize)]
pub(super) struct CollectionForm {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    curator: String,
    #[serde(default)]
    creator: String,
    #[serde(default)]
    dates: String,
    #[serde(default)]
    rights: String,
    #[serde(default)]
    subjects: String,
    #[serde(default)]
    narrative: String,
}

/// Trim a form field to `Some(value)`, or `None` if empty. (v1 leaves cleared
/// fields untouched rather than blanking them; explicit clearing is a follow-up.)
fn field_opt(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// `POST /api/collections` — create or edit a collection finding aid, then
/// redirect (POST-redirect-GET) to its page. Wraps [`crate::index::set_collection`].
pub(super) async fn create_collection(
    State(state): State<Arc<AppState>>,
    curator: Curator,
    Form(form): Form<CollectionForm>,
) -> Response {
    let name = form.name.trim().to_string();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "collection name is required").into_response();
    }
    audit_detail(
        &state,
        curator.principal(),
        Action::CollectionSet,
        &crate::collections::slugify(&name),
        // The display name is what was actually set, and it can change while
        // the slug stays put, so it is worth keeping alongside.
        Some(serde_json::json!({ "collection_name": name })),
    );
    let subjects: Vec<String> = form
        .subjects
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let fields = crate::collections::CollectionFields {
        description: field_opt(&form.description),
        curator: field_opt(&form.curator),
        creator: field_opt(&form.creator),
        dates: field_opt(&form.dates),
        rights: field_opt(&form.rights),
        subjects: (!subjects.is_empty()).then_some(subjects),
        narrative: field_opt(&form.narrative),
    };
    let home = state.home.clone();
    let actor = curator.principal().id().clone();
    // set_collection writes the README + manifest — quick, but blocking, so keep
    // it off the async runtime. The homepage re-reads the manifest per request,
    // so the new/edited collection shows immediately (no searcher reload needed).
    let result = tokio::task::spawn_blocking(move || {
        crate::index::set_collection(&home, &name, &fields, Some(&actor))
    })
    .await;
    match result {
        Ok(Ok(id)) => Redirect::to(&format!("/collection/{id}")).into_response(),
        Ok(Err(e)) => error_response(e).into_response(),
        Err(e) => error_response(anyhow::anyhow!(e)).into_response(),
    }
}

/// `POST /api/crawls/{id}/delete` — remove a crawl (index docs, manifest entry,
/// local WACZ, thumbnail), reload the searcher, and return to its collection.
pub(super) async fn delete_crawl_handler(
    State(state): State<Arc<AppState>>,
    // Curator, not Admin: a curator may remove a crawl *they* accessioned, so
    // a mis-upload doesn't need someone else to clean up. Whose it is depends
    // on the manifest, so the real check happens below.
    curator: Curator,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let added_by = match Manifest::open(&state.index_dir) {
        Ok(m) => m
            .wacz_by_id(&id)
            .and_then(|w| w.added_by.as_ref().map(|s| s.as_str().to_string())),
        Err(e) => return error_response(e).into_response(),
    };
    if !curator.principal().may_delete_crawl(added_by.as_deref()) {
        return Denied::Insufficient("deleting a crawl someone else added").into_response();
    }
    audit(&state, curator.principal(), Action::CrawlDelete, &id);
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Delete opens Tantivy's exclusive writer + rewrites the manifest, so it
        // takes the same write lock as an add (poison-tolerant); it's quick, so
        // there's no queued-progress channel to announce a wait on.
        let _guard = state.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let plan = crate::index::delete_crawl(&state.home, &id)?;
        state.reload_searcher()?;
        Ok::<_, anyhow::Error>(plan)
    })
    .await;
    match result {
        Ok(Ok(plan)) => Redirect::to(&format!("/collection/{}", plan.collection)).into_response(),
        Ok(Err(e)) => error_response(e).into_response(),
        Err(e) => error_response(anyhow::anyhow!(e)).into_response(),
    }
}

/// Form body for a collection delete: the `with_crawls` checkbox (absent when
/// unticked; `"true"`/`"on"`/`"1"` when ticked).
#[derive(Deserialize)]
pub(super) struct DeleteCollectionForm {
    #[serde(default)]
    with_crawls: Option<String>,
}

/// `POST /api/collections/{id}/delete` — remove a collection grouping (and, with
/// `with_crawls`, its member crawls), reload the searcher, and return home.
pub(super) async fn delete_collection_handler(
    State(state): State<Arc<AppState>>,
    admin: Admin,
    axum::extract::Path(id): axum::extract::Path<String>,
    Form(form): Form<DeleteCollectionForm>,
) -> Response {
    let with_crawls = form
        .with_crawls
        .as_deref()
        .is_some_and(|v| matches!(v, "true" | "on" | "1"));
    audit_detail(
        &state,
        admin.principal(),
        Action::CollectionDelete,
        &id,
        Some(serde_json::json!({ "with_crawls": with_crawls })),
    );
    // This ends in a recursive remove_dir_all, so the id has to be a valid
    // single path component before it goes anywhere near the filesystem.
    let Some(cid) = CollectionId::parse(&id) else {
        return (StatusCode::NOT_FOUND, "unknown collection").into_response();
    };
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Refusing a non-empty collection is a client choice, not a server fault,
        // so surface it as 409 rather than letting the lib error become a 500.
        let plan = crate::index::plan_collection_deletion(&state.home, &cid)?;
        if plan.member_count > 0 && !with_crawls {
            return Ok(DeleteOutcome::Refused(plan.member_count));
        }
        let _guard = state.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        crate::index::delete_collection(&state.home, &cid, with_crawls)?;
        state.reload_searcher()?;
        Ok::<_, anyhow::Error>(DeleteOutcome::Done)
    })
    .await;
    match result {
        Ok(Ok(DeleteOutcome::Done)) => Redirect::to("/").into_response(),
        Ok(Ok(DeleteOutcome::Refused(n))) => (
            StatusCode::CONFLICT,
            format!(
                "This collection has {n} crawl(s); tick “also delete member crawls” \
                 to remove them too, or delete/move the crawls first."
            ),
        )
            .into_response(),
        Ok(Err(e)) => error_response(e).into_response(),
        Err(e) => error_response(anyhow::anyhow!(e)).into_response(),
    }
}

/// Outcome of a collection-delete attempt: done, or refused because it still has
/// members and `with_crawls` wasn't set (a 409, not a 500).
enum DeleteOutcome {
    Done,
    Refused(usize),
}
