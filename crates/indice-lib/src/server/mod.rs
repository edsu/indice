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

/// Who is trusted, which is the only question that separates indice's two
/// deployment shapes.
///
/// - [`Access::local`] — a **workstation**. Whoever reaches the port is the
///   operator and may do everything, and the roster is not consulted. Only valid
///   on a loopback bind, which is why the constructor demands the address: there
///   is no way to ask for local trust on a public one.
/// - [`Access::proxy`] — a **server**. indice sits behind something that performs
///   the login (OIDC, SAML, whatever the institution runs) and injects the
///   authenticated user in a header. indice trusts that header **only** when the
///   request also carries the shared secret in `X-Indice-Auth-Secret`, so a
///   forged identity, or a request that never went through the proxy, is
///   refused. indice stores no passwords and speaks to no identity provider.
///
/// A private field rather than public variants, following [`crate::identity::SubjectId`]
/// and [`crate::collections::CollectionId`]: the loopback rule is then a property
/// of the type rather than a check someone can forget to call. Nothing outside
/// this module can conjure `Local` for an address that does not deserve it.
#[derive(Clone)]
pub struct Access(AccessKind);

#[derive(Clone)]
enum AccessKind {
    Local,
    Proxy(ForwardAuth),
}

impl Access {
    /// Local trust, for a workstation. Errors on a non-loopback address: local
    /// mode trusts every caller, so offering it on a public interface would be
    /// an unauthenticated write surface on the network.
    pub fn local(bind: std::net::SocketAddr) -> Result<Self> {
        if !bind.ip().is_loopback() {
            // The fact, not the remedy. AGENTS.md keeps this crate free of
            // user-facing concerns, and an embedder with no CLI should not be
            // told about flags their program does not have; `indice-bin` adds
            // those when it prints this.
            anyhow::bail!(
                "local access trusts every caller, so it is only available on a loopback \
                 address, and this one is {bind}"
            );
        }
        Ok(Access(AccessKind::Local))
    }

    /// Behind a trusted authenticating proxy. Valid on any bind, including
    /// loopback: the usual service deployment puts the proxy on the same host
    /// and has it reach indice over the loopback interface.
    pub fn proxy(user_header: impl Into<String>, secret: impl Into<String>) -> Self {
        Access(AccessKind::Proxy(ForwardAuth {
            user_header: user_header.into(),
            secret: secret.into(),
        }))
    }

    /// The forward-auth settings, or `None` in local mode.
    pub(super) fn forward_auth(&self) -> Option<&ForwardAuth> {
        match &self.0 {
            AccessKind::Local => None,
            AccessKind::Proxy(fa) => Some(fa),
        }
    }

    /// Whether this is the workstation shape. Public because the binary needs
    /// it to decide whether `--site-url` applies.
    pub fn is_local(&self) -> bool {
        matches!(self.0, AccessKind::Local)
    }

    /// A one-line summary for the startup log.
    fn summary(&self) -> &'static str {
        match self.0 {
            AccessKind::Local => "local: every caller on this port is the operator",
            AccessKind::Proxy(_) => "forward-auth: identity comes from the trusted proxy",
        }
    }
}

impl std::fmt::Debug for Access {
    /// Hand-written rather than derived, because [`ForwardAuth`] holds the
    /// shared secret and this type reaches error messages and test output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            AccessKind::Local => f.write_str("Access::Local"),
            AccessKind::Proxy(fa) => {
                write!(f, "Access::Proxy({}, <secret>)", fa.user_header)
            }
        }
    }
}

/// Server configuration beyond the home directory and the bind address.
#[derive(Clone)]
pub struct ServerConfig {
    /// Who is trusted. See [`Access`].
    pub access: Access,
    /// Where `/logout` sends the browser after clearing indice's display cookie.
    /// `None` → `/`. Behind a login service, set this to its sign-out URL (e.g.
    /// `/oauth2/sign_out?rd=/`) so one click ends both sessions.
    pub logout_redirect: Option<String>,
    /// This site's public authority (`host[:port]`), for the cross-site (CSRF)
    /// check on writes. `None` — the normal case — means "infer it from
    /// `X-Forwarded-Host`/`Host`", which is correct for the shipped `Caddyfile`
    /// and for a direct loopback bind. Set it (`--site-url`) only behind a proxy
    /// that rewrites `Host` *without* setting `X-Forwarded-Host`, which is
    /// nginx's and Apache's default.
    pub site_authority: Option<String>,
}

impl ServerConfig {
    /// The given access, with everything else defaulted.
    pub fn new(access: Access) -> Self {
        Self {
            access,
            logout_redirect: None,
            site_authority: None,
        }
    }
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
    /// in-flight add at a time is plenty for a workstation, and for the handful
    /// of curators a server has.
    write_lock: std::sync::Mutex<()>,
    /// Progress channels for in-flight add-archive jobs, drained once by the SSE
    /// endpoint. Keyed by an incrementing job id ([`AppState::job_counter`]).
    jobs: std::sync::Mutex<HashMap<u64, mpsc::UnboundedReceiver<ProgressEvent>>>,
    job_counter: AtomicU64,
    /// Who may do what, from `<home>/users.yaml`, re-read when the file
    /// changes so approving a colleague does not mean restarting the service.
    /// See [`crate::identity::Roster`] for why a reload can only ever replace a
    /// good roster with another good one.
    users: crate::identity::Roster,
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

/// Build a router without binding a socket, for tests and for anything
/// embedding indice's HTTP surface.
///
/// Takes the same [`ServerConfig`] as [`serve_with_resolver`], so a caller has
/// to say who is trusted. Note that [`Access::local`]'s loopback check is
/// re-run against the real socket in [`serve_on_listener`], which this path
/// skips: there is no socket to check. That is fine for a test harness and
/// worth knowing before serving the result over a public listener.
pub fn router(home: &Path, config: ServerConfig) -> Result<Router> {
    build_router(home, None, config, Providers::default())
}

/// Like [`router`], but with a [`crate::index::SourceResolver`] so the server
/// can replay Browsertrix sources (re-resolving fresh presigned URLs on demand).
pub fn router_with_resolver(
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    config: ServerConfig,
) -> Result<Router> {
    build_router(home, resolver, config, Providers::default())
}

/// Build the app router. Every route is mounted, including the write surface:
/// who may use it is an authorization question, answered by the typed
/// `Curator`/`Admin` extractors and `users.yaml`, not by which routes exist.
///
/// It used to be both. `--manage` decided whether the write routes were mounted
/// at all, crossed with local-or-forward-auth deciding who was trusted, which
/// made four states for three meanings and left "read-only server" as a shape
/// nobody wanted: a server you cannot write to sends you back to the command
/// line the first time you need to fix a finding aid. Collapsing it leaves one
/// question, which [`Access`] answers.
fn build_router(
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    config: ServerConfig,
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
    // Authentication comes from the CLI and the proxy ([`Access`]); authorization
    // comes from the home directory. Loading it here means a malformed
    // permissions file stops startup rather than silently granting whatever the
    // default is.
    let users = crate::identity::Roster::load(home)?;
    let access_is_local = config.access.is_local();
    tracing::info!("access: {}", config.access.summary());
    // A server with no roster hands admin to the first stranger its identity
    // provider admits, and `remove_dir_all` with it. That default is correct on
    // a workstation and indefensible on a network, and nothing distinguished
    // the two until now: the only signal was one `tracing::info` line that
    // scrolls past at startup.
    //
    // Refusing matches the precedent next door, where a non-loopback bind with
    // no auth proxy also refuses, and it fails in the direction you can
    // recover from. There is deliberately no flag to opt out. "Everyone my IdP
    // admits is an admin" is not a configuration worth keeping reachable on a
    // server, and the people who mean it can write the handful of lines below;
    // an opt-out would mostly be found by someone wanting the error to go away.
    if !access_is_local && !users.current().is_configured() {
        let path = crate::identity::Users::path(home);
        anyhow::bail!(
            "refusing to start: this indice is reachable over the network and has no \
             roster at {}, so everyone your identity provider admits would be an admin \
             and could delete the archive.\n\n\
             Create it, listing the people who may change things:\n\n\
             users:\n  \
               - id: you@example.org\n    \
                 role: admin\n\n\
             Anyone not listed can still read. An empty `users: []` means nobody may \
             write, which is how you run a public read-only archive. \
             See https://indice.page/docs/guides/manage/",
            path.display()
        );
    }
    // Only worth saying when it is consulted. Local access never looks at the
    // roster, and printing "every authenticated user is an admin" to someone
    // running on their laptop invites them to go fix a file that changes
    // nothing.
    if !access_is_local {
        tracing::info!("roles: {}", users.current().summary());
    } else if crate::identity::Users::path(home).exists() {
        // Local access never consults the roster, yet a malformed one still
        // aborts startup above, so someone can write a users.yaml, demote
        // themselves in it, have a typo stop the server, fix the typo, and
        // still be an admin. Say so rather than letting them find out.
        tracing::warn!(
            "users.yaml is present but not consulted: this indice trusts every caller \
             on its loopback port, so roles apply only behind an authenticating proxy"
        );
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
        users,
        forward_auth: config.access.forward_auth().cloned(),
        logout_redirect: config.logout_redirect.clone(),
        browsertrix: providers.browsertrix,
        archiveit: providers.archiveit,
    });

    // `--site-url` exists to fix the Origin comparison behind a proxy that
    // rewrites `Host`, which local mode cannot be behind. Honouring it there
    // pins the expected authority to a public name, so every workroom write
    // 403s with a message advising the very setting that caused it. Dropped
    // rather than respected, so one env file shared between a server and a
    // laptop stays harmless.
    let site_authority = if access_is_local {
        None
    } else {
        config.site_authority.clone()
    };

    let mut app = Router::new()
        .route("/", get(homepage))
        .route("/health", get(health))
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

    // The write surface: the browser management UI plus its endpoints — add a
    // crawl (`index_location`, streaming progress over SSE), upload a WACZ, and
    // create or edit a collection finding aid (`set_collection`).
    //
    // Always mounted. Who may use it is decided by the `Curator`/`Admin`
    // extractors, which are unforgeable witness types, so an anonymous caller
    // gets 403 rather than 404. A 404 for an authorization failure is obscurity,
    // and it used to mean the same request answered differently depending on a
    // flag rather than on who was asking.
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
    if let Some(fa) = config.access.forward_auth().cloned() {
        let guard = Arc::new(fa);
        manage_routes = manage_routes.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let guard = guard.clone();
                async move { forward_auth(&guard, req, next).await }
            },
        ));
    }

    app = app.merge(manage_routes);

    // Outside `manage_routes` on purpose: logout must NOT be
    // forward-auth-gated, because that middleware re-sets the display
    // cookie on its way out and would sign you straight back in. The
    // server-wide CSRF guard still covers it, which is what makes being a
    // POST sufficient — see `auth::logout`.
    app = app.route("/logout", post(logout));

    // CSRF: refuse any state-changing request that some *other* site's page
    // initiated. Applied once, over the WHOLE server, rather than onto the
    // router that happens to need it.
    //
    // That distinction is the lesson of the bug this replaced. `/logout` was
    // vulnerable precisely because it sat outside the guarded block, and
    // guarding it by hand would have left the next state-changing route mounted
    // elsewhere in exactly the same position, with nothing to catch it. Here
    // the default is protected and a route has to be *safe* to opt out, which
    // it does by being a GET.
    //
    // Cheap to apply broadly: for safe methods (GET/HEAD/OPTIONS/TRACE) the
    // cross-site half returns immediately, so a read request pays one `match`
    // and, on a server, nothing else.
    //
    // In local mode `Host` is whatever the browser sends, so matching it
    // against `Origin` can be satisfied by DNS rebinding; local mode is
    // loopback-only anyway, so also require a loopback authority there, on
    // every request rather than only the state-changing ones. Behind
    // a proxy (or with an explicit --site-url) the authority comes from a
    // trusted source and needs no such check.
    let csrf = Arc::new(CsrfPolicy {
        // Not `&& site_authority.is_none()`. `--site-url` exists to fix Origin
        // comparison behind a proxy that rewrites `Host`, a situation local mode
        // cannot be in, and letting it clear this turned a stray INDICE_SITE_URL
        // in a shared .env into a silent kill switch for the whole loopback
        // requirement. The two settings are now independent: the loopback check
        // reads `Host` directly and ignores `site_authority` entirely.
        require_loopback: access_is_local,
        site_authority,
    });

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
        // Outermost: `Router::layer` wraps, so this runs FIRST. A cross-site
        // POST is refused before anything else happens to it — before
        // forward-auth does any identity bookkeeping (notably before it
        // refreshes the display cookie), and before a body is read, which
        // matters given the disabled body limit on `/api/archives/upload`.
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let csrf = csrf.clone();
                async move { same_origin_guard(&csrf, req, next).await }
            },
        ))
        .with_state(state);

    Ok(app)
}

/// Bind and serve. `config` says who is trusted (see [`Access`]); `providers`
/// supplies authenticated import clients (Browsertrix, Archive-It) for the UI.
///
/// There used to be a `serve(bind, home)` above this that hard-coded "management
/// off". It had no callers, and the mode it selected no longer exists.
pub async fn serve_with_resolver(
    bind: &str,
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    config: ServerConfig,
    providers: Providers,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("listening on {bind}");
    serve_on_listener(listener, home, resolver, config, providers).await
}

/// Serve on an already-bound listener. This lets a caller bind `127.0.0.1:0`,
/// read back the OS-assigned port via [`TcpListener::local_addr`], and only then
/// serve, which is what you want when something has to know the port before the
/// server starts accepting. The test suite uses it for exactly that; it was
/// originally added for a desktop shell that no longer exists.
///
/// [`TcpListener::local_addr`]: tokio::net::TcpListener::local_addr
pub async fn serve_on_listener(
    listener: tokio::net::TcpListener,
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    config: ServerConfig,
    providers: Providers,
) -> Result<()> {
    // [`Access::local`] already refuses a non-loopback address, but it is given
    // the address the *caller* intends to bind rather than the one the listener
    // actually got. Re-check against the real socket, which is the only thing
    // that knows: a caller passing 127.0.0.1 and handing over a listener bound
    // to 0.0.0.0 would otherwise slip through.
    if config.access.is_local() {
        let addr = listener.local_addr()?;
        if !addr.ip().is_loopback() {
            anyhow::bail!(
                "refusing to start: local access trusts every caller, so it must be \
                 bound to a loopback address (127.0.0.1 / ::1), but this listener is \
                 bound to {addr}. Configure an authenticating proxy to run as a server."
            );
        }
    }
    let app = build_router(home, resolver, config, providers)?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
