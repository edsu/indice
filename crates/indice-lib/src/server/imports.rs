//! Management-mode import browsers: Browsertrix (orgs → collections → items)
//! and Archive-It (collections → crawls), driven by server-side credentials.
//!
//! Note that the *browse* endpoints require `Curator` even though they mutate
//! nothing. They read through the operator's own credentials, so leaving them
//! open would let any signed-in reader enumerate the whole upstream account.
//! Privilege here follows whose credentials are being spent, not whether a
//! write happens.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::collections::Manifest;

use super::*;

/// Record custody for crawls an import just created.
///
/// Takes the **exact** ids the import produced rather than diffing the manifest
/// before and after. An import holds the write lock per resource (never across
/// a download), so a before/after diff would span other jobs' writes and could
/// stamp this curator's id onto someone else's crawl. `set_added_by` is itself
/// a read-modify-write of the manifest, so it runs under the lock.
fn attribute_import(
    job_state: &Arc<AppState>,
    progress: &ChannelProgress,
    actor: &crate::identity::SubjectId,
    created: &[String],
) {
    if created.is_empty() {
        return;
    }
    let ids: std::collections::HashSet<String> = created.iter().cloned().collect();
    let _guard = acquire_write_lock(&job_state.write_lock, progress);
    if let Err(e) = crate::index::set_added_by(&job_state.home, &ids, actor) {
        tracing::warn!(
            "recording who imported {} crawl(s) failed: {e:#}",
            ids.len()
        );
    }
}

/// Mint a job id for an import.
///
/// Takes a `&Curator` it never reads, for the same reason `start_index_job`
/// does: the token cannot be constructed outside `auth.rs`, so a caller has to
/// have passed the check to call this at all. Imports spend the operator's own
/// Browsertrix/Archive-It credentials, which is exactly the kind of thing that
/// should not be reachable by forgetting an extractor.
fn new_import_job(_curator: &Curator, state: &Arc<AppState>) -> u64 {
    state.job_counter.fetch_add(1, Ordering::Relaxed)
}

/// Response when a Browsertrix endpoint is hit but no credentials are configured.
fn browsertrix_unconfigured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "Browsertrix import is not configured — set BROWSERTRIX_TOKEN (or \
         BROWSERTRIX_USER + BROWSERTRIX_PASSWORD) in the server's environment.",
    )
        .into_response()
}

/// Query for the browse endpoints (orgs → collections → items). The host is
/// server-configured, not accepted here.
#[derive(Deserialize)]
pub(super) struct BxBrowse {
    #[serde(default)]
    org: String,
    #[serde(default)]
    collection: String,
}

/// Run a blocking Browsertrix client call off the async runtime, returning its
/// JSON result (or a 502 on a Browsertrix/transport error).
async fn bx_json<F>(provider: Arc<dyn crate::browsertrix::BrowsertrixProvider>, f: F) -> Response
where
    F: FnOnce(&crate::browsertrix::Client) -> Result<serde_json::Value> + Send + 'static,
{
    match tokio::task::spawn_blocking(move || {
        let client = provider.client()?;
        f(&client)
    })
    .await
    {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => {
            (StatusCode::BAD_GATEWAY, format!("Browsertrix error: {e:#}")).into_response()
        }
        Err(e) => error_response(anyhow::anyhow!(e)).into_response(),
    }
}

/// `GET /api/browsertrix/orgs` — the orgs the configured credentials can see.
pub(super) async fn bx_orgs(State(state): State<Arc<AppState>>, _curator: Curator) -> Response {
    let Some(provider) = state.browsertrix.clone() else {
        return browsertrix_unconfigured();
    };
    bx_json(provider, |c| {
        let orgs = c.orgs()?;
        Ok(serde_json::json!(orgs
            .iter()
            .map(|o| serde_json::json!({ "id": o.id, "name": o.name, "slug": o.slug }))
            .collect::<Vec<_>>()))
    })
    .await
}

/// `GET /api/browsertrix/collections?org=<oid>` — collections in an org.
pub(super) async fn bx_collections(
    State(state): State<Arc<AppState>>,
    _curator: Curator,
    Query(q): Query<BxBrowse>,
) -> Response {
    let Some(provider) = state.browsertrix.clone() else {
        return browsertrix_unconfigured();
    };
    if q.org.is_empty() {
        return (StatusCode::BAD_REQUEST, "org is required").into_response();
    }
    let org = q.org.clone();
    bx_json(provider, move |c| {
        let colls = c.collections(&org)?;
        Ok(serde_json::json!(colls
            .iter()
            .map(|c| serde_json::json!({ "id": c.id, "name": c.name }))
            .collect::<Vec<_>>()))
    })
    .await
}

/// Browsertrix item ids already imported anywhere in this instance. A crawl
/// records its origin two ways — streamed imports keep a `Source::Browsertrix`
/// (item id in the source), downloaded ones keep a local file source plus a
/// `BrowsertrixRef` provenance — so collect from both to catch either kind.
pub(super) fn imported_browsertrix_ids<'a>(
    waczs: impl Iterator<
        Item = (
            &'a crate::collections::Source,
            Option<&'a crate::collections::BrowsertrixRef>,
        ),
    >,
) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    for (source, provenance) in waczs {
        if let crate::collections::Source::Browsertrix { item, .. } = source {
            ids.insert(item.clone());
        }
        if let Some(b) = provenance {
            ids.insert(b.item_id.clone());
        }
    }
    ids
}

/// `GET /api/browsertrix/items?org=<oid>&collection=<cid>` — crawls (optionally
/// scoped to a collection), with QA-review status for the selection UI.
pub(super) async fn bx_items(
    State(state): State<Arc<AppState>>,
    _curator: Curator,
    Query(q): Query<BxBrowse>,
) -> Response {
    let Some(provider) = state.browsertrix.clone() else {
        return browsertrix_unconfigured();
    };
    if q.org.is_empty() {
        return (StatusCode::BAD_REQUEST, "org is required").into_response();
    }
    let org = q.org.clone();
    let collection = q.collection.clone();
    // Browsertrix item ids already imported into any collection in this instance,
    // so the UI can mark them and prevent accidental re-imports.
    let imported = Manifest::open(&state.index_dir)
        .map(|m| {
            imported_browsertrix_ids(m.waczs.iter().map(|w| (&w.source, w.browsertrix.as_ref())))
        })
        .unwrap_or_default();
    bx_json(provider, move |c| {
        let query = crate::browsertrix::ItemQuery {
            collection_id: (!collection.is_empty()).then_some(collection.as_str()),
            item_id: None,
        };
        let items = c.items(&org, &query)?;
        Ok(serde_json::json!(items
            .iter()
            .map(|it| {
                // Preformatted, unit-scaling size (B→TB) so the list matches the
                // rest of the app; blank when the size is unknown.
                let size_h = if it.file_size > 0 {
                    human_size(it.file_size)
                } else {
                    String::new()
                };
                serde_json::json!({
                    "id": it.id,
                    "name": it.name,
                    "date": it.date(),
                    "reviewed": it.is_reviewed(),
                    "review_status": it.review_status,
                    "upload": it.is_upload(),
                    "size": it.file_size,
                    "size_h": size_h,
                    "imported": imported.contains(&it.id),
                })
            })
            .collect::<Vec<_>>()))
    })
    .await
}

/// One selected crawl to import.
#[derive(Deserialize)]
struct BxImportItem {
    id: String,
    #[serde(default)]
    name: String,
    /// QA review rating (1–5), carried onto the crawl as provenance.
    #[serde(default)]
    review_status: Option<u8>,
}

/// Body of `POST /api/browsertrix/import`. The host is server-configured.
#[derive(Deserialize)]
pub(super) struct BxImportRequest {
    org: String,
    /// Target indice collection (display name); created if new.
    collection: String,
    items: Vec<BxImportItem>,
    /// Download a durable local copy (default) vs. stream-index in place.
    #[serde(default = "default_true")]
    download: bool,
}

fn default_true() -> bool {
    true
}

/// `POST /api/browsertrix/import` — import the selected Browsertrix crawls into a
/// collection as an ingest job (progress over the shared SSE endpoint). Two
/// modes: `download` (the default) fetches a durable local copy and indexes it;
/// otherwise the crawl is stream-indexed in place, with replay re-resolving a
/// fresh presigned URL via the resolver.
pub(super) async fn bx_import(
    State(state): State<Arc<AppState>>,
    curator: Curator,
    Json(req): Json<BxImportRequest>,
) -> Response {
    let Some(provider) = state.browsertrix.clone() else {
        return browsertrix_unconfigured();
    };
    if req.collection.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "collection is required").into_response();
    }
    if req.org.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "org is required").into_response();
    }
    if req.items.is_empty() {
        return (StatusCode::BAD_REQUEST, "select at least one crawl").into_response();
    }
    // Streaming needs the resolver (to fetch a fresh presigned URL at index time
    // and again at replay); downloading fetches each WACZ directly and doesn't.
    let resolver = state.resolver.clone();
    if !req.download && resolver.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Browsertrix streaming import requires the resolver (credentials).",
        )
            .into_response();
    }

    audit(curator.principal(), "import.browsertrix", &req.collection);
    let id = new_import_job(&curator, &state);
    let (tx, rx) = mpsc::unbounded_channel::<ProgressEvent>();
    state.jobs.lock().unwrap().insert(id, rx);

    let job_state = state.clone();
    let actor = curator.principal().id().clone();
    tokio::task::spawn_blocking(move || {
        let progress = ChannelProgress { tx: tx.clone() };
        // Snapshot custody before the import so exactly the new crawls are
        // attributed (see start_index_job).
        // Exact ids as they're created, so a partial import still attributes
        // what actually landed — those are precisely the crawls their curator
        // needs to be able to undo.
        let mut created: Vec<String> = Vec::new();
        let result = (|| -> Result<Vec<serde_json::Value>> {
            let client = provider.client()?;
            let host = client.host().to_string();
            // Each indexed WACZ, as {id, name}, so the UI can link every crawl.
            let mut crawls: Vec<serde_json::Value> = Vec::new();
            for item in &req.items {
                let resources = client.item_resources(&req.org, &item.id)?;
                let name = (!item.name.trim().is_empty()).then_some(item.name.as_str());
                for (i, res) in resources.iter().enumerate() {
                    // Both modes record the Browsertrix provenance (item id,
                    // resource hash, QA rating) so the crawl is later recognized
                    // as already-imported and carries its review. The write lock is
                    // held only around the Tantivy write + provenance — never
                    // around the download, which would block other imports.
                    let crawl_id = if req.download {
                        // Durable: download each WACZ into archive/<collection>/<item>/
                        // (no lock) and index it as a local File source.
                        let item_dir = crate::index::archive_dir(&job_state.home)
                            .join(crate::collections::slugify(&req.collection))
                            .join(crate::index::safe_component(&item.id));
                        std::fs::create_dir_all(&item_dir)?;
                        let filename =
                            crate::index::safe_wacz_filename(&res.name, &format!("resource-{i}"));
                        let dest = item_dir.join(&filename);
                        let size = if res.size > 0 {
                            format!(" ({})", human_size(res.size))
                        } else {
                            String::new()
                        };
                        crate::index::IndexProgress::phase(
                            &progress,
                            &format!("downloading {filename}{size}"),
                        );
                        crate::index::download_wacz(&res.path, &dest)?;
                        let _guard = acquire_write_lock(&job_state.write_lock, &progress);
                        crate::index::index_location(
                            &dest.to_string_lossy(),
                            &job_state.home,
                            name,
                            &req.collection,
                            false, // download (already a local file)
                            true,  // force: honor the explicitly selected crawl
                            None,
                            Some(&progress),
                        )?;
                        let abs = dest.canonicalize().unwrap_or(dest.clone());
                        let crawl_id = crate::collections::wacz_id(
                            &crate::collections::Source::for_file(&abs, &job_state.home),
                        );
                        crate::index::set_browsertrix_provenance_by_id(
                            &job_state.home,
                            &crawl_id,
                            &host,
                            &item.id,
                            &res.hash,
                            item.review_status,
                        )?;
                        crawl_id
                    } else {
                        // Index-only: stream the crawl in place under the lock;
                        // replay/reindex re-resolve a fresh URL.
                        let _guard = acquire_write_lock(&job_state.write_lock, &progress);
                        let resolver = resolver.as_ref().expect("resolver present when streaming");
                        let source = crate::collections::Source::Browsertrix {
                            host: host.clone(),
                            org: req.org.clone(),
                            item: item.id.clone(),
                            resource: res.name.clone(),
                        };
                        crate::index::index_location_with_resolver(
                            &source.location(),
                            &job_state.home,
                            name,
                            &req.collection,
                            false, // download (stream in place)
                            true,  // force: honor the explicitly selected crawl
                            None,
                            Some(resolver.as_ref()),
                            Some(&progress),
                        )?;
                        let crawl_id = crate::collections::wacz_id(&source);
                        crate::index::set_browsertrix_provenance_by_id(
                            &job_state.home,
                            &crawl_id,
                            &host,
                            &item.id,
                            &res.hash,
                            item.review_status,
                        )?;
                        crawl_id
                    };
                    // Label the crawl by item name; disambiguate by resource
                    // filename when one item yielded several WACZs.
                    let display = if item.name.trim().is_empty() {
                        item.id.clone()
                    } else {
                        item.name.clone()
                    };
                    let label = if resources.len() > 1 {
                        format!("{display} · {}", res.name)
                    } else {
                        display
                    };
                    created.push(crawl_id.clone());
                    crawls.push(serde_json::json!({ "id": crawl_id, "name": label }));
                }
            }
            Ok(crawls)
        })();
        // Not gated on `result`: `created` holds whatever committed before a
        // failure, and those crawls are durable.
        attribute_import(&job_state, &progress, &actor, &created);
        match result {
            Ok(crawls) => {
                // A bulk import commits a segment per WACZ; if that left the
                // index fragmented, compact it so search (and the homepage facet
                // overview) stays fast. Best-effort — a failure here doesn't fail
                // the import, only leaves the index un-compacted. Needs the write
                // lock (the per-resource loop above released it each time).
                {
                    let _guard = acquire_write_lock(&job_state.write_lock, &progress);
                    match crate::index::optimize_if_fragmented(&job_state.home, Some(&progress)) {
                        Ok(Some((before, after))) => {
                            tracing::info!("compacted fragmented index: {before} → {after} segments");
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!(
                            "post-import index compaction failed ({e:#}); run `indice optimize` later"
                        ),
                    }
                }
                match job_state.reload_searcher() {
                    Ok(()) => tx
                        .send(ProgressEvent::Done {
                            collection: crate::collections::slugify(&req.collection),
                            crawls,
                        })
                        .ok(),
                    Err(e) => tx
                        .send(ProgressEvent::Error {
                            message: format!("indexed, but reloading the searcher failed: {e:#}"),
                        })
                        .ok(),
                }
            }
            Err(e) => tx
                .send(ProgressEvent::Error {
                    message: format!("{e:#}"),
                })
                .ok(),
        };
    });

    (StatusCode::ACCEPTED, Json(AddArchiveResponse { job: id })).into_response()
}

fn archiveit_unconfigured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "Archive-It import is not configured — set ARCHIVEIT_USER + \
         ARCHIVEIT_PASSWORD in the server's environment.",
    )
        .into_response()
}

/// Run a blocking Archive-It client call off the async runtime, returning its
/// JSON result (or a 502 on an Archive-It/transport error). Mirrors [`bx_json`].
async fn archiveit_json<F>(provider: Arc<dyn crate::archiveit::ArchiveItProvider>, f: F) -> Response
where
    F: FnOnce(&crate::archiveit::Client) -> Result<serde_json::Value> + Send + 'static,
{
    match tokio::task::spawn_blocking(move || {
        let client = provider.client()?;
        f(&client)
    })
    .await
    {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, format!("Archive-It error: {e:#}")).into_response(),
        Err(e) => error_response(anyhow::anyhow!(e)).into_response(),
    }
}

/// `GET /api/archiveit/collections` — the account's collections (including
/// inactive ones, which still hold importable crawls).
pub(super) async fn ait_collections(
    State(state): State<Arc<AppState>>,
    _curator: Curator,
) -> Response {
    let Some(provider) = state.archiveit.clone() else {
        return archiveit_unconfigured();
    };
    archiveit_json(provider, |c| {
        let colls = c.collections(false)?;
        Ok(serde_json::json!(colls
            .iter()
            .map(|c| serde_json::json!({ "id": c.id, "name": c.name, "state": c.state }))
            .collect::<Vec<_>>()))
    })
    .await
}

#[derive(Deserialize)]
pub(super) struct AitBrowse {
    collection: Option<i64>,
}

/// `GET /api/archiveit/crawls?collection=<id>` — a collection's importable crawls
/// (finished, not deleted), each marked if already imported into this instance.
pub(super) async fn ait_crawls(
    State(state): State<Arc<AppState>>,
    _curator: Curator,
    Query(q): Query<AitBrowse>,
) -> Response {
    let Some(provider) = state.archiveit.clone() else {
        return archiveit_unconfigured();
    };
    let Some(collection) = q.collection else {
        return (StatusCode::BAD_REQUEST, "collection is required").into_response();
    };
    let index_dir = state.index_dir.clone();
    archiveit_json(provider, move |c| {
        // Crawls of this collection already imported from this host, so the UI can
        // mark them and prevent accidental re-imports (keyed by host+collection).
        let host = c.host().to_string();
        let imported: std::collections::HashSet<i64> = Manifest::open(&index_dir)
            .map(|m| {
                m.waczs
                    .iter()
                    .filter_map(|w| w.archive_it.as_ref())
                    .filter(|r| r.host == host && r.collection_id == collection)
                    .map(|r| r.crawl_id)
                    .collect()
            })
            .unwrap_or_default();
        // Per-crawl WARC totals (bytes + file count) from WASAPI — the Partner
        // API's crawl list carries no byte totals, so sum the file records.
        let mut totals: std::collections::HashMap<i64, (u64, u64)> =
            std::collections::HashMap::new();
        for f in c.webdata(&crate::archiveit::WasapiQuery {
            collection: Some(collection),
            crawl: None,
            crawl_time_after: None,
            crawl_time_before: None,
        })? {
            if let Some(cr) = f.crawl {
                let e = totals.entry(cr).or_default();
                e.0 += f.size;
                e.1 += 1;
            }
        }
        let jobs = c.crawl_jobs(Some(collection))?;
        Ok(serde_json::json!(jobs
            .iter()
            .filter(|j| j.importable())
            .map(|j| {
                let (bytes, warcs) = totals.get(&j.id).copied().unwrap_or((0, 0));
                serde_json::json!({
                    "id": j.id,
                    "status": j.status,
                    "type": j.kind,
                    "start": j.original_start_date,
                    "end": j.end_date,
                    "size": bytes,
                    "size_h": if bytes > 0 { human_size(bytes) } else { String::new() },
                    "warcs": warcs,
                    "imported": imported.contains(&j.id),
                })
            })
            .collect::<Vec<_>>()))
    })
    .await
}

/// Body of `POST /api/archiveit/import`. The host is server-configured.
#[derive(Deserialize)]
pub(super) struct AitImportRequest {
    /// Source Archive-It collection id.
    collection_id: i64,
    /// Target indice collection (display name); created if new.
    #[serde(rename = "collection")]
    into: String,
    /// Selected Archive-It crawl (job) ids to import.
    crawls: Vec<i64>,
    /// Re-import a crawl even if it's already imported.
    #[serde(default)]
    force: bool,
}

/// `POST /api/archiveit/import` — download the selected crawls' WARCs, build one
/// WACZ per crawl, and index them into `collection` as a job (progress over the
/// shared SSE endpoint). Reuses [`crate::archiveit::import_crawls`], the same
/// orchestrator the CLI uses.
pub(super) async fn ait_import(
    State(state): State<Arc<AppState>>,
    curator: Curator,
    Json(req): Json<AitImportRequest>,
) -> Response {
    let Some(provider) = state.archiveit.clone() else {
        return archiveit_unconfigured();
    };
    if req.into.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "collection is required").into_response();
    }
    if req.crawls.is_empty() {
        return (StatusCode::BAD_REQUEST, "select at least one crawl").into_response();
    }

    audit(curator.principal(), "import.archiveit", &req.into);
    let id = new_import_job(&curator, &state);
    let (tx, rx) = mpsc::unbounded_channel::<ProgressEvent>();
    state.jobs.lock().unwrap().insert(id, rx);

    let job_state = state.clone();
    let actor = curator.principal().id().clone();
    tokio::task::spawn_blocking(move || {
        let progress = ChannelProgress { tx: tx.clone() };
        // Snapshot custody before the import so exactly the new crawls are
        // attributed (see start_index_job).
        // Archive-It's importer is one orchestrator call, so it reports the
        // crawls it created. On failure it reports nothing, so anything it
        // partially committed stays unattributed — safe (only an admin can
        // remove it) rather than misattributed; noted on bead
        // rustyweb-manifest-provenance-sfu7 as a known edge.
        let mut created: Vec<String> = Vec::new();
        let result = (|| -> Result<Vec<serde_json::Value>> {
            let client = provider.client()?;
            let selected: std::collections::HashSet<i64> = req.crawls.iter().copied().collect();

            // Source metadata → Catalog (crawl_jobs for the selection + the
            // collection record), so each built WACZ can embed its provenance and
            // the finding aid can be seeded.
            let mut catalog = crate::archiveit::Catalog::default();
            for j in client.crawl_jobs(Some(req.collection_id))? {
                if selected.contains(&j.id) {
                    catalog.crawl_jobs.insert(j.id, j);
                }
            }
            let collection = client
                .collections(false)?
                .into_iter()
                .find(|c| c.id == req.collection_id);
            let mut fields = collection
                .as_ref()
                .map(crate::archiveit::collection_fields)
                .unwrap_or_default();
            if let Some(c) = collection {
                catalog.collections.insert(c.id, c);
            }

            // WARC files for the collection, grouped by crawl, kept to the selection.
            let files = client.webdata(&crate::archiveit::WasapiQuery {
                collection: Some(req.collection_id),
                crawl: None,
                crawl_time_after: None,
                crawl_time_before: None,
            })?;
            let plans: Vec<crate::archiveit::CrawlPlan> = crate::archiveit::plan_crawls(files)
                .into_iter()
                .filter(|p| selected.contains(&p.crawl_id))
                .collect();
            if plans.is_empty() {
                anyhow::bail!("no WARC files found for the selected crawls");
            }
            fields.dates = crate::archiveit::crawl_year_range(&plans);

            // Hold the write lock across the whole import: `import_crawls`
            // interleaves per-crawl download and Tantivy writes, and single-user
            // management mode only ever needs one import in flight at a time.
            let outcome = {
                let _guard = acquire_write_lock(&job_state.write_lock, &progress);
                crate::archiveit::import_crawls(
                    &client,
                    &job_state.home,
                    &req.into,
                    &plans,
                    &fields,
                    &catalog,
                    req.force,
                    Some(&progress),
                )?
            };
            created.extend(outcome.crawls.iter().map(|(id, _)| id.clone()));
            Ok(outcome
                .crawls
                .into_iter()
                .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
                .collect())
        })();
        attribute_import(&job_state, &progress, &actor, &created);
        match result {
            Ok(crawls) => {
                // A per-crawl import commits a segment per WACZ; compact if that
                // left the index fragmented (best-effort — see `bx_import`).
                {
                    let _guard = acquire_write_lock(&job_state.write_lock, &progress);
                    match crate::index::optimize_if_fragmented(&job_state.home, Some(&progress)) {
                        Ok(Some((before, after))) => {
                            tracing::info!("compacted fragmented index: {before} → {after} segments")
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!(
                            "post-import index compaction failed ({e:#}); run `indice optimize` later"
                        ),
                    }
                }
                match job_state.reload_searcher() {
                    Ok(()) => tx
                        .send(ProgressEvent::Done {
                            collection: crate::collections::slugify(&req.into),
                            crawls,
                        })
                        .ok(),
                    Err(e) => tx
                        .send(ProgressEvent::Error {
                            message: format!("indexed, but reloading the searcher failed: {e:#}"),
                        })
                        .ok(),
                }
            }
            Err(e) => tx
                .send(ProgressEvent::Error {
                    message: format!("{e:#}"),
                })
                .ok(),
        };
    });

    (StatusCode::ACCEPTED, Json(AddArchiveResponse { job: id })).into_response()
}
