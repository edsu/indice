use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::mpsc;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use crate::search::SearchIndex;

mod api;
mod assets;
mod auth;
mod collection;
mod crawl;
mod imports;
mod manage;
mod pages;
mod util;

// Re-imported here so every submodule can reach its siblings' handlers and
// helpers through a single `use super::*;`.
use api::*;
use assets::*;
use auth::*;
use collection::*;
use crawl::*;
use imports::*;
use manage::*;
use pages::*;
use util::*;

#[cfg(test)]
mod tests;

pub use util::human_size;

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
    /// This site's public authority (`host[:port]`), for the cross-site (CSRF)
    /// check on management writes. `None` — the normal case — means "infer it from
    /// `X-Forwarded-Host`/`Host`", which is correct for both shipped Caddyfiles and
    /// for a direct loopback bind. Set it (`--site-url`) only behind a proxy that
    /// rewrites `Host` *without* setting `X-Forwarded-Host`, e.g. nginx's default
    /// `proxy_set_header Host $proxy_host`.
    pub site_authority: Option<String>,
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
            ..Self::default()
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
            ..Self::default()
        }
    }
}

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
        let forward_auth_off = manage.forward_auth.is_none();
        if let Some(fa) = manage.forward_auth {
            let guard = Arc::new(fa);
            manage_routes = manage_routes.layer(axum::middleware::from_fn(
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    let guard = guard.clone();
                    async move { forward_auth(&guard, req, next).await }
                },
            ));
        }

        // CSRF: refuse a state-changing management request that some *other*
        // site's page initiated. Applied unconditionally — not inside the
        // `if let` above — because local (loopback) mode has no forward-auth
        // layer and is therefore the mode with no other protection at all.
        //
        // `Router::layer` wraps, so this last layer is the OUTERMOST one and runs
        // first: a cross-site POST is refused before forward-auth does any
        // identity bookkeeping (notably before it refreshes the display cookie),
        // and before any body is read — which matters given the disabled body
        // limit on `/api/archives/upload`.
        // In local mode `Host` is whatever the browser sends, so matching it
        // against `Origin` can be satisfied by DNS rebinding; local mode is
        // loopback-only anyway, so also require a loopback authority there.
        // Behind a proxy (or with an explicit --site-url) the authority comes
        // from a trusted source and needs no such check.
        let policy = Arc::new(CsrfPolicy {
            require_loopback: forward_auth_off && manage.site_authority.is_none(),
            site_authority: manage.site_authority.clone(),
        });
        manage_routes = manage_routes.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let policy = policy.clone();
                async move { same_origin_guard(&policy, req, next).await }
            },
        ));
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
