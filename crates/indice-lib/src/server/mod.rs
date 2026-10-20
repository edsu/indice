use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::mpsc;
use tower_http::compression::CompressionLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::search::SearchIndex;

/// How long a request that should be quick is allowed to take.
///
/// Every route under this bound answers from memory, the index, or an embedded
/// asset. Thirty seconds is far longer than any of them has a right to need, so
/// hitting it means something has gone wrong rather than that a visitor asked
/// for too much, and the 503 is the honest answer to "is this server healthy".
/// Picking a tighter number would start refusing legitimate work on a cold
/// index or a loaded machine, for no gain: this is a backstop, not a budget.
///
/// It only became a real bound once index reads moved to the blocking pool —
/// see [`AppState::read_index`]. A timeout is a future racing a timer, and
/// neither is polled while the runtime's workers sit inside Tantivy, so before
/// that change this layer would have been most inert exactly when it mattered.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Headroom above the per-upload cap for everything in a multipart body that is
/// not the file: boundaries, part headers, and the collection and display-name
/// fields, which are themselves capped at 4 KiB each.
pub(super) const UPLOAD_FRAMING_ALLOWANCE: u64 = 64 * 1024;

mod api;
mod assets;
mod auth;
mod collection;
mod crawl;
mod headers;
mod imports;
mod limits;
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
    /// it to decide whether `--site-url` is required or ignored.
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
    /// This site's public URL. A server must be given one (the router refuses
    /// to build without it); a workstation ignores it. Set with
    /// [`ServerConfig::set_site_url`].
    site: Option<SiteUrl>,
    /// Let a networked instance fetch a curator-supplied URL that resolves to
    /// this machine or this network.
    ///
    /// Off by default, because behind an auth proxy the curator naming the
    /// address may be an approved stranger rather than the operator. See
    /// [`crate::http_range::FetchPolicy`].
    ///
    /// There is an opt-out here and deliberately none for the roster, which is
    /// worth explaining because the two look alike. A missing roster has no
    /// legitimate use on a server; it is only ever a mistake, so a flag to
    /// permit it would be a flag to stay wrong. An archive host on the internal
    /// network is ordinary institutional infrastructure, and an operator who
    /// turns this on is enabling something they actually have, having been told
    /// what it costs.
    pub allow_internal_fetch: bool,
    /// Where replay runs. Archived pages run their own JavaScript, so they are
    /// replayed on an origin of their own, where that script can reach nothing
    /// of indice's: not the write API, not the reader's session, not the
    /// viewer's page. See [`ReplayOrigin`].
    ///
    /// A server must be given one ([`serve_on_listeners`] refuses to start
    /// without it). A workstation's is the address its replay listener bound,
    /// and this setting is ignored there.
    pub replay_origin: Option<ReplayOrigin>,
    /// The app listener's address, on a workstation, so the replay origin can
    /// say who may frame it. Set by [`serve_on_listeners`], which is the only
    /// thing that knows it.
    app_addr: Option<std::net::SocketAddr>,
}

/// A server's public URL, as an origin (`scheme://host[:port]`) spelled exactly
/// as a browser's `Origin` is: what a write's `Origin` must equal, and who may
/// frame replay. The scheme is kept so a page on `http://` cannot write to a
/// site on `https://`, and so `frame-ancestors` matches a site on plain http
/// while replay is on https.
#[derive(Clone, Debug)]
struct SiteUrl(String);

impl ServerConfig {
    /// Set this site's public URL (`--site-url`): the origin the CSRF check
    /// compares with, and the origin replay may be framed by. Refuses anything
    /// but a bare http(s) origin, by the same rules as [`ReplayOrigin::parse`].
    pub fn set_site_url(&mut self, raw: &str) -> Result<()> {
        self.site = Some(SiteUrl(parse_origin(raw, "https://archive.example.org")?));
        Ok(())
    }

    /// The site's public origin, if one has been set.
    pub fn site_url(&self) -> Option<&str> {
        self.site.as_ref().map(|s| s.0.as_str())
    }

    /// Refuse replay on the site's own host. A different port is another
    /// origin, but cookies ignore ports, so replay there would share the
    /// site's cookies.
    ///
    /// `indice serve` calls this at startup. The router does not, because the
    /// browser tests need exactly that arrangement: two loopback ports are how
    /// a test gets the same-site pair that a subdomain gives a real server, and
    /// Chrome shares the replay host's service worker between the replay frame
    /// and the replay host opened directly only when they are same-site.
    pub fn check_replay_host(&self) -> Result<()> {
        if let (Some(site), Some(replay)) = (&self.site, &self.replay_origin) {
            let host = origin_host(&site.0);
            if host.eq_ignore_ascii_case(replay.host()) {
                anyhow::bail!(
                    "replay is on the site's own host ({}). A different port is not \
                     enough, because cookies are shared across ports, so give replay a \
                     hostname of its own.",
                    host
                );
            }
        }
        Ok(())
    }

    /// The given access, with everything else defaulted.
    pub fn new(access: Access) -> Self {
        Self {
            access,
            logout_redirect: None,
            site: None,
            allow_internal_fetch: false,
            replay_origin: None,
            app_addr: None,
        }
    }
}

/// The origin replay is served from, as `scheme://host[:port]`.
///
/// Replay runs archived third-party JavaScript. On indice's own origin that
/// script is same-origin with the write API, so a page someone got crawled
/// could act as whoever replays it, and no CSRF check can tell, because the
/// request really is same-origin. On an origin of its own the browser keeps
/// it out: its requests to indice carry a foreign `Origin` and are refused,
/// and it cannot reach into the viewer that frames it.
///
/// Constructed only by [`ReplayOrigin::parse`] or from a bound listener, so
/// one that reaches the viewer is always a bare origin: the viewer compares it
/// against `MessageEvent.origin` byte for byte, and a trailing slash or a path
/// would silently drop every message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayOrigin(String);

/// Parse `raw` as a bare http(s) origin, `scheme://host[:port]`, spelled as a
/// browser spells `Origin`. Refuses a path, query, fragment or credentials
/// rather than dropping them: `https://http://archive.example.org` is a valid
/// URL whose origin is `https://http`, and keeping only that would start a
/// server that refuses every write. `example` goes in the error message.
fn parse_origin(raw: &str, example: &str) -> Result<String> {
    let url = url::Url::parse(raw.trim())
        .map_err(|e| anyhow::anyhow!("{raw:?} is not an absolute URL ({e})"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("{raw:?} is not an http or https URL");
    }
    if url.host_str().is_none() {
        anyhow::bail!("{raw:?} has no host");
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        anyhow::bail!(
            "{raw:?} is more than an origin; give only the scheme and host, e.g. {example}"
        );
    }
    // `ascii_serialization` elides the scheme's default port, as a browser's
    // `Origin` does, so the two compare directly.
    Ok(url.origin().ascii_serialization())
}

/// The host of an origin from [`parse_origin`], without scheme, port or the
/// brackets around an IPv6 address.
fn origin_host(origin: &str) -> &str {
    let rest = origin.split_once("://").map_or(origin, |(_, r)| r);
    if let Some(v6) = rest.strip_prefix('[') {
        return v6.split(']').next().unwrap_or(v6);
    }
    rest.rsplit_once(':').map_or(rest, |(h, _)| h)
}

impl ReplayOrigin {
    /// Parse a replay URL such as `https://replay.example.org`. Refuses
    /// anything but a bare http(s) origin; see [`parse_origin`].
    pub fn parse(raw: &str) -> Result<Self> {
        parse_origin(raw, "https://replay.example.org").map(Self)
    }

    /// The origin of a listener bound on this machine, for a workstation.
    pub fn for_listener(addr: std::net::SocketAddr) -> Self {
        // `SocketAddr`'s Display brackets an IPv6 address, as a URL needs.
        Self(format!("http://{addr}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The host, without scheme or port.
    pub fn host(&self) -> &str {
        origin_host(&self.0)
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
    /// Whether a networked instance may fetch from its own network. See
    /// [`ServerConfig::allow_internal_fetch`].
    allow_internal_fetch: bool,
    /// How much the browser write surface may add to the archive, from
    /// `config.yaml`. Read once at startup; a change takes a restart.
    upload_limits: UploadLimits,
    /// How much one client, and everyone together, may ask at once. `None` on
    /// a workstation, which has one user and nothing to protect them from.
    limits: Option<limits::RequestLimits>,
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
    /// Where the viewer frames replay. `None` only for a router built without
    /// a replay listener, whose viewer then says replay is not configured.
    replay_origin: Option<ReplayOrigin>,
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
    /// Run a read against the search index on a blocking thread.
    ///
    /// Tantivy is synchronous and CPU-bound, and every handler here used to
    /// call it inline. On tokio's multi-threaded runtime that occupies a worker
    /// for the whole query, and there is one worker per core by default, so a
    /// handful of concurrent expensive queries starves the executor. Everything
    /// else stops with it: `/health` stops answering, so an orchestrator starts
    /// failing the container over for reasons it cannot see.
    ///
    /// It also defeats the obvious mitigation. A request timeout only produces
    /// a response when its future is polled, and a starved runtime polls
    /// nothing, so the layer meant to cut these requests off is inert exactly
    /// when it is needed. Moving the work to the blocking pool is what makes a
    /// timeout mean anything, which is why it comes first.
    ///
    /// The `Arc` is cloned rather than the read guard held across the call.
    /// Holding it would make [`AppState::reload_searcher`]'s write lock wait on
    /// every in-flight query, so a busy server could stall an ingest's swap.
    ///
    /// A panic inside `f` is re-raised on the handler task, which is what
    /// calling Tantivy inline did too: this changes where the work runs, not
    /// what a bug in it does.
    pub(super) async fn read_index<T, F>(&self, f: F) -> T
    where
        F: FnOnce(&SearchIndex) -> T + Send + 'static,
        T: Send + 'static,
    {
        let index = Arc::clone(&self.search.read().unwrap());
        match tokio::task::spawn_blocking(move || f(&index)).await {
            Ok(value) => value,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => std::panic::panic_any(e.to_string()),
        }
    }

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
/// re-run against the real socket in [`serve_on_listeners`], which this path
/// skips: there is no socket to check. That is fine for a test harness and
/// worth knowing before serving the result over a public listener.
pub fn router(home: &Path, config: ServerConfig) -> Result<Router> {
    Ok(routers(home, config)?.app)
}

/// Both of indice's routers over one shared state: the app, and replay.
///
/// They are served on separate listeners, and so on separate origins; see
/// [`ReplayOrigin`] for why. Shared state means one client has one rate-limit
/// budget across the two.
pub struct Routers {
    /// Everything but replay: pages, search, the viewer, the write API.
    pub app: Router,
    /// Replay alone: the frame, ReplayWeb.page, and the archive bytes and
    /// manifests it reads. No write route, no page, no identity.
    pub replay: Router,
}

/// Build both routers without binding a socket. A server still needs its site
/// URL. Like [`router`], this skips the checks
/// [`serve_on_listeners`] makes against real sockets, including that a server
/// has a replay origin.
pub fn routers(home: &Path, config: ServerConfig) -> Result<Routers> {
    build_routers(home, None, config, Providers::default())
}

/// Like [`router`], but with a [`crate::index::SourceResolver`] so the server
/// can replay Browsertrix sources (re-resolving fresh presigned URLs on demand).
pub fn router_with_resolver(
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    config: ServerConfig,
) -> Result<Router> {
    Ok(build_routers(home, resolver, config, Providers::default())?.app)
}

/// Build the app and replay routers over one state. Every app route is
/// mounted, including the write surface:
/// who may use it is an authorization question, answered by the typed
/// `Curator`/`Admin` extractors and `users.yaml`, not by which routes exist.
///
/// It used to be both. `--manage` decided whether the write routes were mounted
/// at all, crossed with local-or-forward-auth deciding who was trusted, which
/// made four states for three meanings and left "read-only server" as a shape
/// nobody wanted: a server you cannot write to sends you back to the command
/// line the first time you need to fix a finding aid. Collapsing it leaves one
/// question, which [`Access`] answers.
fn build_routers(
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    config: ServerConfig,
    providers: Providers,
) -> Result<Routers> {
    // Configuration first, so a missing setting is reported before any I/O.
    //
    // What a write's `Origin` must match. A server compares with `--site-url`
    // and will not start without it: working the address out from `Host` or
    // `X-Forwarded-Host` is right behind one proxy and wrong behind another, and
    // the check that replay is on another host has nothing to compare without it.
    // A workstation compares with the request's own loopback `Host` and ignores
    // `--site-url`, so one env file shared between a server and a laptop stays
    // harmless.
    let csrf = if config.access.is_local() {
        CsrfPolicy::Loopback
    } else {
        match &config.site {
            Some(site) => CsrfPolicy::Site(site.0.clone()),
            // The fact, not the flag, as for the replay origin: indice-bin
            // names `--site-url` when it prints this.
            None => anyhow::bail!(
                "refusing to start: this indice is reachable over the network and does \
                 not know its own public URL, so it cannot tell its own pages' writes \
                 from another site's, or check that replay is on a different host."
            ),
        }
    };

    if let CsrfPolicy::Site(origin) = &csrf {
        tracing::info!("site: {origin} (writes are accepted only from pages here)");
    }
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
    // Loaded here so a malformed config.yaml stops the server at startup, as
    // the roster does, rather than at the first upload or ingest.
    let home_config = crate::config::Config::load(home)?;
    crate::config::Config::check_upload_limits(home)?;
    let upload_limits = UploadLimits {
        per_upload: home_config.max_upload_bytes(),
        archive: home_config.max_archive_bytes(),
    };
    // Said at startup beside `access:` and `roles:`, so an operator who meant to
    // set a limit can see whether it took. Unset is stated, not left out: on a
    // server open to curators it is a decision with consequences.
    let show = |b: Option<u64>| b.map_or("no limit".to_string(), human_size);
    tracing::info!(
        "uploads: {} per upload, {} for the archive",
        show(upload_limits.per_upload),
        show(upload_limits.archive)
    );
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
        allow_internal_fetch: config.allow_internal_fetch,
        upload_limits,
        limits: (!access_is_local).then(limits::RequestLimits::new),
        logout_redirect: config.logout_redirect.clone(),
        browsertrix: providers.browsertrix,
        archiveit: providers.archiveit,
        replay_origin: config.replay_origin.clone(),
    });

    // Everything here answers from memory, the index, or an embedded asset, so
    // it has a time bound. `/files/{id}` does not and is merged in below.
    let mut app = Router::new()
        .route("/", get(homepage))
        .route("/search", get(search_page))
        .route("/collection/{id}", get(collection_page))
        .route("/collection/{id}/replay.json", get(collection_replay_json))
        .route("/collection/{id}/pages", get(collection_pages))
        .route("/collection/{id}/annotations", get(collection_annotations))
        .route("/crawl/{id}", get(crawl_page))
        .route("/thumb/{id}", get(thumb_handler))
        .route("/collection-thumb/{id}", get(collection_thumb_handler))
        .route("/replay/viewer", get(replay_viewer))
        .route("/api/search", get(search_api))
        // Public read of page annotations (display is public; writes are gated below).
        .route("/api/annotations", get(list_annotations))
        .route("/assets/{*path}", get(asset_handler))
        // The viewer's own script and style, and nothing else under /replay/.
        // ReplayWeb.page, its service worker, its app shell and the frame are
        // served only by the replay router: anything on this origin that can
        // boot ReplayWeb.page can be framed by a stranger's page with a
        // `source` of their choosing, which puts archived script on indice's
        // origin again.
        .route("/replay/{*path}", get(viewer_asset))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            limits::limit_quick,
        ));

    // Streaming a WACZ has no sensible time bound: it is whole gigabytes, and a
    // reader on a slow link is doing nothing wrong. Replay also issues many
    // ranged reads per page, so this is the busiest route indice serves.
    //
    // Which leaves it the one anonymous route a timeout does not cover. That is
    // a concurrency problem rather than a duration one — many open streams, not
    // one slow request — and the answer to it is a connection limit, which is
    // the next piece of this ticket rather than something to fake with a clock.
    let streaming = Router::new().route("/files/{id}", get(serve_file)).layer(
        axum::middleware::from_fn_with_state(state.clone(), limits::limit_streams),
    );

    // Outside every limit, on purpose: an orchestrator reads a busy server's
    // 503 on `/health` as a dead one and restarts it, which turns load into an
    // outage. It answers as long as the process does.
    let health_route = Router::new().route("/health", get(health));

    // The write surface: the browser management UI plus its endpoints — add a
    // crawl (`index_location`, streaming progress over SSE), upload a WACZ, and
    // create or edit a collection finding aid (`set_collection`).
    //
    // Always mounted. Who may use it is decided by the `Curator`/`Admin`
    // extractors, which are unforgeable witness types, so an anonymous caller
    // gets 403 rather than 404. A 404 for an authorization failure is obscurity,
    // and it used to mean the same request answered differently depending on a
    // flag rather than on who was asking.
    let manage_routes = Router::new()
        .route("/manage/collections/new", get(new_collection_form))
        .route("/manage/edit/{id}", get(edit_collection_form))
        .route("/manage/add", get(accession_desk_page))
        // A login entry point: being gated, visiting it forces the proxy's
        // login, then bounces back to where the user came from.
        .route("/manage/login", get(manage_login))
        .route("/api/archives", post(add_archive))
        .route("/api/collections", post(create_collection))
        // Delete a crawl or a collection (removes files + updates the index).
        .route("/api/crawls/{id}/delete", post(delete_crawl_handler))
        .route(
            "/api/collections/{id}/delete",
            post(delete_collection_handler),
        )
        // Browsertrix import: browse (orgs → collections → items) using the
        // binary-supplied credentials, then import selected items as a job.
        .route("/api/browsertrix/import", post(bx_import))
        // Archive-It import: the same shape.
        .route("/api/archiveit/import", post(ait_import))
        // Page annotations: create/edit/delete, gated like the rest. The
        // public GET /api/annotations lives in the read block above.
        .route("/api/annotations", post(create_annotation))
        .route("/api/annotations/{id}", post(update_annotation))
        .route("/api/annotations/{id}/delete", post(delete_annotation))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ));

    // Management routes with no time bound, for three different reasons.
    //
    // The upload streams a whole WACZ in, so it is as long as the client's link
    // is slow. The SSE stream is long-lived by design: it reports an ingest that
    // may run for hours, and timing it out would blank the progress bar on a job
    // that is still running perfectly well. The browse endpoints call out to
    // Browsertrix and Archive-It, where the request is only as quick as someone
    // else's service — a bound there belongs on the outbound client, which has
    // none either (`rustyweb-stream-fetch-timeouts-v3su`), rather than on a
    // curator's page.
    //
    // All of these require a curator, so none is reachable by the anonymous
    // caller this ticket is about.
    let manage_streaming = Router::new()
        // A whole WACZ, so axum's 2 MB default is lifted on this route only:
        // to the configured per-upload cap when there is one, with room for
        // the multipart framing and the two short text fields around the file
        // (the exact count on the file itself is in `stream_field_to_file`),
        // and off entirely when there is not.
        .route(
            "/api/archives/upload",
            post(upload_archive).layer(match upload_limits.per_upload {
                Some(cap) => DefaultBodyLimit::max(
                    usize::try_from(cap.saturating_add(UPLOAD_FRAMING_ALLOWANCE))
                        .unwrap_or(usize::MAX),
                ),
                None => DefaultBodyLimit::disable(),
            }),
        )
        .route("/api/archives/{id}/events", get(add_archive_events))
        .route("/api/browsertrix/orgs", get(bx_orgs))
        .route("/api/browsertrix/collections", get(bx_collections))
        .route("/api/browsertrix/items", get(bx_items))
        .route("/api/archiveit/collections", get(ait_collections))
        .route("/api/archiveit/crawls", get(ait_crawls));

    let mut manage_routes = manage_routes.merge(manage_streaming);

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

    app = app
        .merge(streaming)
        .merge(health_route)
        .merge(manage_routes);

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
    // and nothing else. On a workstation a write's `Host` must also be a
    // loopback name; see [`CsrfPolicy::Loopback`].
    let csrf = Arc::new(csrf);

    // Replay alone, on its own listener and so its own origin. Everything here
    // is public, read-only and anonymous: the frame, ReplayWeb.page and its
    // service worker, and the archive bytes and collection manifests it reads.
    // No write route, no forward-auth, nothing that resolves or renders an
    // identity. That is a property of this list rather than of a check, so it
    // cannot be undone by a route mounted somewhere else.
    let replay = Router::new()
        .route("/replay-frame", get(replay_frame))
        .route("/replay/", get(replay_index))
        .route("/replay/{*path}", get(replay_asset))
        .route("/collection/{id}/replay.json", get(collection_replay_json))
        .route("/collection/{id}/pages", get(collection_pages))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            limits::limit_quick,
        ))
        .merge(Router::new().route("/files/{id}", get(serve_file)).layer(
            axum::middleware::from_fn_with_state(state.clone(), limits::limit_streams),
        ))
        .route("/health", get(health));

    // Who may frame replay: wherever the viewer is served from. A workstation
    // knows its own listener; a server, what `--site-url` says. Empty only for a
    // workstation router built without listeners, as tests do.
    let ancestors = match (access_is_local, config.app_addr, &config.site) {
        (true, Some(addr), _) => headers::loopback_origins(addr),
        (false, _, Some(site)) => vec![site.0.clone()],
        _ => Vec::new(),
    };
    let app_headers = headers::SecurityHeaders::app(config.replay_origin.as_ref());
    let replay_headers = headers::SecurityHeaders::replay(&ancestors);

    Ok(Routers {
        app: outer_layers(app, csrf.clone(), app_headers).with_state(state.clone()),
        // The CSRF guard has nothing to refuse here, since every route is a
        // GET and the guard checks only writes. It is applied anyway so the two
        // listeners share one layer stack.
        replay: outer_layers(replay, csrf, replay_headers).with_state(state),
    })
}

/// The layers both routers share, outermost last.
fn outer_layers(
    router: Router<Arc<AppState>>,
    csrf: Arc<CsrfPolicy>,
    security: headers::SecurityHeaders,
) -> Router<Arc<AppState>> {
    router
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
                    let id = req.extensions().get::<RequestId>().copied();
                    tracing::info_span!(
                        "request",
                        id = id.map(tracing::field::display),
                        method = %req.method(),
                        uri = %req.uri(),
                        client_ip = %ip,
                    )
                })
                .on_response(
                    |res: &Response, latency: std::time::Duration, _span: &tracing::Span| {
                        // A limit's refusal is reported by the limit itself, once
                        // per client per window and naming who was charged. Logging
                        // each one here as well would put back the flood the
                        // reporter exists to prevent. See `limits::Reporter`.
                        if res.extensions().get::<limits::LimitRefusal>().is_some() {
                            tracing::debug!(status = res.status().as_u16(), "refused by a limit");
                            return;
                        }
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
        // Outside the trace layer, so the span it makes can carry the id.
        .layer(axum::middleware::from_fn(assign_request_id))
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
        // Outside the CSRF guard, so its refusals carry the headers too.
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                security.clone().apply(req, next)
            },
        ))
}

/// Bind and serve. `config` says who is trusted (see [`Access`]); `providers`
/// supplies authenticated import clients (Browsertrix, Archive-It) for the UI.
///
/// Replay gets a listener of its own, at `replay_bind` or, by default, the
/// port after the app's on the same address.
pub async fn serve_with_resolver(
    bind: &str,
    replay_bind: Option<&str>,
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    config: ServerConfig,
    providers: Providers,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let app_addr = listener.local_addr()?;
    let replay_listener = match replay_bind {
        Some(addr) => tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| anyhow::anyhow!("could not bind the replay listener to {addr}: {e}"))?,
        None => {
            // The next port, unless the app's was chosen by the OS, in which
            // case so is this one.
            let port = match app_addr.port() {
                0 => 0,
                p => p.checked_add(1).ok_or_else(|| {
                    anyhow::anyhow!("the app is on port {p}, which leaves no next port for replay")
                })?,
            };
            let addr = std::net::SocketAddr::new(app_addr.ip(), port);
            tokio::net::TcpListener::bind(addr).await.map_err(|e| {
                anyhow::anyhow!(
                    "replay runs on its own port, the one after the app's, and {addr} could \
                     not be bound: {e}"
                )
            })?
        }
    };
    tracing::info!("listening on {app_addr}");
    serve_on_listeners(listener, replay_listener, home, resolver, config, providers).await
}

/// Serve on already-bound listeners, one for the app and one for replay. This
/// lets a caller bind `127.0.0.1:0`, read back the OS-assigned ports via
/// [`TcpListener::local_addr`], and only then serve, which is what the test
/// suite does.
///
/// Refuses to start a server with no [`ServerConfig::replay_origin`]: without
/// one, replay would have to run on indice's own origin, which is the hole
/// [`ReplayOrigin`] exists to close. A workstation's replay origin is the
/// replay listener's own address, whatever the config says.
///
/// [`TcpListener::local_addr`]: tokio::net::TcpListener::local_addr
pub async fn serve_on_listeners(
    listener: tokio::net::TcpListener,
    replay_listener: tokio::net::TcpListener,
    home: &Path,
    resolver: Option<Arc<dyn crate::index::SourceResolver>>,
    mut config: ServerConfig,
    providers: Providers,
) -> Result<()> {
    let replay_addr = replay_listener.local_addr()?;
    // [`Access::local`] already refuses a non-loopback address, but it is given
    // the address the *caller* intends to bind rather than the one the listener
    // actually got. Re-check against the real sockets, which are the only
    // things that know: a caller passing 127.0.0.1 and handing over a listener
    // bound to 0.0.0.0 would otherwise slip through.
    if config.access.is_local() {
        for addr in [listener.local_addr()?, replay_addr] {
            if !addr.ip().is_loopback() {
                anyhow::bail!(
                    "refusing to start: local access trusts every caller, so it must be \
                     bound to a loopback address (127.0.0.1 / ::1), but a listener is \
                     bound to {addr}. Configure an authenticating proxy to run as a server."
                );
            }
        }
        config.replay_origin = Some(ReplayOrigin::for_listener(replay_addr));
        config.app_addr = Some(listener.local_addr()?);
    } else if config.replay_origin.is_none() {
        // The fact, not the flag: indice-bin names `--replay-url` when it
        // prints this, as it does for the loopback refusal.
        anyhow::bail!(
            "refusing to start: this indice is reachable over the network and has no \
             replay origin. Archived pages run their own JavaScript, and on indice's \
             own origin that script could act as whoever replays it."
        );
    }
    let origin = config.replay_origin.clone();
    let Routers { app, replay } = build_routers(home, resolver, config, providers)?;
    if let Some(origin) = origin {
        tracing::info!("replay: {} (listening on {replay_addr})", origin.as_str());
    }
    let app = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    );
    let replay = axum::serve(
        replay_listener,
        replay.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    );
    // Either one stopping stops the server: half of indice is not a state
    // worth leaving running.
    tokio::try_join!(async { app.await.map_err(anyhow::Error::from) }, async {
        replay.await.map_err(anyhow::Error::from)
    },)?;
    Ok(())
}
