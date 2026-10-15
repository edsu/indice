//! Serving bytes: WACZ range requests, thumbnails, and the embedded
//! ReplayWeb.page / site static assets.

use rust_embed::RustEmbed;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio_util::io::ReaderStream;

use crate::collections::{CollectionId, Manifest, Wacz};

use super::*;

#[derive(RustEmbed)]
#[folder = "static/replay"]
struct ReplayAssets;

/// Site static assets (the shared stylesheet, etc.), served at `/assets/*`.
#[derive(RustEmbed)]
#[folder = "static/assets"]
struct SiteAssets;

/// Resolve a Browsertrix crawl's WACZ to a fresh presigned URL (cached well
/// under its ~48h expiry) and 302-redirect to it, so wabac.js reads the archived
/// copy directly. 503 if the server has no credentials; 502 if resolution fails.
fn browsertrix_redirect(state: &AppState, col: &Wacz) -> Response {
    // Cache TTL: comfortably under Browsertrix's ~48h presigned-URL expiry.
    const TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

    let Some(resolver) = &state.resolver else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "this server has no Browsertrix credentials to resolve the archived copy",
        )
            .into_response();
    };
    if let Some((url, at)) = state.signed_cache.lock().unwrap().get(&col.id) {
        if at.elapsed() < TTL {
            return axum::response::Redirect::temporary(url).into_response();
        }
    }
    match resolver.resolve(&col.source) {
        Ok(url) => {
            state
                .signed_cache
                .lock()
                .unwrap()
                .insert(col.id.clone(), (url.clone(), std::time::Instant::now()));
            axum::response::Redirect::temporary(&url).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("could not resolve the archived copy from Browsertrix: {e}"),
        )
            .into_response(),
    }
}

pub(super) async fn serve_file(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let collections = load_waczs(&state);
    let Some(col) = collections.iter().find(|c| c.id == id) else {
        return (StatusCode::NOT_FOUND, "collection not found").into_response();
    };

    // Remote sources aren't proxied: wabac.js reads them directly. If /files/{id}
    // is hit for a remote source anyway, redirect to the URL as a convenience.
    if let crate::collections::Source::Url(u) = &col.source {
        return axum::response::Redirect::temporary(u).into_response();
    }
    // A Browsertrix source has no stable URL (its presigned URLs expire), so
    // re-resolve a fresh one (cached) and redirect wabac.js to it.
    if matches!(
        &col.source,
        crate::collections::Source::Browsertrix { .. }
            | crate::collections::Source::BrowsertrixPublic { .. }
    ) {
        return browsertrix_redirect(&state, col);
    }
    // File source: resolve relative paths against home.
    let path = col.source.resolve(&state.home).unwrap();
    if !path.exists() {
        return (StatusCode::NOT_FOUND, "archive file not found on disk").into_response();
    }

    let file_size = col.file_size;
    let range = headers
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| parse_byte_range(s, file_size));

    match tokio::fs::File::open(path).await {
        Ok(mut file) => {
            const CONTENT_TYPE: &str = "application/octet-stream";
            const CORS_EXPOSE: &str = "Content-Length, Content-Range, Accept-Ranges";
            if let Some((start, end)) = range {
                use tokio::io::AsyncSeekExt;
                if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
                    return error_response(anyhow::anyhow!(e)).into_response();
                }
                let length = end - start + 1;
                let limited = tokio::io::AsyncReadExt::take(file, length);
                let body = Body::from_stream(ReaderStream::new(limited));
                Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header("content-type", CONTENT_TYPE)
                    .header("content-length", length)
                    .header("content-range", format!("bytes {start}-{end}/{file_size}"))
                    .header("accept-ranges", "bytes")
                    .header("access-control-allow-origin", "*")
                    .header("access-control-expose-headers", CORS_EXPOSE)
                    .body(body)
                    .unwrap()
            } else {
                let body = Body::from_stream(ReaderStream::new(file));
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", CONTENT_TYPE)
                    .header("content-length", file_size)
                    .header("accept-ranges", "bytes")
                    .header("access-control-allow-origin", "*")
                    .header("access-control-expose-headers", CORS_EXPOSE)
                    .body(body)
                    .unwrap()
            }
        }
        Err(e) => error_response(anyhow::anyhow!(e)).into_response(),
    }
}

fn parse_byte_range(range: &str, file_size: u64) -> Option<(u64, u64)> {
    let s = range.strip_prefix("bytes=")?;
    if let Some(suffix_len) = s.strip_prefix('-') {
        let n: u64 = suffix_len.parse().ok()?;
        let start = file_size.saturating_sub(n);
        Some((start, file_size - 1))
    } else {
        let (start_str, end_str) = s.split_once('-')?;
        let start: u64 = start_str.parse().ok()?;
        let end = if end_str.is_empty() {
            file_size - 1
        } else {
            end_str.parse::<u64>().ok()?.min(file_size - 1)
        };
        Some((start, end))
    }
}

/// The placeholder in `viewer.html` that [`replay_viewer`] fills with the
/// replay origin.
const REPLAY_ORIGIN_SLOT: &str = "{{replay_origin}}";

/// The viewer, told where replay runs. The origin goes into the page rather
/// than coming from the frame, because the viewer checks every message against
/// it: it has to come from configuration, which archived script cannot touch.
pub(super) async fn replay_viewer(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    let Some(origin) = &state.replay_origin else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "replay is not configured: this server has no replay origin",
        )
            .into_response();
    };
    let Some(file) = ReplayAssets::get("viewer.html") else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let html = String::from_utf8_lossy(&file.data)
        .replace(REPLAY_ORIGIN_SLOT, &html_escape(origin.as_str()));
    // The ETag covers the origin too, so a restart with a different replay URL
    // is not answered from a cached page that frames the old one.
    let hash: [u8; 32] = {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        h.update(file.metadata.sha256_hash());
        h.update(origin.as_str().as_bytes());
        h.finalize().into()
    };
    serve_bytes(html.into_bytes().into(), &hash, "viewer.html", &headers)
}

/// What the app origin serves under `/replay/`: the viewer's own script and
/// style. Listed rather than "everything embedded", because the same folder
/// holds ReplayWeb.page, and the app origin must never serve that.
const VIEWER_FILES: &[&str] = &[
    "viewer.js",
    "viewer.css",
    "annotations.js",
    "annotations.css",
];

/// What the replay origin serves under `/replay/`: ReplayWeb.page, its service
/// worker, and the agent that runs beside it in the frame.
const REPLAY_FILES: &[&str] = &["ui.js", "sw.js", "agent.js"];

pub(super) async fn viewer_asset(
    axum::extract::Path(path): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    listed_asset(VIEWER_FILES, &path, &headers)
}

pub(super) async fn replay_asset(
    axum::extract::Path(path): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    if path == "ui.js" {
        return guarded_ui(&headers);
    }
    listed_asset(REPLAY_FILES, &path, &headers)
}

/// ReplayWeb.page, with indice's guard ahead of it in the same file.
///
/// ReplayWeb.page replays whatever WACZ its page's address names, and its
/// shell at `/replay/` is served by the service worker itself once installed,
/// so a check in a page indice serves would miss most visits. Every page that
/// starts ReplayWeb.page loads this file, though, and a script that throws
/// stops there: the guard (`guard.js`) refuses any source but indice's own,
/// and ReplayWeb.page never runs. One file rather than a second `<script>`,
/// because the shell the service worker writes loads only this one. The
/// vendored `ui.js` stays exactly as fetched, so an upgrade is still a copy.
fn guarded_ui(headers: &HeaderMap) -> Response {
    // Built once: the two files are compiled in, so the result never changes,
    // and every replay frame and shell asks for it. A `Bytes` clone is a
    // reference count, not a copy of the 700-odd KB.
    static GUARDED: std::sync::OnceLock<Option<(axum::body::Bytes, [u8; 32])>> =
        std::sync::OnceLock::new();
    let built = GUARDED.get_or_init(|| {
        let (guard, ui) = (ReplayAssets::get("guard.js")?, ReplayAssets::get("ui.js")?);
        let mut body = guard.data.to_vec();
        body.extend_from_slice(b"\n");
        body.extend_from_slice(&ui.data);
        let hash: [u8; 32] = {
            use sha2::Digest;
            let mut h = sha2::Sha256::new();
            h.update(guard.metadata.sha256_hash());
            h.update(ui.metadata.sha256_hash());
            h.finalize().into()
        };
        Some((body.into(), hash))
    });
    match built {
        Some((body, hash)) => serve_bytes(body.clone(), hash, "ui.js", headers),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn listed_asset(list: &[&str], path: &str, headers: &HeaderMap) -> Response {
    if !list.contains(&path) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    serve_embedded_asset(ReplayAssets::get(path), path, headers)
}

/// The frame the viewer embeds: ReplayWeb.page and the agent that reaches into
/// the replayed document. Archived pages run their own JavaScript here, which
/// is why the viewer, not this page, holds the notes panel and every API call.
///
/// Served at `/replay-frame`, **outside** the service worker's `/replay/` scope,
/// and that is load-bearing. At `/replay/frame` the worker controls the page
/// hosting ReplayWeb.page as well as ReplayWeb.page's own inner frames, and on
/// the next load it either missed or failed the inner frame's request: the
/// multi-WACZ test in `browser_collection.rs` landed that frame on indice's
/// homepage, or on a browser error page. Outside the scope it replays, which is
/// also how wacz-exhibitor (Perma.cc's embedding) arranges it.
pub(super) async fn replay_frame(headers: HeaderMap) -> impl IntoResponse {
    serve_embedded_asset(ReplayAssets::get("frame.html"), "frame.html", &headers)
}

/// ReplayWeb.page's app shell, at its replay base. `<replay-web-page>` frames
/// `/replay/?source=…`, which the service worker answers once it controls the
/// page. On a first visit the frame can navigate before the worker has taken
/// control, and then the request reaches us; answering with the shell lets
/// ReplayWeb.page boot and wait for the worker. A redirect elsewhere loses the
/// race for good: the multi-WACZ browser test then shows indice's homepage
/// inside the replay about one run in three.
pub(super) async fn replay_index(headers: HeaderMap) -> impl IntoResponse {
    serve_embedded_asset(ReplayAssets::get("index.html"), "index.html", &headers)
}

/// Serve a site static asset (CSS, etc.) embedded from `static/assets`.
pub(super) async fn asset_handler(
    axum::extract::Path(path): axum::extract::Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    serve_embedded_asset(SiteAssets::get(&path), &path, &headers)
}

/// The on-disk path to a crawl's thumbnail, preferring a curator's committed
/// pinned image (`collections/<slug>/crawls/<id>.jpg`) over the auto-selected
/// cache (`index/thumbs/<id>.jpg`). `None` if neither exists.
fn thumb_path(
    home: &Path,
    index_dir: &Path,
    collection: &CollectionId,
    crawl_id: &str,
) -> Option<PathBuf> {
    let pinned = crate::collections::pinned_thumb_path(home, collection, crawl_id);
    if pinned.is_file() {
        return Some(pinned);
    }
    auto_thumb_path(index_dir, crawl_id)
}

/// The auto-selected thumbnail cache for a crawl (`index/thumbs/<id>.jpg`),
/// which needs no collection — so it still serves when the manifest can't be
/// read.
fn auto_thumb_path(index_dir: &Path, crawl_id: &str) -> Option<PathBuf> {
    let auto = index_dir.join("thumbs").join(format!("{crawl_id}.jpg"));
    auto.is_file().then_some(auto)
}

/// The `/thumb/{id}` href for a crawl, or `None` if it has no thumbnail (the UI
/// then shows a CSS placeholder). `id` is a crawl id.
pub(super) fn thumb_href(
    home: &Path,
    index_dir: &Path,
    collection: &CollectionId,
    crawl_id: &str,
) -> Option<String> {
    thumb_path(home, index_dir, collection, crawl_id).map(|_| format!("/thumb/{crawl_id}"))
}

/// Serve a crawl's representative thumbnail (a committed pinned image under the
/// collection, else the auto cache under `index/thumbs`). 404 when it has none.
pub(super) async fn thumb_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    // Crawl ids are hex hashes; reject anything else so the id can't escape the
    // thumbs directory.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Resolve the crawl's collection (needed for the committed pinned path).
    // Resolve the crawl's collection, needed for the committed *pinned* path.
    // A crawl the manifest doesn't know can't have one, so 404 — but an
    // unreadable manifest is a different thing entirely: fall through so the
    // auto cache at index/thumbs/<id>.jpg still serves, rather than silently
    // stripping every thumbnail from the grid.
    let collection = match Manifest::open(&state.index_dir) {
        Ok(m) => match m.wacz_by_id(&id) {
            Some(w) => Some(w.collection.clone()),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
        Err(_) => None,
    };
    let Some(path) = collection
        .and_then(|c| thumb_path(&state.home, &state.index_dir, &c, &id))
        .or_else(|| auto_thumb_path(&state.index_dir, &id))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match std::fs::read(path) {
        Ok(bytes) => (
            [
                (axum::http::header::CONTENT_TYPE, "image/jpeg"),
                (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The `/collection-thumb/{slug}` href for a collection, if a curator committed
/// one at `collections/<slug>/thumbnail.jpg`.
pub(super) fn collection_thumb_href(home: &Path, slug: &CollectionId) -> Option<String> {
    crate::collections::collection_thumb_path(home, slug)
        .is_file()
        .then(|| format!("/collection-thumb/{slug}"))
}

/// Serve a collection's curator-set representative thumbnail. 404 when unset.
pub(super) async fn collection_thumb_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    // CollectionId::parse is the single definition of a safe id; an id that
    // fails it can't name a real collection either, so 404 as before.
    let Some(id) = CollectionId::parse(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match std::fs::read(crate::collections::collection_thumb_path(&state.home, &id)) {
        Ok(bytes) => (
            [
                (axum::http::header::CONTENT_TYPE, "image/jpeg"),
                (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Serve an embedded ReplayWebPage asset with an ETag derived from its content
/// hash and `Cache-Control: no-cache`. Browsers must revalidate on every load,
/// so a rebuild that changes an asset (e.g. `viewer.html`, `sw.js`) propagates
/// to clients on their next request instead of being masked by the HTTP cache.
/// When the client's `If-None-Match` matches, we return `304` with no body so
/// unchanged assets aren't re-downloaded.
fn serve_embedded_asset(
    content: Option<rust_embed::EmbeddedFile>,
    path: &str,
    req_headers: &HeaderMap,
) -> Response {
    match content {
        Some(content) => serve_bytes(
            match content.data {
                // Compiled in, in a release build: no copy at all.
                std::borrow::Cow::Borrowed(b) => axum::body::Bytes::from_static(b),
                std::borrow::Cow::Owned(v) => v.into(),
            },
            &content.metadata.sha256_hash(),
            path,
            req_headers,
        ),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// Serve `data` with an ETag from `hash`, answering a matching
/// `If-None-Match` with 304.
fn serve_bytes(
    data: axum::body::Bytes,
    hash: &[u8],
    path: &str,
    req_headers: &HeaderMap,
) -> Response {
    let etag = etag_for(hash);
    let matches = req_headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .map(|inm| inm == etag)
        .unwrap_or(false);
    if matches {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header("etag", &etag)
            .header("cache-control", "no-cache")
            .body(Body::empty())
            .unwrap();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", mime_guess_from_path(path))
        .header("etag", etag)
        .header("cache-control", "no-cache")
        .body(Body::from(data))
        .unwrap()
}

/// Build a quoted ETag from the first 8 bytes of a content hash.
fn etag_for(hash: &[u8]) -> String {
    let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

fn mime_guess_from_path(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") || path.ends_with(".mjs") {
        "application/javascript"
    } else if path.ends_with(".css") {
        "text/css"
    } else if path.ends_with(".wasm") {
        "application/wasm"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    }
}
