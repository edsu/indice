use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::Result;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use crate::annotations::{self, EditOutcome, UpdateResult};
use crate::collections::{Manifest, Wacz};
use crate::search::SearchIndex;
use crate::views;

// ── Embedded static assets ────────────────────────────────────────────────────

#[derive(RustEmbed)]
#[folder = "static/replay"]
struct ReplayAssets;

/// Site static assets (the shared stylesheet, etc.), served at `/assets/*`.
#[derive(RustEmbed)]
#[folder = "static/assets"]
struct SiteAssets;

// ── Management-mode configuration ───────────────────────────────────────────

/// How `serve --manage` authenticates the write surface.
///
/// - **Local** (`forward_auth: None`): every request is trusted. Only valid on a
///   loopback bind (enforced at startup) — the local operator is the admin, no
///   login. This is the laptop / single-user case.
/// - **Forward-auth** (`forward_auth: Some`): indice sits behind an authenticating
///   reverse proxy that performs the real login (SSO/OIDC/SAML) and injects the
///   authenticated user in a header. indice trusts that header **only** when the
///   request also carries the shared secret in `X-Indice-Auth-Secret` (which the
///   proxy adds), so a client that forges the identity header — or a request that
///   never went through the proxy — is rejected. This is the institutional
///   "install as a service" case; indice stores no passwords and speaks to no IdP.
#[derive(Clone, Default)]
pub struct ManageConfig {
    /// Whether the management routes are mounted at all (`--manage`).
    pub enabled: bool,
    pub forward_auth: Option<ForwardAuth>,
    /// Where `/logout` sends the browser after clearing indice's display cookie.
    /// `None` → `/` (the basic-auth stopgap). Behind an SSO proxy, set this to the
    /// proxy's sign-out URL (e.g. `/oauth2/sign_out?rd=/`) so a single click ends
    /// both indice's display session and the proxy's login session.
    pub logout_redirect: Option<String>,
}

/// Forward-auth settings: which header carries the authenticated user, and the
/// shared secret the trusted proxy must present alongside it.
#[derive(Clone)]
pub struct ForwardAuth {
    /// Header the proxy injects with the authenticated identity, e.g.
    /// `X-Forwarded-Email` (oauth2-proxy), `Remote-Email` (Authelia).
    pub user_header: String,
    /// Secret the proxy must send in `X-Indice-Auth-Secret`. Static, proxy-side
    /// config (not the IdP); its presence is what makes trusting the identity
    /// header safe.
    pub secret: String,
}

impl ManageConfig {
    /// Management disabled — the default read-only server.
    pub fn off() -> Self {
        Self::default()
    }
    /// Management on, local mode (trust every request; requires a loopback bind).
    pub fn local() -> Self {
        Self {
            enabled: true,
            forward_auth: None,
            logout_redirect: None,
        }
    }
    /// Management on, gated behind a trusted auth proxy.
    pub fn forward_auth(user_header: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            enabled: true,
            forward_auth: Some(ForwardAuth {
                user_header: user_header.into(),
                secret: secret.into(),
            }),
            logout_redirect: None,
        }
    }
}

/// The fixed header carrying the proxy↔indice shared secret in forward-auth mode.
const AUTH_SECRET_HEADER: &str = "x-indice-auth-secret";

/// Cookie indice sets to remember a signed-in identity for **display** on the
/// ungated public pages — the browser won't send the proxy's Basic-auth
/// credentials to `/`, so the workroom chrome would otherwise never appear there.
/// HMAC-signed with the forward-auth secret (see [`sign_session`]); it drives
/// *rendering only* — every write is still re-checked against the proxy headers.
const SESSION_COOKIE: &str = "indice_session";
/// How long a signed display cookie is honored (the expiry baked into its
/// signature; refreshed on every gated request). The cookie itself is a *session*
/// cookie — no `Max-Age` — so it's dropped when the browser closes, matching the
/// lifetime of the browser's cached Basic-auth credentials and avoiding a stale
/// cookie that outlives them.
const SESSION_TTL_SECS: u64 = 12 * 60 * 60;

// ── AppState ──────────────────────────────────────────────────────────────────

struct AppState {
    /// Read-only searcher, behind an `RwLock<Arc<…>>` so management-mode ingestion
    /// can hot-reload it after a commit without restarting the server. Read-mostly:
    /// search handlers take the read lock only long enough to clone the `Arc` (then
    /// query against the snapshot); [`AppState::reload_searcher`] takes the write
    /// lock just long enough to swap in a freshly-opened index.
    search: RwLock<Arc<SearchIndex>>,
    /// indice home directory; local WACZ sources resolve against it.
    home: PathBuf,
    /// `<home>/index`, where the manifest and full-text index live.
    index_dir: PathBuf,
    /// Resolves refreshable remote sources (Browsertrix) to fresh presigned URLs
    /// for replay. `None` if the server was started without credentials.
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    /// Cache of resolved presigned URLs by crawl id, with when they were fetched
    /// — Browsertrix URLs expire (~48h), so this is refreshed well before that.
    signed_cache: std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>,
    /// Serializes management writes: [`crate::index::index_location`] takes
    /// Tantivy's exclusive write lock, so two concurrent adds would contend. One
    /// in-flight add at a time is plenty for the single-user desktop case this
    /// mode targets.
    write_lock: std::sync::Mutex<()>,
    /// Progress channels for in-flight add-archive jobs, drained once by the SSE
    /// endpoint. Keyed by an incrementing job id ([`AppState::job_counter`]).
    jobs: std::sync::Mutex<HashMap<u64, mpsc::UnboundedReceiver<ProgressEvent>>>,
    job_counter: AtomicU64,
    /// Whether management mode is on. The write *routes* are gated at mount time
    /// (below), but the read handlers also read this to decide whether to render
    /// management affordances (the `/manage` link, the empty-state CTA).
    management: bool,
    /// Forward-auth settings, when management runs behind an auth proxy. Handlers
    /// read the `user_header` to show who's signed in; the route middleware does
    /// the actual enforcement.
    forward_auth: Option<ForwardAuth>,
    /// Where `/logout` redirects after clearing the display cookie (`None` → `/`).
    /// Set to the SSO proxy's sign-out URL for a real single-click logout.
    logout_redirect: Option<String>,
    /// Builds authenticated Browsertrix clients for the import UI (binary-provided
    /// with env credentials). `None` when no Browsertrix credentials are set —
    /// the import endpoints then report that it's unconfigured.
    browsertrix: Option<Arc<dyn crate::browsertrix::BrowsertrixProvider>>,
    /// Builds authenticated Archive-It clients for the import UI, same boundary as
    /// `browsertrix`. `None` when no `ARCHIVEIT_*` credentials are set.
    archiveit: Option<Arc<dyn crate::archiveit::ArchiveItProvider>>,
}

/// Import providers wired in by the binary (used only by the management UI).
/// Bundled into one value so the `serve`/`router` constructors don't grow a
/// parameter per provider as more import sources are added.
#[derive(Default, Clone)]
pub struct Providers {
    pub browsertrix: Option<Arc<dyn crate::browsertrix::BrowsertrixProvider>>,
    pub archiveit: Option<Arc<dyn crate::archiveit::ArchiveItProvider>>,
}

impl AppState {
    /// Re-open the read-only search index and swap it in, so documents committed
    /// by a management-mode ingest become visible to search without a restart.
    /// Called from the blocking add-archive task after `index_location` commits.
    fn reload_searcher(&self) -> Result<()> {
        let fresh = SearchIndex::open_read_only(self.index_dir.join("full_text").as_path())?;
        *self.search.write().unwrap() = Arc::new(fresh);
        Ok(())
    }
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router(home: &Path) -> Result<Router> {
    build_router(home, None, ManageConfig::off(), Providers::default())
}

/// Like [`router`], but with a [`crate::index::SourceResolver`] so the server can
/// replay Browsertrix sources (re-resolving fresh presigned URLs on demand).
pub fn router_with_resolver(
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
) -> Result<Router> {
    build_router(home, resolver, ManageConfig::off(), Providers::default())
}

/// Build the app router. `manage` gates the opt-in write routes: when disabled
/// (the default for `serve`) only the read-only site is mounted, so the public
/// deployment can never mutate the archive; when enabled (`serve --manage`) the
/// management endpoints are added on top, and — in forward-auth mode — wrapped in
/// the [`forward_auth`] middleware so every management request must carry the
/// trusted proxy's identity header and shared secret.
fn build_router(
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    manage: ManageConfig,
    providers: Providers,
) -> Result<Router> {
    let index_dir = crate::index::index_dir(home);
    // Read-only: the server never holds Tantivy's exclusive write lock, so
    // `indice index` (and, in management mode, an add-archive job in a separate
    // writer) can run while serving. Management writes reload this searcher after
    // they commit — see [`AppState::reload_searcher`].
    let search = SearchIndex::open_read_only(index_dir.join("full_text").as_path())?;
    // A fragmented index (many segments — e.g. a big ingest whose background
    // merges didn't keep up, or one built by an older version) slows every
    // query; nudge the operator to compact it. Best-effort: a count error here
    // must not stop the server from starting.
    if let Ok(n) = search.segment_count() {
        if n > crate::index::FRAGMENTED_SEGMENT_THRESHOLD {
            tracing::warn!("{}", crate::index::fragmentation_warning(n));
        }
    }
    let state = Arc::new(AppState {
        search: RwLock::new(Arc::new(search)),
        home: home.to_path_buf(),
        index_dir,
        resolver,
        signed_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        write_lock: std::sync::Mutex::new(()),
        jobs: std::sync::Mutex::new(HashMap::new()),
        job_counter: AtomicU64::new(0),
        management: manage.enabled,
        forward_auth: manage.forward_auth.clone(),
        logout_redirect: manage.logout_redirect.clone(),
        browsertrix: providers.browsertrix,
        archiveit: providers.archiveit,
    });

    let mut app = Router::new()
        .route("/", get(homepage))
        .route("/health", get(health))
        // Public: clears the display session cookie (logout). Harmless when no
        // cookie is set; deliberately outside the forward-auth-gated routes.
        .route("/logout", get(logout))
        .route("/search", get(search_page))
        .route("/collection/{id}", get(collection_page))
        .route("/collection/{id}/replay.json", get(collection_replay_json))
        .route("/collection/{id}/pages", get(collection_pages))
        .route("/collection/{id}/annotations", get(collection_annotations))
        .route("/crawl/{id}", get(crawl_page))
        .route("/thumb/{id}", get(thumb_handler))
        .route("/collection-thumb/{id}", get(collection_thumb_handler))
        .route("/files/{id}", get(serve_file))
        .route("/replay/viewer", get(replay_viewer))
        .route("/api/search", get(search_api))
        // Public read of page annotations (display is public; writes are gated below).
        .route("/api/annotations", get(list_annotations))
        .route("/assets/{*path}", get(asset_handler))
        .route("/replay/", get(replay_index))
        .route("/replay/{*path}", get(replay_handler));

    // Opt-in write surface: mounted only under `serve --manage`. The browser
    // management UI plus its write endpoints — add a crawl (`index_location`,
    // streaming progress over SSE), upload a WACZ, and create/edit a collection
    // finding aid (`set_collection`). None of this exists in the default
    // read-only server.
    if manage.enabled {
        let mut manage_routes = Router::new()
            .route("/manage/collections/new", get(new_collection_form))
            .route("/manage/edit/{id}", get(edit_collection_form))
            .route("/manage/add", get(accession_desk_page))
            // A login entry point: being gated, visiting it forces the proxy's
            // login, then bounces back to where the user came from.
            .route("/manage/login", get(manage_login))
            .route("/api/archives", post(add_archive))
            // File upload can be large (a whole WACZ), so lift axum's 2 MB default
            // body limit on this route only.
            .route(
                "/api/archives/upload",
                post(upload_archive).layer(DefaultBodyLimit::disable()),
            )
            .route("/api/archives/{id}/events", get(add_archive_events))
            .route("/api/collections", post(create_collection))
            // Delete a crawl or a collection (removes files + updates the index).
            .route("/api/crawls/{id}/delete", post(delete_crawl_handler))
            .route(
                "/api/collections/{id}/delete",
                post(delete_collection_handler),
            )
            // Browsertrix import: browse (orgs → collections → items) using the
            // binary-supplied credentials, then import selected items as a job.
            .route("/api/browsertrix/orgs", get(bx_orgs))
            .route("/api/browsertrix/collections", get(bx_collections))
            .route("/api/browsertrix/items", get(bx_items))
            .route("/api/browsertrix/import", post(bx_import))
            // Archive-It import: browse (collections → crawls) using the
            // binary-supplied credentials, then import selected crawls as a job.
            .route("/api/archiveit/collections", get(ait_collections))
            .route("/api/archiveit/crawls", get(ait_crawls))
            .route("/api/archiveit/import", post(ait_import))
            // Page annotations: create/edit/delete, gated like the rest. The
            // public GET /api/annotations lives in the read block above.
            .route("/api/annotations", post(create_annotation))
            .route("/api/annotations/{id}", post(update_annotation))
            .route("/api/annotations/{id}/delete", post(delete_annotation));

        // Forward-auth: reject any management request that doesn't carry the
        // trusted proxy's shared secret + a non-empty identity header. Layered
        // outermost so it runs before a body is read (e.g. a large upload).
        if let Some(fa) = manage.forward_auth {
            let guard = Arc::new(fa);
            manage_routes = manage_routes.layer(axum::middleware::from_fn(
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    let guard = guard.clone();
                    async move { forward_auth(&guard, req, next).await }
                },
            ));
        }
        app = app.merge(manage_routes);
    }

    let app = app
        // Mark rendered HTML non-cacheable (it varies by auth); innermost so it
        // tags the handler's response before compression. Runs before with_state.
        .layer(axum::middleware::from_fn(html_no_cache))
        .layer(CompressionLayer::new())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::http::Request<Body>| {
                    let ip = req
                        .extensions()
                        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                        .map(|ci| ci.0.ip().to_string())
                        .unwrap_or_else(|| "-".to_string());
                    tracing::info_span!(
                        "request",
                        method = %req.method(),
                        uri = %req.uri(),
                        client_ip = %ip,
                    )
                })
                .on_response(
                    |res: &Response, latency: std::time::Duration, _span: &tracing::Span| {
                        let ct = res
                            .headers()
                            .get(axum::http::header::CONTENT_TYPE)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("-");
                        let status = res.status().as_u16();
                        let ms = latency.as_millis();
                        if status >= 500 {
                            tracing::error!(status, content_type = ct, latency_ms = ms);
                        } else if status >= 400 {
                            tracing::warn!(status, content_type = ct, latency_ms = ms);
                        } else {
                            tracing::info!(status, content_type = ct, latency_ms = ms);
                        }
                    },
                ),
        )
        .with_state(state);

    Ok(app)
}

pub async fn serve(bind: &str, home: &Path) -> Result<()> {
    serve_with_resolver(bind, home, None, ManageConfig::off(), Providers::default()).await
}

/// Like [`serve`], but with a [`crate::index::SourceResolver`] so Browsertrix
/// sources can be replayed (fresh presigned URLs resolved on demand). `manage`
/// configures the opt-in write routes (see [`build_router`]); `providers`
/// supplies authenticated import clients (Browsertrix, Archive-It) for the UI.
pub async fn serve_with_resolver(
    bind: &str,
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    manage: ManageConfig,
    providers: Providers,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("listening on {bind}");
    serve_on_listener(listener, home, resolver, manage, providers).await
}

/// Serve on an already-bound listener. This lets a caller bind `127.0.0.1:0`,
/// read back the OS-assigned port via [`TcpListener::local_addr`], and only then
/// serve — which is exactly what the desktop app shell needs so it can point the
/// window at `http://127.0.0.1:<port>` before the server starts accepting.
///
/// [`TcpListener::local_addr`]: tokio::net::TcpListener::local_addr
pub async fn serve_on_listener(
    listener: tokio::net::TcpListener,
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    manage: ManageConfig,
    providers: Providers,
) -> Result<()> {
    // Safety guard: local management mode (no auth proxy) trusts every request, so
    // it must not be reachable beyond this machine. Refuse to start if it's bound
    // to a non-loopback address without forward-auth configured — otherwise it
    // would expose an unauthenticated write surface. To run as a service, put an
    // authenticating proxy in front and configure forward-auth.
    if manage.enabled && manage.forward_auth.is_none() {
        let addr = listener.local_addr()?;
        if !addr.ip().is_loopback() {
            anyhow::bail!(
                "refusing to start: management mode (--manage) without an auth proxy \
                 trusts every request, so it must bind to a loopback address \
                 (127.0.0.1 / ::1), but it is bound to {addr}. To run as a service, \
                 front it with an authenticating reverse proxy and set \
                 --auth-proxy-header / --auth-proxy-secret."
            );
        }
    }
    let app = build_router(home, resolver, manage, providers)?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Forward-auth middleware for the management routes: allow the request through
/// only if it carries the shared secret in `X-Indice-Auth-Secret` (matching the
/// configured value) **and** a non-empty identity in the configured user header —
/// both injected by the trusted proxy. Anything else (a forged identity header, a
/// request that skipped the proxy) gets 403.
async fn forward_auth(
    fa: &ForwardAuth,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    match check_forward_auth(fa, req.headers()) {
        Some(user) => {
            // Mark the display cookie Secure when the external hop was HTTPS (Caddy
            // sets X-Forwarded-Proto), so it's never sent in the clear in prod.
            let secure = forwarded_https(req.headers());
            let mut res = next.run(req).await;
            set_session_cookie(&mut res, fa, &user, secure);
            res
        }
        None => (
            StatusCode::FORBIDDEN,
            "forbidden: this management surface requires authentication via its front proxy",
        )
            .into_response(),
    }
}

/// Validate a forward-auth request: returns the authenticated identity iff the
/// request carries the shared secret (in `X-Indice-Auth-Secret`) **and** a
/// non-empty identity in the configured user header — both injected by the
/// trusted proxy. A forged identity header, or a request that skipped the proxy,
/// lacks the secret and yields `None`.
fn check_forward_auth(fa: &ForwardAuth, headers: &HeaderMap) -> Option<String> {
    let secret_ok = headers
        .get(AUTH_SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| constant_time_eq(v.as_bytes(), fa.secret.as_bytes()))
        .unwrap_or(false);
    if !secret_ok {
        return None;
    }
    headers
        .get(fa.user_header.as_str())
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Whether this request may use management affordances, plus the signed-in user.
/// Local mode trusts every request (it's loopback-only); forward-auth defers to
/// [`check_forward_auth`]. Used by the read handlers to decide whether to render
/// the workroom chrome and edit-in-place controls.
fn admin_ctx(state: &AppState, headers: &HeaderMap) -> (bool, Option<String>) {
    if !state.management {
        return (false, None);
    }
    match &state.forward_auth {
        None => (true, None),
        Some(fa) => {
            // The live proxy-injected identity (management routes), or — on the
            // ungated public pages the browser can't send proxy creds to — the
            // display cookie indice set at login. The cookie drives *rendering*
            // only; write routes always re-check the proxy headers.
            let user = check_forward_auth(fa, headers).or_else(|| session_cookie_user(fa, headers));
            match user {
                Some(u) => (true, Some(u)),
                None => (false, None),
            }
        }
    }
}

/// Whether to offer a "Log in" link: forward-auth is configured but this request
/// is anonymous. Centralizes the rule the four page handlers share (`who` is the
/// identity from [`admin_ctx`]).
fn login_available(state: &AppState, who: &Option<String>) -> bool {
    state.forward_auth.is_some() && who.is_none()
}

/// Constant-time byte comparison, to avoid leaking the secret via timing. The
/// length check can leak length, which is fine for a shared secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// HMAC-SHA256 — the standard construction over the `sha2` hash already used for
/// WACZ fixity, so we don't pull in a separate hmac crate. Pinned by an RFC 4231
/// known-answer test (see the tests module).
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(msg)
        .finalize();
    let outer = Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer);
    out
}

/// Build a signed display-cookie value for `user`, valid until `exp` (unix secs):
/// `b64url(user)|exp|b64url(hmac)`, where the HMAC covers `b64url(user)|exp`.
fn sign_session(secret: &str, user: &str, exp: u64) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = format!("{}|{}", b64.encode(user), exp);
    let sig = b64.encode(hmac_sha256(secret.as_bytes(), payload.as_bytes()));
    format!("{payload}|{sig}")
}

/// Verify a display-cookie value against `secret` at time `now`; returns the
/// identity iff the signature matches (constant-time) and it hasn't expired.
fn verify_session(secret: &str, value: &str, now: u64) -> Option<String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let (payload, sig) = value.rsplit_once('|')?;
    let expected = b64.encode(hmac_sha256(secret.as_bytes(), payload.as_bytes()));
    if !constant_time_eq(sig.as_bytes(), expected.as_bytes()) {
        return None;
    }
    let (user_b64, exp) = payload.split_once('|')?;
    if exp.parse::<u64>().ok()? <= now {
        return None;
    }
    let user = b64.decode(user_b64).ok()?;
    String::from_utf8(user).ok().filter(|s| !s.is_empty())
}

/// Seconds since the Unix epoch (0 if the clock is before it, which won't happen).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether the external hop reached the proxy over HTTPS (Caddy sets
/// `X-Forwarded-Proto`) — used to mark the session cookie `Secure` in production.
fn forwarded_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https"))
}

/// Attach the signed display cookie to a management response, so subsequent
/// requests to the ungated public pages can render the workroom chrome.
fn set_session_cookie(res: &mut Response, fa: &ForwardAuth, user: &str, secure: bool) {
    let value = sign_session(&fa.secret, user, now_secs() + SESSION_TTL_SECS);
    let mut cookie = format!("{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax");
    if secure {
        cookie.push_str("; Secure");
    }
    if let Ok(hv) = axum::http::HeaderValue::from_str(&cookie) {
        res.headers_mut().append(axum::http::header::SET_COOKIE, hv);
    }
}

/// Expire the display cookie (logout). Mirrors the attributes used when setting it
/// so browsers reliably drop it.
fn clear_session_cookie(res: &mut Response, secure: bool) {
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax");
    if secure {
        cookie.push_str("; Secure");
    }
    if let Ok(hv) = axum::http::HeaderValue::from_str(&cookie) {
        res.headers_mut().append(axum::http::header::SET_COOKIE, hv);
    }
}

/// Read + verify the display cookie from a request's `Cookie` header, if present.
fn session_cookie_user(fa: &ForwardAuth, headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    let value = cookies
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, v)| v)?;
    verify_session(&fa.secret, value, now_secs())
}

/// `GET /manage/login` — a login entry point for forward-auth deployments. It is
/// mounted under the gated management routes, so merely reaching it forces the
/// front proxy's login (a Basic-auth prompt, or an SSO redirect). Once
/// authenticated it bounces the browser back to the page it came from (the
/// `Referer`, if it's a local path) so that page re-renders with its management
/// chrome. In local-trust mode there is no login, so it just redirects home.
async fn manage_login(headers: HeaderMap) -> Response {
    let dest = headers
        .get(axum::http::header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(local_redirect_target)
        .unwrap_or_else(|| "/".to_string());
    axum::response::Redirect::to(&dest).into_response()
}

/// Extract a safe **same-site path** from a `Referer` so `/manage/login` can't be
/// turned into an open redirect. Takes only the path (+query) of an absolute
/// Referer and requires a single leading slash — never an off-site URL, and never
/// a protocol-relative `//host` (nor its `/\host` backslash variant, which some
/// browsers normalize to `//`). Returns `None` if it can't (caller falls to `/`).
fn local_redirect_target(referer: &str) -> Option<String> {
    // Absolute Referer: scheme://host[:port]/path?query — drop scheme+host, keep
    // from the first '/' of the path onward.
    let after_scheme = referer.split_once("://")?.1;
    let path = &after_scheme[after_scheme.find('/')?..];
    let offsite = path.starts_with("//") || path.starts_with("/\\");
    (path.starts_with('/') && !offsite).then(|| path.to_string())
}

/// `GET /logout` — clear the display session cookie, then redirect. Public and
/// un-gated on purpose: logging out shouldn't require auth, and it must NOT pass
/// through the forward-auth middleware (which would immediately re-set the
/// cookie). By default it redirects to `/`; behind an SSO proxy `logout_redirect`
/// points at the proxy's sign-out (e.g. `/oauth2/sign_out?rd=/`) so one click ends
/// both sessions. With the HTTP Basic stopgap there's no proxy sign-out, so the
/// browser keeps its cached credentials until it's closed — logout only hides the
/// chrome there.
async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let dest = state.logout_redirect.as_deref().unwrap_or("/");
    let mut res = Redirect::to(dest).into_response();
    clear_session_cookie(&mut res, forwarded_https(&headers));
    res
}

/// Mark server-rendered HTML pages non-cacheable. Their content varies by
/// authentication (the workroom chrome + signed-in identity), so a cached copy
/// could show the wrong variant — e.g. an anonymous homepage lingering after you
/// sign in, or (via the back/forward cache) a signed-in page restored after
/// logout. `no-store` is used rather than `no-cache` precisely because it also
/// makes the page ineligible for bfcache, so Back after logout refetches the
/// anonymous view. Only text/html is touched — WACZ bytes (`/files`) and static
/// assets stay cacheable.
async fn html_no_cache(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut res = next.run(req).await;
    let is_html = res
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if is_html
        && !res
            .headers()
            .contains_key(axum::http::header::CACHE_CONTROL)
    {
        res.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
    }
    res
}

// ── Management mode: add-archive ────────────────────────────────────────────
//
// Opt-in (`serve --manage`) write surface. `POST /api/archives` starts an ingest
// job that reuses the exact library path the CLI uses (`index::index_location`),
// running it on a blocking thread and returning a job id immediately. The browser
// then streams `GET /api/archives/{id}/events` (Server-Sent Events) to watch
// progress. On success the read-only searcher is hot-reloaded so results appear
// without a restart. None of this is mounted in the default read-only server.

/// Progress events streamed to the management UI over SSE while an add-archive
/// job runs. The first six mirror [`crate::index::IndexProgress`]; `done`/`error`
/// are the terminal outcomes. Serialized as a tagged JSON object, e.g.
/// `{"type":"total","total":1234}`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ProgressEvent {
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
struct ChannelProgress {
    tx: mpsc::UnboundedSender<ProgressEvent>,
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
struct AddArchiveRequest {
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
struct AddArchiveResponse {
    /// Id to stream progress from at `/api/archives/{job}/events`.
    job: u64,
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
fn acquire_write_lock<'a>(
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

fn start_index_job(
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
    // `index_location` blocks (file IO, network range reads, the Tantivy commit),
    // so run it off the async runtime — never on a request-handling thread.
    tokio::task::spawn_blocking(move || {
        // Held until the job ends; dropping it deletes any uploaded temp file
        // (after `index_location` has copied it into `archive/`).
        let _keepalive = keepalive;
        let progress = ChannelProgress { tx: tx.clone() };
        let result = {
            let _guard = acquire_write_lock(&job_state.write_lock, &progress);
            crate::index::index_location(
                &location,
                &job_state.home,
                name.as_deref(),
                &collection,
                false, // download
                false, // force
                None,
                Some(&progress),
            )
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
async fn add_archive(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddArchiveRequest>,
) -> Response {
    // Mirror the CLI's "every crawl belongs to a collection" guard.
    if req.collection.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "collection is required").into_response();
    }
    let id = start_index_job(&state, req.path, req.collection, req.name, None);
    (StatusCode::ACCEPTED, Json(AddArchiveResponse { job: id })).into_response()
}

/// `POST /api/archives/upload` — add a crawl by uploading the `.wacz` bytes
/// (multipart/form-data: `collection`, optional `name`, and the `file`). The
/// upload is streamed to a temp file, then indexed exactly like a local path
/// (`index_location` copies it into `archive/`); the temp file is deleted when
/// the job finishes. Returns a job id (202) to stream progress from.
async fn upload_archive(State(state): State<Arc<AppState>>, mut multipart: Multipart) -> Response {
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
    let id = start_index_job(&state, location, collection, name, tmpdir);
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
async fn add_archive_events(
    State(state): State<Arc<AppState>>,
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
async fn new_collection_form(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let (_, who) = admin_ctx(&state, &headers);
    views::collection_form(&views::CollectionFormData::default(), who.as_deref()).into_response()
}

/// `GET /manage/edit/{id}` — the finding-aid form pre-filled for an existing
/// collection (name locked, since the slug is its identity).
async fn edit_collection_form(
    State(state): State<Arc<AppState>>,
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
        id: c.id.clone(),
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
struct AddQuery {
    #[serde(default)]
    collection: String,
}

/// `GET /manage/add` — the add-crawls accession desk, with the target collection
/// prefilled when arriving from a collection page.
async fn accession_desk_page(
    State(state): State<Arc<AppState>>,
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

// ── Management mode: Browsertrix import ─────────────────────────────────────

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
struct BxBrowse {
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
async fn bx_orgs(State(state): State<Arc<AppState>>) -> Response {
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
async fn bx_collections(State(state): State<Arc<AppState>>, Query(q): Query<BxBrowse>) -> Response {
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
fn imported_browsertrix_ids<'a>(
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
async fn bx_items(State(state): State<Arc<AppState>>, Query(q): Query<BxBrowse>) -> Response {
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
struct BxImportRequest {
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
async fn bx_import(
    State(state): State<Arc<AppState>>,
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

    let id = state.job_counter.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::unbounded_channel::<ProgressEvent>();
    state.jobs.lock().unwrap().insert(id, rx);

    let job_state = state.clone();
    tokio::task::spawn_blocking(move || {
        let progress = ChannelProgress { tx: tx.clone() };
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
                    crawls.push(serde_json::json!({ "id": crawl_id, "name": label }));
                }
            }
            Ok(crawls)
        })();
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

// ── Archive-It import (management UI) ──────────────────────────────────────────

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
async fn ait_collections(State(state): State<Arc<AppState>>) -> Response {
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
struct AitBrowse {
    collection: Option<i64>,
}

/// `GET /api/archiveit/crawls?collection=<id>` — a collection's importable crawls
/// (finished, not deleted), each marked if already imported into this instance.
async fn ait_crawls(State(state): State<Arc<AppState>>, Query(q): Query<AitBrowse>) -> Response {
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
struct AitImportRequest {
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
async fn ait_import(
    State(state): State<Arc<AppState>>,
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

    let id = state.job_counter.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::unbounded_channel::<ProgressEvent>();
    state.jobs.lock().unwrap().insert(id, rx);

    let job_state = state.clone();
    tokio::task::spawn_blocking(move || {
        let progress = ChannelProgress { tx: tx.clone() };
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
            Ok(outcome
                .crawls
                .into_iter()
                .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
                .collect())
        })();
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

/// Form body for create/edit collection (`application/x-www-form-urlencoded`).
/// All finding-aid fields are optional; `subjects` is a comma-separated list.
#[derive(Deserialize)]
struct CollectionForm {
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
async fn create_collection(
    State(state): State<Arc<AppState>>,
    Form(form): Form<CollectionForm>,
) -> Response {
    let name = form.name.trim().to_string();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "collection name is required").into_response();
    }
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
    // set_collection writes the README + manifest — quick, but blocking, so keep
    // it off the async runtime. The homepage re-reads the manifest per request,
    // so the new/edited collection shows immediately (no searcher reload needed).
    let result =
        tokio::task::spawn_blocking(move || crate::index::set_collection(&home, &name, &fields))
            .await;
    match result {
        Ok(Ok(id)) => Redirect::to(&format!("/collection/{id}")).into_response(),
        Ok(Err(e)) => error_response(e).into_response(),
        Err(e) => error_response(anyhow::anyhow!(e)).into_response(),
    }
}

// ── Management mode: delete ─────────────────────────────────────────────────

/// `POST /api/crawls/{id}/delete` — remove a crawl (index docs, manifest entry,
/// local WACZ, thumbnail), reload the searcher, and return to its collection.
async fn delete_crawl_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
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
struct DeleteCollectionForm {
    #[serde(default)]
    with_crawls: Option<String>,
}

/// `POST /api/collections/{id}/delete` — remove a collection grouping (and, with
/// `with_crawls`, its member crawls), reload the searcher, and return home.
async fn delete_collection_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Form(form): Form<DeleteCollectionForm>,
) -> Response {
    let with_crawls = form
        .with_crawls
        .as_deref()
        .is_some_and(|v| matches!(v, "true" | "on" | "1"));
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Refusing a non-empty collection is a client choice, not a server fault,
        // so surface it as 409 rather than letting the lib error become a 500.
        let plan = crate::index::plan_collection_deletion(&state.home, &id)?;
        if plan.member_count > 0 && !with_crawls {
            return Ok(DeleteOutcome::Refused(plan.member_count));
        }
        let _guard = state.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        crate::index::delete_collection(&state.home, &id, with_crawls)?;
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

// ── Health ──────────────────────────────────────────────────────────────────

/// Liveness/readiness probe for a reverse proxy or orchestrator (Docker
/// HEALTHCHECK, Kubernetes, YunoHost, …). Deliberately trivial and un-gated: the
/// server only starts once the index has opened, so a 200 here means the process
/// is up and serving. Returns `ok` as plain text.
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

// ── Homepage ──────────────────────────────────────────────────────────────────

async fn homepage(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e).into_response(),
    };

    let cards: Vec<views::CollectionCard> = manifest
        .collections
        .iter()
        .map(|c| {
            let members: Vec<&Wacz> = manifest.members_of(&c.id).collect();
            views::CollectionCard {
                id: c.id.clone(),
                name: c.name.clone(),
                count: members.len(),
                description: c.description.clone(),
                replay_href: collection_replay_href(
                    &c.id,
                    &c.name,
                    collection_default_page(&members),
                ),
                // Capture date range (temporal span is meaningful at the
                // collection level; per-tool software lives on the WACZ page).
                date_range: members_capture_range(&members),
                // Representative image: a curator-set collection thumbnail if
                // present, else the first member crawl that has one.
                thumb: collection_thumb_href(&state.home, &c.id).or_else(|| {
                    members.iter().find_map(|w| {
                        thumb_href(&state.home, &state.index_dir, &w.collection, &w.id)
                    })
                }),
                // Which source kinds the members span — both true = mixed.
                has_local: members.iter().any(|w| !w.source.is_remote()),
                has_remote: members.iter().any(|w| w.source.is_remote()),
            }
        })
        .collect();

    // Browse entry points: years (most recent first) and the busiest sites,
    // each a search link. Derived from an archive-wide facet overview.
    let overview = state
        .search
        .read()
        .unwrap()
        .facet_overview()
        .unwrap_or_default();
    let browse = views::HomeBrowse {
        years: browse_links(&overview, "year", "year", 12, true),
        sites: browse_links(&overview, "site", "site", 8, false),
    };

    let (manage, who) = admin_ctx(&state, &headers);
    let can_login = login_available(&state, &who);
    views::home(&cards, &browse, manage, who.as_deref(), can_login).into_response()
}

/// Build homepage browse links from one facet dimension: `field` is the facet
/// group to read, `query_field` the `field:value` used in the search link.
/// `by_value_desc` sorts by the value (e.g. year, newest first) instead of by
/// count; `max` caps how many are shown.
fn browse_links(
    overview: &[crate::search::FacetGroup],
    field: &str,
    query_field: &str,
    max: usize,
    by_value_desc: bool,
) -> Vec<views::BrowseLink> {
    let Some(group) = overview.iter().find(|g| g.field == field) else {
        return Vec::new();
    };
    let mut buckets: Vec<&crate::search::FacetBucket> = group.buckets.iter().collect();
    if by_value_desc {
        buckets.sort_by(|a, b| b.value.cmp(&a.value));
    }
    buckets
        .into_iter()
        .take(max)
        .map(|b| views::BrowseLink {
            label: b.value.clone(),
            count: b.count,
            href: format!(
                "/search?q={}",
                url_encode(&format!("{query_field}:{}", b.value))
            ),
        })
        .collect()
}

// ── Search results page ───────────────────────────────────────────────────────

/// Search results per page.
const PAGE_SIZE: usize = 20;

/// Format a `YYYYMM` month as `YYYY-MM` for display.
fn format_ym(ym: u64) -> String {
    format!("{:04}-{:02}", ym / 100, ym % 100)
}

/// The active `field:value` facet filters present in a query, in order. Only
/// single-token filters are recognized: a range like `month:[202101 TO 202106]`
/// is a valid query but splits into several whitespace tokens, so it does not
/// appear as a removable chip. Filter fields come from `search::is_filter_field`
/// so this stays in sync with the facet dimensions.
fn active_filters(q: &str) -> Vec<(String, String)> {
    q.split_whitespace()
        .filter_map(|tok| {
            let (f, v) = tok.split_once(':')?;
            (crate::search::is_filter_field(f) && !v.is_empty())
                .then(|| (f.to_string(), v.to_string()))
        })
        .collect()
}

/// Add a `field:value` filter to a query, leaving the rest (including quoted
/// phrases) untouched. A no-op if that exact filter is already present.
fn query_with_filter(q: &str, field: &str, value: &str) -> String {
    let token = format!("{field}:{value}");
    let base = q.trim();
    if base.split_whitespace().any(|t| t == token) {
        return base.to_string();
    }
    if base.is_empty() {
        token
    } else {
        format!("{base} {token}")
    }
}

/// Remove a `field:value` filter from a query (all occurrences of that token).
fn query_without_filter(q: &str, field: &str, value: &str) -> String {
    let token = format!("{field}:{value}");
    q.split_whitespace()
        .filter(|t| *t != token)
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Deserialize)]
struct SearchPageParams {
    q: String,
    /// A `field:value` token to scope the search (e.g. `collection:<id>`),
    /// carried by the header search box when viewing a collection/crawl. ANDed
    /// into the query so it rides the normal filter machinery (removable chip).
    #[serde(default)]
    scope: String,
    /// 1-based page number; absent/`<1` means the first page.
    page: Option<usize>,
}

async fn search_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<SearchPageParams>,
) -> impl IntoResponse {
    // Fold any scope token (e.g. `collection:<id>` from the header search on a
    // collection page) into the query, so downstream faceting + the removable
    // active-filter chip treat it like any other `field:value`.
    let typed = params.q.trim();
    let scope = params.scope.trim();
    let q = if scope.is_empty() || typed.split_whitespace().any(|t| t == scope) {
        typed.to_string()
    } else if typed.is_empty() {
        scope.to_string()
    } else {
        format!("{scope} {typed}")
    };
    if q.is_empty() {
        return (
            StatusCode::SEE_OTHER,
            [("location", "/"), ("content-type", "text/html")],
            String::new(),
        )
            .into_response();
    }

    let page = params.page.unwrap_or(1).max(1);
    let offset = (page - 1) * PAGE_SIZE;
    let response = match state
        .search
        .read()
        .unwrap()
        .search_faceted(&q, PAGE_SIZE, offset)
    {
        Ok(r) => r,
        Err(e) => return error_response(e).into_response(),
    };
    let results = &response.results;

    // Map each WACZ id to the wabac `source` to use: /files/{id} for a local
    // WACZ, or the remote URL directly for an http source.
    let waczs = load_waczs(&state);
    let source_for = |wacz_id: &str| -> String {
        waczs
            .iter()
            .find(|w| w.id == wacz_id)
            .map(viewer_source)
            .unwrap_or_else(|| format!("/files/{wacz_id}"))
    };
    // Curated collection id -> display name, for the "in <collection>" link.
    let collection_names: std::collections::HashMap<String, String> =
        Manifest::open(&state.index_dir)
            .map(|m| {
                m.collections
                    .iter()
                    .map(|c| (c.id.clone(), c.name.clone()))
                    .collect()
            })
            .unwrap_or_default();

    let rows: Vec<views::SearchResultRow> = results
        .iter()
        .map(|r| {
            let is_collection = r.doc_type == "collection";
            let title = if r.title.is_empty() {
                if is_collection {
                    r.crawl_name.clone()
                } else {
                    r.url.clone()
                }
            } else {
                r.title.clone()
            };

            // The curated collection this result belongs to (falls back to the
            // slug/id if the name isn't found).
            let coll_display = collection_names
                .get(&r.collection)
                .map(String::as_str)
                .unwrap_or(&r.collection)
                .to_string();
            let coll_href = url_encode(&r.collection);
            let name_enc = url_encode(&r.crawl_name);
            let source_enc = url_encode(&source_for(&r.crawl_id));
            // Carry the breadcrumb into the replay viewer: the collection (name +
            // id) and the crawl id (so its crumb links to the crawl page).
            let coll_q = format!(
                "&collection={}&collection_id={coll_href}&crawl={}",
                url_encode(&coll_display),
                url_encode(&r.crawl_id)
            );

            let href = if is_collection {
                // Link to the collection's root in the viewer.
                format!("/replay/viewer?source={source_enc}&name={name_enc}{coll_q}")
            } else {
                format!(
                    "/replay/viewer?source={source_enc}&url={}&ts={}&name={name_enc}{coll_q}",
                    url_encode(&r.url),
                    r.timestamp
                )
            };

            // Prefer the hit-highlighted body snippet; if the query didn't match
            // the body (e.g. a title-only or URL-only hit), fall back to the
            // page's description so the result still has context. The snippet is
            // already-safe HTML (Tantivy emits `<b>` tags); the description is
            // plain text, so escape it before splicing as pre-escaped HTML.
            // Fallback chain: the hit-highlighted body snippet; else the page
            // description; else a plain leading excerpt of the stored body prefix
            // (e.g. a title/URL-only hit, or a match deeper than the stored cap);
            // else nothing (the row shows title + URL). Plain text is escaped
            // before splicing as pre-escaped HTML; the snippet is already safe.
            let snippet_html = if !r.snippet.is_empty() {
                Some(r.snippet.clone())
            } else if !r.description.is_empty() {
                Some(html_escape(&r.description))
            } else if !r.body_excerpt.is_empty() {
                Some(html_escape(&r.body_excerpt))
            } else {
                None
            };

            let timestamp_display = if !is_collection && !r.timestamp.is_empty() {
                format_timestamp(&r.timestamp)
            } else {
                String::new()
            };

            views::SearchResultRow {
                href,
                title,
                is_collection,
                url: r.url.clone(),
                timestamp_display,
                snippet_html,
                coll_href,
                coll_display,
                capture_count: r.capture_count,
                status: r.status,
            }
        })
        .collect();

    let total_pages = response.total_hits.div_ceil(PAGE_SIZE).max(1);
    let page_nav = views::PageNav {
        page,
        total_pages,
        total_hits: response.total_hits,
        capped: response.capped,
        query_encoded: url_encode(&q),
    };

    // Facet sidebar: clickable buckets that add/remove a `field:value` filter,
    // plus chips for the filters already active in the query. Refining resets
    // to page 1.
    let filters = active_filters(&q);
    let search_href = |new_q: &str| format!("/search?q={}", url_encode(new_q));
    // The `crawl:` filter's value is an opaque WACZ id (from a crawl-page facet
    // link); show the crawl's name in the chip instead. Other filters show their
    // value as-is. The removal token still uses the raw id.
    let manifest = Manifest::open(&state.index_dir).ok();
    let active: Vec<views::ActiveFilter> = filters
        .iter()
        .map(|(f, v)| {
            let display = if f == "crawl" {
                manifest
                    .as_ref()
                    .and_then(|m| m.wacz_by_id(v))
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| v.clone())
            } else {
                v.clone()
            };
            views::ActiveFilter {
                label: crate::search::filter_label(f).to_string(),
                value: display,
                remove_href: search_href(&query_without_filter(&q, f, v)),
            }
        })
        .collect();
    let groups: Vec<views::FacetGroupView> = response
        .facets
        .iter()
        .map(|g| views::FacetGroupView {
            label: g.label.clone(),
            items: g
                .buckets
                .iter()
                .map(|b| {
                    let is_active = filters.iter().any(|(f, v)| f == &g.field && v == &b.value);
                    let new_q = if is_active {
                        query_without_filter(&q, &g.field, &b.value)
                    } else {
                        query_with_filter(&q, &g.field, &b.value)
                    };
                    views::FacetItem {
                        value: b.value.clone(),
                        count: b.count,
                        href: search_href(&new_q),
                        active: is_active,
                    }
                })
                .collect(),
        })
        .collect();
    let sidebar = views::FacetSidebar { active, groups };

    // Timeline: one clickable bar per crawl month, oldest first, height scaled
    // to the busiest month. Clicking toggles a `month:YYYYMM` filter.
    let max_count = response
        .timeline
        .iter()
        .map(|t| t.count)
        .max()
        .unwrap_or(1)
        .max(1);
    let timeline: Vec<views::TimelineBar> = response
        .timeline
        .iter()
        .map(|t| {
            let month = t.ym.to_string();
            let is_active = filters.iter().any(|(f, v)| f == "month" && v == &month);
            let new_q = if is_active {
                query_without_filter(&q, "month", &month)
            } else {
                query_with_filter(&q, "month", &month)
            };
            views::TimelineBar {
                label: format_ym(t.ym),
                count: t.count,
                pct: (t.count as f64 / max_count as f64 * 100.0).round() as u32,
                href: search_href(&new_q),
                active: is_active,
            }
        })
        .collect();

    let (manage, who) = admin_ctx(&state, &headers);
    let can_login = login_available(&state, &who);
    views::search_results(
        &q,
        &page_nav,
        &sidebar,
        &timeline,
        &rows,
        manage,
        who.as_deref(),
        can_login,
    )
    .into_response()
}

// ── Collection detail page ──────────────────────────────────────────────────

async fn collection_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e).into_response(),
    };
    let Some(c) = manifest.collection_by_id(&id) else {
        return (StatusCode::NOT_FOUND, "collection not found").into_response();
    };
    let members: Vec<&Wacz> = manifest.members_of(&id).collect();

    // Aggregates derived from members.
    let total_size: u64 = members.iter().map(|w| w.file_size).sum();
    let software = collection_software(&members);
    let range = members_capture_range(&members);

    let mut meta = Vec::new();
    if let Some(cur) = &c.curator {
        meta.push(views::MetaRow::new("Curator", cur.clone()));
    }
    meta.push(views::MetaRow::new("Crawls", members.len().to_string()));
    meta.push(views::MetaRow::new("Size", human_size(total_size)));
    if !software.is_empty() {
        meta.push(views::MetaRow::new("Software", software.join(", ")));
    }
    if let Some(r) = &range {
        meta.push(views::MetaRow::new("Capture dates", r.clone()));
    }
    if let Some(q) = capture_quality(&merged_status_counts(&members)) {
        meta.push(views::MetaRow::new("Capture quality", q));
    }
    let created = c.created.get(..10).unwrap_or(&c.created);
    meta.push(views::MetaRow::new("Created", created));

    let member_items: Vec<views::MemberItem> = members
        .iter()
        .map(|w| views::MemberItem {
            id: w.id.clone(),
            name: w.name.clone(),
            present: w.is_present(&state.home),
            remote: w.source.is_remote(),
            provenance: provenance_summary(w),
            thumb: thumb_href(&state.home, &state.index_dir, &w.collection, &w.id),
        })
        .collect();

    // Scoped facet overview: what's *in* this collection, each value a search
    // scoped to it. Turns the page into a faceted entry point, not just a list.
    let overview = state
        .search
        .read()
        .unwrap()
        .facet_overview_scoped(crate::search::FacetScope::Collection(&id))
        .unwrap_or_default();
    let facets = scoped_facet_sections(&overview, &format!("collection:{id}"));

    let (manage, who) = admin_ctx(&state, &headers);
    let can_login = login_available(&state, &who);
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
        replay_href: collection_replay_href(&id, &c.name, collection_default_page(&members)),
        id: id.clone(),
        management: manage,
        signed_in: who,
        can_login,
        annotation_count: annotations::load(&state.home, &id)
            .map(|v| v.len())
            .unwrap_or(0),
    };
    views::collection(&page).into_response()
}

/// GET /collection/{id}/annotations — a public browse of every annotation in the
/// collection, each linking into the collection replay (where it re-anchors).
async fn collection_annotations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e),
    };
    let Some(c) = manifest.collection_by_id(&id) else {
        return (StatusCode::NOT_FOUND, "collection not found").into_response();
    };
    let (manage, who) = admin_ctx(&state, &headers);
    let can_login = login_available(&state, &who);
    let anns = annotations::load(&state.home, &id).unwrap_or_default();
    let items = anns
        .iter()
        .map(|a| {
            let region = a.target.selector.as_ref().map(|s| match s {
                annotations::Selector::TextQuoteSelector { exact, .. } => exact.clone(),
            });
            views::AnnoLink {
                author: a
                    .creator
                    .name
                    .clone()
                    .unwrap_or_else(|| "anonymous".to_string()),
                date: a
                    .modified
                    .clone()
                    .unwrap_or_else(|| a.created.clone())
                    .chars()
                    .take(10)
                    .collect(),
                note_html: crate::markdown::render(&a.body.value),
                page_url: a.target.source.clone(),
                replay_href: collection_replay_href(
                    &id,
                    &c.name,
                    Some((a.target.source.clone(), a.target.timestamp.clone())),
                ),
                region,
            }
        })
        .collect();
    let page = views::AnnotationsIndexPage {
        collection_name: c.name.clone(),
        collection_id: id.clone(),
        items,
        management: manage,
        signed_in: who,
        can_login,
    };
    views::annotations_index(&page).into_response()
}

/// A wabac (ReplayWeb.page) multi-WACZ collection manifest for a collection: the
/// JSON that `<replay-web-page source="…/replay.json">` loads to replay every
/// member crawl as one collection. Each member maps to a resource pointing at the
/// same byte-serving endpoint single-WACZ replay already uses (`viewer_source`):
/// `/files/{id}` for local/Browsertrix sources, the remote URL for a plain URL.
///
/// `name`/`crawlId` are the WACZ id (kept identical so a future server-side pages
/// endpoint can return the same id as `filename` — see the scale-valve phase of
/// rustyweb-homepage-replay-bukh / rustyweb-cross-wacz-replay-dk4). `hash` carries
/// the `sha256:` prefix wabac expects.
async fn collection_replay_json(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let manifest = match Manifest::open(&state.index_dir) {
        Ok(m) => m,
        Err(e) => return error_response(e),
    };
    let Some(c) = manifest.collection_by_id(&id) else {
        return (StatusCode::NOT_FOUND, "collection not found").into_response();
    };
    let resources: Vec<serde_json::Value> = manifest
        .members_of(&id)
        .map(|w| {
            let mut res = serde_json::json!({
                "name": w.id,
                "path": viewer_source(w),
                "crawlId": w.id,
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
        return (StatusCode::NOT_FOUND, "collection has no crawls to replay").into_response();
    }
    let body = serde_json::json!({
        "resources": resources,
        "metadata": {
            "title": c.name,
            "desc": c.description,
            // No `pagesQueryUrl` yet: wabac replays this as a native multi-WACZ
            // collection, loading each member's CDX and resolving URLs itself.
            // Deferring the pagesQueryUrl scale valve (server-side resolution via
            // `collection_pages`) until its lazy-loading + resolution-completeness
            // are browser-verified — the hash fix unblocked it, but proving the
            // flat-footprint win needs more than a render check. See
            // rustyweb-scale-footprint-qw5.10.
        },
    });
    (StatusCode::OK, axum::Json(body)).into_response()
}

#[derive(Deserialize)]
struct PagesParams {
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
async fn collection_pages(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(params): Query<PagesParams>,
) -> Response {
    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(25).clamp(1, 200);
    let offset = (page - 1) * page_size;
    match state.search.read().unwrap().collection_pages(
        &id,
        params.url.as_deref(),
        params.search.as_deref(),
        offset,
        page_size,
    ) {
        Ok((total, hits)) => {
            let items: Vec<serde_json::Value> = hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "url": h.url,
                        "ts": ts_to_iso(&h.timestamp),
                        "title": h.title,
                        "filename": h.crawl_id,
                    })
                })
                .collect();
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({ "total": total, "items": items })),
            )
                .into_response()
        }
        Err(e) => error_response(e),
    }
}

/// Build the scoped facet sections for a detail page. Each dimension becomes a
/// labeled group whose links run a search within `scope` (e.g. `collection:slug`)
/// further filtered by that value. The Collection dimension is skipped (moot on a
/// scoped page), and empty dimensions are dropped.
fn scoped_facet_sections(
    overview: &[crate::search::FacetGroup],
    scope: &str,
) -> Vec<views::FacetSection> {
    // (facet field == filter field, heading, sort by value desc, max shown)
    const DIMS: [(&str, &str, bool, usize); 4] = [
        ("site", "Top sites", false, 10),
        ("year", "By year", true, 12),
        ("type", "Types", false, 6),
        ("lang", "Languages", false, 8),
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
                label: label.to_string(),
                links,
            })
        })
        .collect()
}

async fn crawl_page(
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
    let crumb = col.map(|col| (col.id.clone(), col.name.clone()));
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
        signed_in: who,
        can_login,
    };

    views::crawl(&page).into_response()
}

/// Format a byte count as a short human-readable size.
/// Format a byte count for display (e.g. `48.2 MB`). Shared by the web UI and
/// the CLI so both show sizes the same way.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut b = bytes as f64;
    let mut i = 0;
    while b >= 1024.0 && i < UNITS.len() - 1 {
        b /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{b:.1} {}", UNITS[i])
    }
}

// ── File serving ──────────────────────────────────────────────────────────────

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

async fn serve_file(
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

// ── Search API (JSON) ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    limit: Option<usize>,
}

async fn search_api(
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

// ── ReplayWebPage static assets ───────────────────────────────────────────────

async fn replay_viewer(headers: HeaderMap) -> impl IntoResponse {
    serve_embedded_asset(ReplayAssets::get("viewer.html"), "viewer.html", &headers)
}

async fn replay_index() -> impl IntoResponse {
    (StatusCode::SEE_OTHER, [("location", "/")]).into_response()
}

async fn replay_handler(
    axum::extract::Path(path): axum::extract::Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    serve_embedded_asset(ReplayAssets::get(&path), &path, &headers)
}

/// Serve a site static asset (CSS, etc.) embedded from `static/assets`.
async fn asset_handler(
    axum::extract::Path(path): axum::extract::Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    serve_embedded_asset(SiteAssets::get(&path), &path, &headers)
}

/// The on-disk path to a crawl's thumbnail, preferring a curator's committed
/// pinned image (`collections/<slug>/crawls/<id>.jpg`) over the auto-selected
/// cache (`index/thumbs/<id>.jpg`). `None` if neither exists.
fn thumb_path(home: &Path, index_dir: &Path, collection: &str, crawl_id: &str) -> Option<PathBuf> {
    let pinned = crate::collections::pinned_thumb_path(home, collection, crawl_id);
    if pinned.is_file() {
        return Some(pinned);
    }
    let auto = index_dir.join("thumbs").join(format!("{crawl_id}.jpg"));
    auto.is_file().then_some(auto)
}

/// The `/thumb/{id}` href for a crawl, or `None` if it has no thumbnail (the UI
/// then shows a CSS placeholder). `id` is a crawl id.
fn thumb_href(home: &Path, index_dir: &Path, collection: &str, crawl_id: &str) -> Option<String> {
    thumb_path(home, index_dir, collection, crawl_id).map(|_| format!("/thumb/{crawl_id}"))
}

/// Serve a crawl's representative thumbnail (a committed pinned image under the
/// collection, else the auto cache under `index/thumbs`). 404 when it has none.
async fn thumb_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    // Crawl ids are hex hashes; reject anything else so the id can't escape the
    // thumbs directory.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Resolve the crawl's collection (needed for the committed pinned path).
    let collection = Manifest::open(&state.index_dir)
        .ok()
        .and_then(|m| m.wacz_by_id(&id).map(|w| w.collection.clone()))
        .unwrap_or_default();
    let Some(path) = thumb_path(&state.home, &state.index_dir, &collection, &id) else {
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
fn collection_thumb_href(home: &Path, slug: &str) -> Option<String> {
    crate::collections::collection_thumb_path(home, slug)
        .is_file()
        .then(|| format!("/collection-thumb/{slug}"))
}

/// Serve a collection's curator-set representative thumbnail. 404 when unset.
async fn collection_thumb_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return StatusCode::NOT_FOUND.into_response();
    }
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
        Some(content) => {
            let etag = etag_for(&content.metadata.sha256_hash());

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

            let mime = mime_guess_from_path(path);
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", mime)
                .header("etag", etag)
                .header("cache-control", "no-cache")
                .body(Body::from(content.data.to_vec()))
                .unwrap()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
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

// ── helpers ───────────────────────────────────────────────────────────────────

fn load_waczs(state: &AppState) -> Vec<Wacz> {
    Manifest::open(&state.index_dir)
        .map(|m| m.waczs)
        .unwrap_or_default()
}

/// The `source` value to hand wabac.js for a collection: our local byte-range
/// endpoint for a file, or the remote URL directly (read client-side) for a URL.
fn viewer_source(col: &Wacz) -> String {
    match &col.source {
        // A Browsertrix source is served through /files/{id}, which re-resolves a
        // fresh presigned URL and 302-redirects to it (its stored URL expires).
        crate::collections::Source::File(_)
        | crate::collections::Source::Browsertrix { .. }
        | crate::collections::Source::BrowsertrixPublic { .. } => {
            format!("/files/{}", col.id)
        }
        crate::collections::Source::Url(u) => u.clone(),
    }
}

/// The viewer URL that replays a whole collection (multi-WACZ): wabac loads the
/// collection's `replay.json` manifest (see `collection_replay_json`) as one
/// merged collection. Passes an explicit `coll` namespace plus breadcrumb params.
///
/// A whole-collection entry has no page in mind, so it opens on a sensible
/// default landing page (`default_page`) when one is known — otherwise wabac
/// lands on its collection root. (Specific-context replay — a crawl's Replay
/// button, a search result — carries its own `url`/`ts` and doesn't come through
/// here.)
fn collection_replay_href(id: &str, name: &str, default_page: Option<(String, String)>) -> String {
    let source = format!("/collection/{id}/replay.json");
    let mut href = format!(
        "/replay/viewer?source={}&coll={}&name={}&collection={}&collection_id={}",
        url_encode(&source),
        url_encode(id),
        url_encode(name),
        url_encode(name),
        url_encode(id),
    );
    if let Some((url, ts)) = default_page {
        href.push_str(&format!("&url={}&ts={}", url_encode(&url), url_encode(&ts)));
    }
    href
}

/// A sensible landing page for whole-collection replay: the first member (in
/// manifest order) that has a seed page, with that page's url and wabac
/// timestamp. `None` when no member has a seed page.
fn collection_default_page(members: &[&Wacz]) -> Option<(String, String)> {
    members
        .iter()
        .find_map(|w| w.seed_pages.first())
        .map(|p| (p.url.clone(), ts_to_14digit(&p.ts)))
}

// ── Page annotations API (gnqf.3) ───────────────────────────────────────────
//
// Display is public, authoring is gated — the same pattern as finding aids and
// crawl notes. The public `GET /api/annotations` returns a capture's notes (or
// a whole collection's); create/update/delete live in the management block and
// so inherit its auth gate. Unlike the other write handlers, these also read
// the author identity (via `admin_ctx`) to attribute notes and gate edits.

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
struct AnnotationListQuery {
    collection: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    ts: Option<String>,
}

#[derive(Deserialize)]
struct AnnotationCreateReq {
    collection: String,
    url: String,
    timestamp: String,
    note: String,
    #[serde(default)]
    selector: Option<SelectorDto>,
}

#[derive(Deserialize)]
struct AnnotationUpdateReq {
    collection: String,
    note: String,
}

#[derive(Deserialize)]
struct AnnotationDeleteReq {
    collection: String,
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

/// The author key for the current request: the signed-in identity, or `"local"`
/// on a loopback `--manage` instance (a single trusted admin, no distinct
/// identity). `None` when the request may not annotate.
fn annotation_author(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let (can, who) = admin_ctx(state, headers);
    can.then(|| who.unwrap_or_else(|| "local".to_string()))
}

fn annotation_view(a: &annotations::Annotation, author_key: Option<&str>) -> AnnotationView {
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
    let editable = author_key.is_some() && a.creator.id.as_deref() == author_key;
    AnnotationView {
        id: a.id.clone(),
        created: a.created.clone(),
        modified: a.modified.clone(),
        author: a.creator.name.clone(),
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
async fn list_annotations(
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
                .map(|a| annotation_view(a, author.as_deref()))
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
async fn create_annotation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<AnnotationCreateReq>,
) -> Response {
    if req.note.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "note is empty").into_response();
    }
    let author = annotation_author(&state, &headers).unwrap_or_else(|| "local".to_string());
    let creator = annotations::Creator::person(author.clone(), author.clone());
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
        annotations::create(&st.home, &collection, &ann)
    })
    .await;
    match saved {
        Ok(Ok(())) => (StatusCode::CREATED, Json(view)).into_response(),
        Ok(Err(e)) => error_response(e),
        Err(e) => error_response(anyhow::anyhow!("annotation task panicked: {e}")),
    }
}

/// POST /api/annotations/{id} — update the note text (author only).
async fn update_annotation(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    Json(req): Json<AnnotationUpdateReq>,
) -> Response {
    if req.note.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "note is empty").into_response();
    }
    let author = annotation_author(&state, &headers).unwrap_or_else(|| "local".to_string());
    let st = state.clone();
    let AnnotationUpdateReq { collection, note } = req;
    let author_key = author.clone();
    let done = tokio::task::spawn_blocking(move || {
        let _guard = st.write_lock.lock().expect("write lock poisoned");
        annotations::update(&st.home, &collection, &id, &note, &author_key)
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
async fn delete_annotation(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    Json(req): Json<AnnotationDeleteReq>,
) -> Response {
    let author = annotation_author(&state, &headers).unwrap_or_else(|| "local".to_string());
    let st = state.clone();
    let AnnotationDeleteReq { collection } = req;
    let done = tokio::task::spawn_blocking(move || {
        let _guard = st.write_lock.lock().expect("write lock poisoned");
        annotations::delete(&st.home, &collection, &id, &author)
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

fn error_response(e: anyhow::Error) -> Response {
    tracing::error!("{e:#}");
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

/// `YYYY-MM-DD` from the first 8 digits of a 14-digit timestamp; the input as-is
/// if it is too short.
fn ymd(ts: &str) -> String {
    if ts.len() >= 8 && ts[..8].bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{}", &ts[0..4], &ts[4..6], &ts[6..8])
    } else {
        ts.to_string()
    }
}

/// The capture date range of a collection as a display string (`start → end`, or
/// a single date when they coincide), or `None` when no range was recorded.
fn capture_range(c: &Wacz) -> Option<String> {
    match (c.capture_start.as_deref(), c.capture_end.as_deref()) {
        (Some(s), Some(e)) => {
            let (sd, ed) = (ymd(s), ymd(e));
            Some(if sd == ed {
                sd
            } else {
                format!("{sd} → {ed}")
            })
        }
        (Some(s), None) => Some(ymd(s)),
        (None, Some(e)) => Some(ymd(e)),
        (None, None) => None,
    }
}

/// A compact "capture quality" summary of an HTTP status histogram: total
/// captures, the share that succeeded (2xx/3xx), and the notable failing codes.
/// The derived DACS Appraisal signal — surfaces the 404/403/504 "absences" that
/// a clean-looking crawl can hide.
fn capture_quality(counts: &std::collections::BTreeMap<u16, u64>) -> Option<String> {
    let total: u64 = counts.values().sum();
    if total == 0 {
        return None;
    }
    let ok: u64 = counts
        .iter()
        .filter(|(c, _)| (200..400).contains(*c))
        .map(|(_, n)| n)
        .sum();
    let ok_pct = (ok as f64 / total as f64 * 100.0).round() as u64;
    let mut s = format!("{total} captures, {ok_pct}% ok");
    // Notable failing codes (>= 400), most frequent first.
    let mut bad: Vec<(u16, u64)> = counts
        .iter()
        .filter(|(c, _)| **c >= 400)
        .map(|(c, n)| (*c, *n))
        .collect();
    bad.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if !bad.is_empty() {
        let parts: Vec<String> = bad
            .iter()
            .take(4)
            .map(|(c, n)| format!("{c}×{n}"))
            .collect();
        s.push_str(" — ");
        s.push_str(&parts.join(", "));
    }
    Some(s)
}

/// Map a Browsertrix QA review rating (1–5) to its label, with the raw value —
/// a DACS Appraisal signal ("this crawl was reviewed and judged …").
fn review_label(rating: u8) -> String {
    let word = match rating {
        5 => "Excellent",
        4 => "Good",
        3 => "Fair",
        2 => "Poor",
        1 => "Bad",
        _ => "Reviewed",
    };
    format!("{word} ({rating}/5)")
}

/// Merge the per-crawl status histograms across a collection's members.
fn merged_status_counts(members: &[&Wacz]) -> std::collections::BTreeMap<u16, u64> {
    let mut agg = std::collections::BTreeMap::new();
    for w in members {
        for (code, n) in &w.status_counts {
            *agg.entry(*code).or_insert(0) += *n;
        }
    }
    agg
}

/// The deduped union of software across a collection's member WACZs.
fn collection_software(members: &[&Wacz]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in members {
        for s in &w.software {
            if !out.contains(s) {
                out.push(s.clone());
            }
        }
    }
    out
}

/// The capture date range spanning a collection's member WACZs.
fn members_capture_range(members: &[&Wacz]) -> Option<String> {
    let start = members.iter().filter_map(|w| w.capture_start.clone()).min();
    let end = members.iter().filter_map(|w| w.capture_end.clone()).max();
    match (start, end) {
        (Some(s), Some(e)) => {
            let (sd, ed) = (ymd(&s), ymd(&e));
            Some(if sd == ed {
                sd
            } else {
                format!("{sd} → {ed}")
            })
        }
        (Some(s), None) => Some(ymd(&s)),
        (None, Some(e)) => Some(ymd(&e)),
        (None, None) => None,
    }
}

/// A compact one-line provenance summary (`Software: X · N pages · dates`) as
/// plain text for collection member listings. `None` when nothing is known.
/// The view wraps it in a `.prov` element and handles escaping.
fn provenance_summary(c: &Wacz) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if !c.software.is_empty() {
        parts.push(format!("Software: {}", c.software.join(", ")));
    }
    if let Some(n) = c.page_count {
        parts.push(format!("{n} page{}", if n == 1 { "" } else { "s" }));
    }
    if let Some(range) = capture_range(c) {
        parts.push(range);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Normalize a timestamp to the 14-digit form wabac.js expects. Seed pages in
/// `pages.jsonl` carry ISO 8601 timestamps (`2026-06-09T21:34:06.891Z`); wabac
/// wants `20260609213406`. Extract the digits and take the first 14.
fn ts_to_14digit(ts: &str) -> String {
    ts.chars().filter(|c| c.is_ascii_digit()).take(14).collect()
}

/// Convert a 14-digit capture timestamp (`YYYYMMDDHHMMSS`) to ISO 8601
/// (`YYYY-MM-DDTHH:MM:SSZ`) so wabac's `new Date(ts)` in the pages list parses
/// it (the index stores the 14-digit form). Anything not 14 digits is returned
/// unchanged (already ISO, or empty).
fn ts_to_iso(ts: &str) -> String {
    if ts.len() == 14 && ts.bytes().all(|b| b.is_ascii_digit()) {
        format!(
            "{}-{}-{}T{}:{}:{}Z",
            &ts[0..4],
            &ts[4..6],
            &ts[6..8],
            &ts[8..10],
            &ts[10..12],
            &ts[12..14],
        )
    } else {
        ts.to_string()
    }
}

fn format_timestamp(ts: &str) -> String {
    if ts.len() >= 14 {
        format!(
            "{}-{}-{} {}:{}",
            &ts[0..4],
            &ts[4..6],
            &ts[6..8],
            &ts[8..10],
            &ts[10..12]
        )
    } else {
        ts.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_browsertrix_ids_from_source_and_provenance() {
        use crate::collections::{BrowsertrixRef, Source};
        // Streamed import: the item id lives in the Browsertrix source.
        let streamed = Source::Browsertrix {
            host: "h".into(),
            org: "o".into(),
            item: "streamed-1".into(),
            resource: "a.wacz".into(),
        };
        // Downloaded import: a local file source + a BrowsertrixRef provenance.
        let downloaded = Source::File(std::path::PathBuf::from("archive/x/y.wacz"));
        let downloaded_ref = BrowsertrixRef {
            host: "h".into(),
            item_id: "downloaded-1".into(),
            resource_hash: String::new(),
            review_status: Some(4),
        };
        // A hand-indexed URL crawl contributes nothing.
        let unrelated = Source::Url("https://ex.org/w.wacz".into());

        let ids = imported_browsertrix_ids(
            [
                (&streamed, None),
                (&downloaded, Some(&downloaded_ref)),
                (&unrelated, None),
            ]
            .into_iter(),
        );

        assert!(ids.contains("streamed-1"), "detects streamed source id");
        assert!(ids.contains("downloaded-1"), "detects provenance item id");
        assert_eq!(ids.len(), 2, "the plain URL crawl adds nothing");
    }

    #[test]
    fn hmac_sha256_matches_rfc4231_vector() {
        // RFC 4231, Test Case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn session_cookie_roundtrips_and_rejects_tampering() {
        let secret = "shared-proxy-secret";
        let now = 1_000_000u64;
        let cookie = sign_session(secret, "ed@example.org", now + 100);

        // Valid + unexpired → the identity comes back.
        assert_eq!(
            verify_session(secret, &cookie, now).as_deref(),
            Some("ed@example.org")
        );
        // Expired → rejected.
        assert_eq!(verify_session(secret, &cookie, now + 200), None);
        // Wrong secret (forged by someone without it) → rejected.
        assert_eq!(verify_session("other-secret", &cookie, now), None);
        // Tampered signature → rejected.
        let mut bad = cookie.clone();
        bad.pop();
        bad.push(if cookie.ends_with('A') { 'B' } else { 'A' });
        assert_eq!(verify_session(secret, &bad, now), None);
        // Tampered identity (re-encode a different user, keep the old sig) → rejected.
        let forged = {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
            let sig = cookie.rsplit_once('|').unwrap().1;
            format!("{}|{}|{}", b64.encode("root"), now + 100, sig)
        };
        assert_eq!(verify_session(secret, &forged, now), None);
        // Garbage → None, not a panic.
        assert_eq!(verify_session(secret, "nonsense", now), None);
    }

    #[test]
    fn clear_session_cookie_expires_the_cookie() {
        let mut res = axum::response::Response::new(axum::body::Body::empty());
        clear_session_cookie(&mut res, true);
        let sc = res
            .headers()
            .get(axum::http::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(sc.starts_with("indice_session=;"), "{sc}");
        assert!(sc.contains("Max-Age=0") && sc.contains("HttpOnly") && sc.contains("Secure"));
    }

    #[test]
    fn local_redirect_target_keeps_same_site_paths_only() {
        // A normal same-site Referer → its path (+query) is kept.
        assert_eq!(
            local_redirect_target("http://localhost/collection/x?y=1").as_deref(),
            Some("/collection/x?y=1")
        );
        assert_eq!(
            local_redirect_target("https://archive.example.org/crawl/abc").as_deref(),
            Some("/crawl/abc")
        );
        // Root path.
        assert_eq!(local_redirect_target("http://host/").as_deref(), Some("/"));
        // A Referer from another host still only yields a *path* (never off-site),
        // so this can't be an open redirect.
        assert_eq!(
            local_redirect_target("https://evil.example/collection/x").as_deref(),
            Some("/collection/x")
        );
        // No path component, or something we can't parse → None (caller uses "/").
        assert_eq!(local_redirect_target("http://host"), None);
        assert_eq!(local_redirect_target("not a url"), None);
        // Protocol-relative smuggling is rejected (would navigate off-site),
        // including the backslash variant some browsers normalize to `//`.
        assert_eq!(local_redirect_target("http://host//evil.example"), None);
        assert_eq!(local_redirect_target("http://host/\\evil.example"), None);
        assert_eq!(local_redirect_target("http://host/\\/evil.example"), None);
    }

    #[test]
    fn appbar_offers_login_when_anonymous_under_forward_auth() {
        use maud::html;
        // Forward-auth configured, request anonymous → a "Log in" link, no name.
        let anon = views::layout("t", false, None, true, None, html! {}).into_string();
        assert!(
            anon.contains(r#"href="/manage/login""#) && anon.contains("Log in"),
            "anonymous + can_login should show a login link: {anon}"
        );
        assert!(!anon.contains("signed in as"));

        // Signed in → the identity + a logout link, and no login link.
        let authed = views::layout("t", true, Some("ed"), false, None, html! {}).into_string();
        assert!(authed.contains("signed in as") && authed.contains("ed"));
        assert!(authed.contains(r#"href="/logout""#) && authed.contains("Log out"));
        assert!(!authed.contains("/manage/login"));

        // Plain read-only server (no forward-auth): neither affordance.
        let plain = views::layout("t", false, None, false, None, html! {}).into_string();
        assert!(!plain.contains("/manage/login") && !plain.contains("signed in as"));
    }

    #[test]
    fn capture_quality_summarizes_status_histogram() {
        use std::collections::BTreeMap;
        let mut c = BTreeMap::new();
        c.insert(200u16, 96u64);
        c.insert(301, 2);
        c.insert(404, 1);
        c.insert(504, 1);
        let s = capture_quality(&c).unwrap();
        // 2xx+3xx are "ok": (96+2)/100 = 98%.
        assert!(s.starts_with("100 captures, 98% ok"), "{s}");
        // Failing codes surfaced, most frequent first.
        assert!(s.contains("404×1") && s.contains("504×1"), "{s}");
        // Empty histogram → nothing to show.
        assert!(capture_quality(&BTreeMap::new()).is_none());
    }

    #[test]
    fn active_filters_extracts_facet_tokens_only() {
        // Free text and non-facet `field:` tokens are ignored.
        let f = active_filters("climate type:pdf domain:example.com title:foo");
        assert_eq!(
            f,
            vec![
                ("type".to_string(), "pdf".to_string()),
                ("domain".to_string(), "example.com".to_string()),
            ]
        );
        assert!(active_filters("just some words").is_empty());
    }

    #[test]
    fn query_with_filter_appends_once() {
        assert_eq!(
            query_with_filter("climate", "type", "pdf"),
            "climate type:pdf"
        );
        // Idempotent: already-present filter is not duplicated.
        assert_eq!(
            query_with_filter("climate type:pdf", "type", "pdf"),
            "climate type:pdf"
        );
        // Empty base query yields just the filter.
        assert_eq!(query_with_filter("  ", "year", "2021"), "year:2021");
    }

    #[test]
    fn query_without_filter_removes_that_token() {
        assert_eq!(
            query_without_filter("climate type:pdf", "type", "pdf"),
            "climate"
        );
        // Leaves other filters and free text intact.
        assert_eq!(
            query_without_filter("climate type:pdf domain:ex.com", "type", "pdf"),
            "climate domain:ex.com"
        );
        // Removing an absent filter is a no-op (modulo whitespace normalization).
        assert_eq!(query_without_filter("climate", "type", "pdf"), "climate");
    }

    #[test]
    fn toggling_a_filter_round_trips() {
        let q = "coral reef";
        let added = query_with_filter(q, "collection", "coralreef-gov");
        assert_eq!(added, "coral reef collection:coralreef-gov");
        assert_eq!(
            query_without_filter(&added, "collection", "coralreef-gov"),
            "coral reef"
        );
    }
}
