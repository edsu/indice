//! Management write surface: adding archives (upload / URL) with SSE
//! progress, the finding-aid form, the accession desk, and deletes.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio_stream::StreamExt;

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

/// Adding by reference means a URL. A path on the server's disk is refused.
///
/// `location` goes to `Source::parse`, where an `http(s)` prefix makes it a URL
/// and **anything else is a filesystem path**. Over HTTP that is a bad trade
/// whichever shape indice is in. On a server the curator and the operator have
/// come apart, so an approved stranger could ask the server to read any `.wacz`
/// it can reach — another tenant's archive, a backup staged on the same volume
/// — and file it into a collection where they can read it back. On a
/// workstation it is safe and nobody used it: the file is already on the disk,
/// and the command line is a shorter route to it than a web form.
///
/// An earlier version of this refused a path only behind an auth proxy, with a
/// flag to opt back in. That bought one capability nobody wanted at the price
/// of a setting whose correct value differed by deployment shape, which is the
/// kind of thing this codebase has been removing all year.
///
/// The capability itself is not gone, only its HTTP surface. `indice index
/// /path/to.wacz` is unchanged, which is where a mounted archive volume is
/// accessioned from anyway, and a curator keeps the upload endpoint, which
/// carries the bytes rather than naming someone else's file.
fn require_url_location(location: &str) -> Option<Response> {
    // Asked of `Source::parse` rather than re-testing the scheme here. That
    // function owns the rule "an http(s) prefix is a URL, anything else is a
    // path", and a second copy of it would have to be kept in step: teach it an
    // `s3://` arm later and a hand-rolled prefix check here silently stops
    // matching what it guards.
    let is_url = matches!(
        crate::collections::Source::parse(location),
        crate::collections::Source::Url(_)
    );
    (!is_url).then(|| {
        (
            StatusCode::BAD_REQUEST,
            "add an archive by an http(s) URL, or upload the file. A path on the \
             server's own disk is not accepted here; use `indice index` on the \
             machine itself for that.",
        )
            .into_response()
    })
}

/// Body of `POST /api/archives` — add a crawl by reference. Browser byte-upload
/// uses [`upload_archive`] (`/api/archives/upload`) instead.
#[derive(Deserialize)]
pub(super) struct AddArchiveRequest {
    /// An `http(s)://` URL to a `.wacz`.
    ///
    /// Named `path` because `index_location` accepts either a URL or a local
    /// filesystem path, and the CLI still passes the latter. Over HTTP only a
    /// URL is accepted: see [`require_url_location`].
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
/// Takes a `&Curator` for two reasons. It is read, for
/// `principal().id()` — the actor credited with accessioning whatever this
/// ingest records. And its field is private to `auth.rs`, so one cannot be
/// fabricated: a caller must have obtained it from the extractor, i.e. must
/// have passed the check. That makes "I added a handler and forgot to gate it"
/// a compile error rather than a silent exposure.
/// An uploaded file staged for indexing, and how many bytes it is.
///
/// Together because the job needs both: the directory, to keep the file alive
/// until it has been filed and then delete it; the size, to check it against the
/// archive's room. The size is what the upload counted as it streamed, rather
/// than read back from the path, so the job never asks the filesystem about a
/// location string, which for an add by URL is request input.
pub(super) struct StagedUpload {
    /// Read by nothing: held for its destructor, which deletes the file.
    pub _dir: tempfile::TempDir,
    pub bytes: u64,
}

fn start_index_job(
    curator: &Curator,
    state: &Arc<AppState>,
    location: String,
    collection: String,
    name: Option<String>,
    staged: Option<StagedUpload>,
) -> u64 {
    // On a server the person naming the location may be an approved stranger
    // rather than the operator, so a URL may only point at a public address.
    // A workstation keeps the unrestricted policy: there, the two are the same
    // person, and fetching from a NAS on the LAN is the ordinary case.
    let fetch = if state.forward_auth.is_some() && !state.allow_internal_fetch {
        crate::http_range::FetchPolicy::PublicOnly
    } else {
        crate::http_range::FetchPolicy::Unrestricted
    };
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
        // An upload brings a staged file to file into the archive; adding by
        // URL does not, since a URL is streamed in place or downloaded to a
        // temp file that is removed afterwards.
        let admit_bytes = staged.as_ref().map(|s| s.bytes);
        // Held until the job ends; dropping it deletes the uploaded temp file
        // (after `index_location` has copied it into `archive/`).
        let _staged = staged;
        let progress = ChannelProgress { tx: tx.clone() };
        let result = {
            // The cross-process index lock is taken *before* the in-process
            // one, and the order matters more than it looks. `index_location`
            // takes this lock itself (re-entrantly, so this does not wait
            // twice); if we let it do so from inside `write_lock`, a long
            // `indice reindex` in another process would park this thread on the
            // flock while it held the mutex that gates *every* workroom write —
            // annotations, deletes, collection edits — so the whole write
            // surface would hang with no 503 and no message, and each waiting
            // request would sit on a blocking-pool thread. Waiting out here
            // instead leaves the other writers free.
            let _index = match crate::index::lock::lock_index(
                &job_state.home,
                "a workroom add",
                &progress,
            ) {
                Ok(l) => l,
                Err(e) => {
                    let _ = tx.send(ProgressEvent::Error {
                        message: format!("could not lock the search index: {e:#}"),
                    });
                    return;
                }
            };
            // `Manifest::save` rewrites waczs.json wholesale from an in-memory
            // vec, so every manifest write is a read-modify-write and has to
            // happen under the lock.
            let _guard = acquire_write_lock(&job_state.write_lock, &progress);
            // The check that actually holds the archive limit. Under the lock
            // every job that files into the archive is serialised, so the room
            // measured here is the room there is; the request-time check could
            // not know about uploads queued alongside this one.
            if let Some(size) = admit_bytes {
                if let Err(refused) = admit(&job_state, size) {
                    let _ = tx.send(ProgressEvent::Error {
                        message: refused.status_and_message().1,
                    });
                    return;
                }
            }
            // Custody travels with the ingest, so each crawl is attributed as
            // it is recorded. A multi-WACZ add that fails part way therefore
            // attributes exactly what committed — which are precisely the
            // crawls their curator needs to be able to undo.
            crate::index::Ingest::new(&job_state.home)
                .name(name.as_deref())
                .actor(Some(&actor))
                .fetch_policy(fetch)
                .download_limit(job_state.upload_limits.per_upload)
                .progress(&progress)
                .index_location(&location, &collection)
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

/// `POST /api/archives` — add a crawl by `http(s)://` URL. Starts
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
    // Before the audit record, not after: a refused probe is not an accession,
    // and twenty of them logged as ordinary adds against a collection that
    // stays empty is worse than no log at all.
    if let Some(refusal) = require_url_location(&req.path) {
        return refusal;
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
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    // Before any of the body is read, so a full archive refuses at once rather
    // than after a curator has waited for gigabytes to arrive.
    let budget = match accession_budget(&state) {
        Ok(b) => b,
        Err(refused) => return refused.into_response(),
    };
    // A body that declares itself too large is refused from its headers.
    // Nothing else would: the route's body limit counts frames as they arrive
    // and never consults `Content-Length`, so without this a 50 GB upload
    // against a 2 GiB cap streams 2 GiB to disk before it is told no.
    if let (Some(b), Some(declared)) = (
        budget,
        headers
            .get(axum::http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok()),
    ) {
        if declared > b.bytes().saturating_add(UPLOAD_FRAMING_ALLOWANCE) {
            return b.refused().into_response();
        }
    }
    let mut collection: Option<String> = None;
    let mut name: Option<String> = None;
    let mut staged: Option<StagedUpload> = None;
    let mut file_path: Option<PathBuf> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return multipart_refusal(e, budget),
        };
        match field.name() {
            Some("collection") => match read_text_field(field, budget).await {
                Ok(text) => collection = Some(text),
                Err(refusal) => return refusal,
            },
            Some("name") => match read_text_field(field, budget).await {
                Ok(text) => name = Some(text),
                Err(refusal) => return refusal,
            },
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
                    Err(e) => return curator.error_response(anyhow::anyhow!(e)),
                };
                let path = dir.path().join(&fname);
                let bytes = match stream_field_to_file(&curator, field, &path, budget).await {
                    Ok(b) => b,
                    Err(refusal) => return refusal,
                };
                file_path = Some(path);
                staged = Some(StagedUpload { _dir: dir, bytes });
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
    let id = start_index_job(&curator, &state, location, collection, name, staged);
    (StatusCode::ACCEPTED, Json(AddArchiveResponse { job: id })).into_response()
}

/// Stream one multipart field's bytes to `path`, chunk by chunk (never buffering
/// the whole upload in memory), stopping once it passes `budget`.
///
/// Counted here as well as by the route's body limit, which also counts frames
/// as they arrive and so stops a chunked body as well as a declared one. This
/// count does what it cannot: it enforces the archive's remaining room, which
/// can be tighter than the per-upload cap and which the body limit knows
/// nothing about, and it is exact for the file alone, where the body limit has
/// to leave room for multipart framing. A body that *declares* itself too large
/// never gets this far; `upload_archive` refuses it from its headers.
async fn stream_field_to_file(
    curator: &Curator,
    mut field: axum::extract::multipart::Field<'_>,
    path: &Path,
    budget: Option<Budget>,
) -> Result<u64, Response> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| curator.error_response(e.into()))?;
    let mut written: u64 = 0;
    loop {
        let chunk = match field.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => return Err(multipart_refusal(e, budget)),
        };
        written += chunk.len() as u64;
        if let Some(b) = budget {
            if written > b.bytes() {
                // The temp directory is dropped with the handler, which
                // deletes this partial file.
                return Err(b.refused().into_response());
            }
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| curator.error_response(e.into()))?;
    }
    file.flush()
        .await
        .map_err(|e| curator.error_response(e.into()))?;
    Ok(written)
}

/// The limits on what the browser write surface may add, from `config.yaml`.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct UploadLimits {
    /// The largest single WACZ a curator may add, in bytes: an upload, or a
    /// URL that has to be downloaded because it cannot be streamed.
    pub per_upload: Option<u64>,
    /// The most `<home>/archive/` may hold, in bytes. See
    /// [`crate::index::archive_bytes`].
    pub archive: Option<u64>,
}

/// How much one accession may still add, and which limit says so.
///
/// An enum rather than a byte count with a label beside it, so the number and
/// the reason cannot disagree: the room left is derived from the limit that
/// sets it.
#[derive(Debug, Clone, Copy)]
pub(super) enum Budget {
    /// The per-upload cap is the tighter limit.
    Upload { cap: u64 },
    /// The archive's remaining room is the tighter limit.
    Archive { used: u64, cap: u64 },
}

impl Budget {
    /// Bytes this accession may add.
    fn bytes(self) -> u64 {
        match self {
            Budget::Upload { cap } => cap,
            Budget::Archive { used, cap } => cap.saturating_sub(used),
        }
    }

    /// The refusal for spending past this budget.
    fn refused(self) -> Refused {
        match self {
            Budget::Upload { cap } => Refused::TooLarge { cap },
            Budget::Archive { used, cap } => Refused::ArchiveFull { used, cap },
        }
    }
}

/// Why an accession was refused.
///
/// A small type rather than a ready-made `Response`, which is a large value to
/// carry through a `Result`. It renders itself through axum's `IntoResponse`,
/// so a handler still just returns it, and every refusal names its limit and
/// the setting behind it.
#[derive(Debug)]
pub(super) enum Refused {
    /// Larger than `uploads.max_mb`.
    TooLarge { cap: u64 },
    /// The archive is full, or this would overfill it.
    ArchiveFull { used: u64, cap: u64 },
    /// The archive could not be measured.
    Measure(std::io::Error),
}

impl Refused {
    /// The status and the message, shared by a handler's response and a job's
    /// progress error so both say the same thing.
    pub(super) fn status_and_message(&self) -> (StatusCode, String) {
        match self {
            Refused::TooLarge { cap } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "this is larger than the {} limit on a single upload \
                     (uploads.max_mb in config.yaml)",
                    human_size(*cap)
                ),
            ),
            // 507 rather than 413: the request is not too large in itself, the
            // archive has no room for it, and a smaller file may still fit.
            Refused::ArchiveFull { used, cap } => (
                StatusCode::INSUFFICIENT_STORAGE,
                format!(
                    "the archive is limited to {} and holds {}, so there is {} left \
                     (uploads.max_total_gb in config.yaml)",
                    human_size(*cap),
                    human_size(*used),
                    human_size(cap.saturating_sub(*used)),
                ),
            ),
            Refused::Measure(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not measure the archive: {e}"),
            ),
        }
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        if let Refused::Measure(_) = &self {
            tracing::error!("{}", self.status_and_message().1);
        }
        self.status_and_message().into_response()
    }
}

/// How full the archive is against its limit: `(used, cap)`, or `None` when no
/// archive limit is set. The one place this is measured, for the request-time
/// check, the job-time check and the accession desk alike.
pub(super) fn archive_usage(state: &AppState) -> Result<Option<(u64, u64)>, Refused> {
    let Some(cap) = state.upload_limits.archive else {
        return Ok(None);
    };
    let used = crate::index::archive_bytes(&state.home).map_err(Refused::Measure)?;
    Ok(Some((used, cap)))
}

/// How much one accession may add, or the refusal if the archive is already
/// full. `None` means no limit applies.
///
/// This is the cheap check, made when the request arrives so a full archive
/// refuses before a curator waits for gigabytes to arrive. It is not the one
/// that holds the limit: any number of uploads arriving together each see the
/// same room. [`admit`] is, made inside the job once it is serialised.
pub(super) fn accession_budget(state: &AppState) -> Result<Option<Budget>, Refused> {
    let archive = match archive_usage(state)? {
        None => None,
        Some((used, cap)) if used >= cap => return Err(Refused::ArchiveFull { used, cap }),
        Some((used, cap)) => Some(Budget::Archive { used, cap }),
    };
    let upload = state
        .upload_limits
        .per_upload
        .map(|cap| Budget::Upload { cap });
    Ok(match (upload, archive) {
        (Some(u), Some(a)) => Some(if u.bytes() <= a.bytes() { u } else { a }),
        (one, other) => one.or(other),
    })
}

/// Whether `size` more bytes may enter the archive now.
///
/// Called by a job after it holds the write lock, which serialises every job
/// that files into the archive, so the room it measures is the room there is:
/// uploads that each passed [`accession_budget`] while queued behind a long
/// import cannot all spend the same space. The size is known by then, so this
/// is exact rather than a forecast.
pub(super) fn admit(state: &AppState, size: u64) -> Result<(), Refused> {
    match archive_usage(state)? {
        Some((used, cap)) if used.saturating_add(size) > cap => {
            Err(Refused::ArchiveFull { used, cap })
        }
        _ => Ok(()),
    }
}

/// The most a text field in an upload may be. A collection name and a display
/// name are a line each; this is generous for either.
const MAX_TEXT_FIELD: usize = 4 * 1024;

/// Read a short text field, refusing one that is not short.
///
/// `Field::text` buffers the whole field in memory, and with the body limit
/// lifted on this route nothing else bounds it, so a client could send a
/// collection "name" of several gigabytes and exhaust the server's memory with
/// one request. This holds whatever `config.yaml` says, because the size caps
/// there are about the file and are optional.
async fn read_text_field(
    mut field: axum::extract::multipart::Field<'_>,
    budget: Option<Budget>,
) -> Result<String, Response> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                if buf.len() + chunk.len() > MAX_TEXT_FIELD {
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!(
                            "a text field in this upload is longer than {MAX_TEXT_FIELD} bytes"
                        ),
                    )
                        .into_response());
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => return Err(multipart_refusal(e, budget)),
        }
    }
    String::from_utf8(buf)
        .map_err(|_| (StatusCode::BAD_REQUEST, "a text field is not valid UTF-8").into_response())
}

/// A multipart error as a response.
///
/// A 413 here is the route's body limit, which axum reports as "Request payload
/// is too large" without saying what the limit is, so the budget's own refusal
/// is used instead. That is always true: the body limit is only set when there
/// is a per-upload cap, and a body past it is past whichever limit the budget
/// names, the budget being the tighter of the two.
///
/// Anything else is reported as the client's problem, 400, whatever status axum
/// attached. The common case is a curator cancelling an upload partway, which
/// axum surfaces as a 500 because the body stream failed; reporting it as a
/// server fault would put an `error!` in the log for ordinary behaviour.
fn multipart_refusal(
    e: axum::extract::multipart::MultipartError,
    budget: Option<Budget>,
) -> Response {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        if let Some(b) = budget {
            return b.refused().into_response();
        }
        return (StatusCode::PAYLOAD_TOO_LARGE, e.body_text()).into_response();
    }
    (
        StatusCode::BAD_REQUEST,
        format!("upload refused: {}", e.body_text()),
    )
        .into_response()
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
    curator: Curator,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return curator.error_response(e),
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
    let room = room_note(&state);
    // The same refusal text the server would send, so the page and the server
    // say the same thing whichever one catches it.
    let max_upload = state
        .upload_limits
        .per_upload
        .map(|cap| (cap, Refused::TooLarge { cap }.status_and_message().1));
    let room_left = archive_usage(&state).ok().flatten().map(|(used, cap)| {
        (
            cap.saturating_sub(used),
            Refused::ArchiveFull { used, cap }.status_and_message().1,
        )
    });
    views::accession_desk(
        &id,
        &name,
        who.as_deref(),
        room.as_deref(),
        max_upload.as_ref(),
        room_left.as_ref(),
    )
    .into_response()
}

/// The accession desk's line about how much room there is, so a curator sees
/// the limit before starting an upload that cannot finish. `None` when neither
/// limit is set.
///
/// Best-effort: if the archive cannot be measured the line is left off. The
/// desk still works and the upload itself reports the real error.
fn room_note(state: &AppState) -> Option<String> {
    let per_upload = state
        .upload_limits
        .per_upload
        .map(|cap| format!("Uploads up to {} each.", human_size(cap)));
    let archive = archive_usage(state).ok().flatten().map(|(used, cap)| {
        if used >= cap {
            // Uploads and download imports are closed. Adding by URL and a
            // streamed Browsertrix import store nothing locally, so they still
            // work, and saying "nothing can be added" would send a curator away
            // from both.
            format!(
                "The archive is full ({} of its {} limit), so uploads and download \
                 imports are refused until a crawl is removed or the limit is raised. \
                 Adding by URL still works.",
                human_size(used),
                human_size(cap)
            )
        } else {
            format!(
                "The archive holds {} of its {} limit; {} left.",
                human_size(used),
                human_size(cap),
                human_size(cap - used)
            )
        }
    });
    match (per_upload, archive) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (one, other) => one.or(other),
    }
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
    let actor = curator.principal().id().clone();
    let state = state.clone();
    // set_collection writes the README + manifest — quick, but blocking, so keep
    // it off the async runtime. The homepage re-reads the manifest per request,
    // so the new/edited collection shows immediately (no searcher reload needed).
    let result = tokio::task::spawn_blocking(move || {
        // Deliberately does NOT take `write_lock`.
        //
        // It used to, for a good reason: `Manifest::save` rewrites waczs.json
        // wholesale whatever changed, so saving a description while an
        // add-crawls job ran could install a stale copy and erase that job's
        // entry. `set_collection` now does its read-modify-write inside the
        // manifest lock, which gives that exclusion properly — against other
        // processes too, which the mutex never did.
        //
        // Keeping the mutex as well would undo the point of the two tiers. An
        // add-crawls job holds `write_lock` for its whole run, so a curator
        // saving a description would still queue behind the entire ingest —
        // the very thing the manifest tier exists to stop. Describing is
        // supposed to stay available while crawls are being added.
        crate::index::set_collection(&state.home, &name, &fields, Some(&actor))
    })
    .await;
    match result {
        Ok(Ok(id)) => Redirect::to(&format!("/collection/{id}")).into_response(),
        Ok(Err(e)) => curator.error_response(e),
        Err(e) => curator.error_response(anyhow::anyhow!(e)),
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
    let principal = curator.principal().clone();
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Index lock first, with a deadline. `delete_crawl` takes it too (so
        // `indice crawl delete` is covered), but a handler must not wait on a
        // cross-process lock from inside `write_lock` — see `index::lock`'s
        // ordering rule. Re-entrant, so the inner acquisition is free.
        let _index = match acquire_index_for_request(&state.home, "deleting a crawl")? {
            Ok(l) => l,
            Err(holder) => return Ok(CrawlDeleteOutcome::Busy(holder)),
        };
        // Delete opens Tantivy's exclusive writer + rewrites the manifest, so it
        // takes the same write lock as an add (poison-tolerant); it's quick, so
        // there's no queued-progress channel to announce a wait on.
        let _guard = state.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        // Custody is read and judged INSIDE the lock. Reading it outside and
        // deleting under the lock leaves a window: a crawl id is a hash of its
        // source, so the entry can be deleted and the same WACZ re-added under
        // a different owner in between, and the check would have passed against
        // the owner who is no longer there.
        let added_by = Manifest::open(&state.index_dir)?
            .wacz_by_id(&id)
            .and_then(|w| w.added_by.as_ref().map(|s| s.as_str().to_string()));
        if !principal.may_delete_crawl(added_by.as_deref()) {
            return Ok(CrawlDeleteOutcome::Denied);
        }
        // Audited here rather than before the check, so a refused delete is not
        // recorded as an authorized attempt.
        audit(&state, &principal, Action::CrawlDelete, &id);
        let plan = crate::index::delete_crawl(&state.home, &id)?;
        state.reload_searcher()?;
        Ok::<_, anyhow::Error>(CrawlDeleteOutcome::Deleted(plan))
    })
    .await;
    match result {
        Ok(Ok(CrawlDeleteOutcome::Deleted(plan))) => {
            Redirect::to(&format!("/collection/{}", plan.collection)).into_response()
        }
        Ok(Ok(CrawlDeleteOutcome::Busy(holder))) => busy_response(&holder),
        Ok(Ok(CrawlDeleteOutcome::Denied)) => {
            Denied::Insufficient("deleting a crawl someone else added").into_response()
        }
        Ok(Err(e)) => curator.error_response(e),
        Err(e) => curator.error_response(anyhow::anyhow!(e)),
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
    let actor = admin.principal().clone();
    // This ends in a recursive remove_dir_all, so the id has to be a valid
    // single path component before it goes anywhere near the filesystem.
    let Some(cid) = CollectionId::parse(&id) else {
        // Logged rather than audited. Moving the audit to the point of writing
        // (so a 503 or a 409 stops recording a deletion that never happened)
        // otherwise loses this entirely, and a rejected id — a typo, or a
        // traversal attempt — is exactly the attempt worth keeping. It is not
        // a CollectionDelete, though, so it does not belong in that record.
        tracing::warn!(
            actor = %actor.id(),
            collection = %id,
            "refused a collection delete: not a valid collection id"
        );
        return (StatusCode::NOT_FOUND, "unknown collection").into_response();
    };
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Index lock before the in-process one, as everywhere else.
        let _index = match acquire_index_for_request(&state.home, "deleting a collection")? {
            Ok(l) => l,
            Err(holder) => return Ok(DeleteOutcome::Busy(holder)),
        };
        let _guard = state.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        // Planned INSIDE the lock, like the custody read in the crawl handler.
        // Refusing a non-empty collection is a client choice, not a server
        // fault, so it is a 409 rather than letting the lib error become a 500
        // — and that only holds if the count this decides on is the count
        // `delete_collection` will see. Planning outside meant an ingest could
        // add a member while we waited up to ten seconds for the lock, leaving
        // the library to re-check, `bail!`, and surface a 500 telling a browser
        // user to pass a CLI flag.
        let plan = crate::index::plan_collection_deletion(&state.home, &cid)?;
        if plan.member_count > 0 && !with_crawls {
            return Ok(DeleteOutcome::Refused(plan.member_count));
        }
        // Audited here, not at the top of the handler: recording it before the
        // lock would log a CollectionDelete for every request that then 503s,
        // and recording it before the plan would log one for every 409 — writes
        // that provably never happened. Same rule the crawl handler states.
        audit_detail(
            &state,
            &actor,
            Action::CollectionDelete,
            &id,
            Some(serde_json::json!({ "with_crawls": with_crawls })),
        );
        crate::index::delete_collection(&state.home, &cid, with_crawls)?;
        state.reload_searcher()?;
        Ok::<_, anyhow::Error>(DeleteOutcome::Done)
    })
    .await;
    match result {
        Ok(Ok(DeleteOutcome::Done)) => Redirect::to("/").into_response(),
        Ok(Ok(DeleteOutcome::Busy(holder))) => busy_response(&holder),
        Ok(Ok(DeleteOutcome::Refused(n))) => (
            StatusCode::CONFLICT,
            format!(
                "This collection has {n} crawl(s); tick “also delete member crawls” \
                 to remove them too, or delete/move the crawls first."
            ),
        )
            .into_response(),
        Ok(Err(e)) => admin.error_response(e),
        Err(e) => admin.error_response(anyhow::anyhow!(e)),
    }
}

/// Outcome of a crawl-delete attempt.
enum CrawlDeleteOutcome {
    Deleted(crate::index::CrawlDeletion),
    /// The caller may not delete a crawl someone else accessioned (403).
    Denied,
    /// Another process holds the index lock (503); `String` describes it.
    Busy(String),
}

/// Outcome of a collection-delete attempt: done, refused because it still has
/// members and `with_crawls` wasn't set (a 409, not a 500), or the index was
/// busy (503).
enum DeleteOutcome {
    Done,
    Refused(usize),
    Busy(String),
}
