//! Authentication for management mode: the forward-auth middleware, the
//! signed session cookie, and login/logout.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};

use crate::identity::{Principal, Role, SubjectId};

use super::*;

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

/// Forward-auth middleware for the management routes: allow the request through
/// only if it carries the shared secret in `X-Indice-Auth-Secret` (matching the
/// configured value) **and** a non-empty identity in the configured user header —
/// both injected by the trusted proxy. Anything else (a forged identity header, a
/// request that skipped the proxy) gets 403.
pub(super) async fn forward_auth(
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

/// Cross-site request forgery guard for the management write surface: reject a
/// state-changing request that a *different* site's page initiated.
///
/// This is layered on the management routes unconditionally — including in local
/// (loopback) mode, which is the mode that needs it most. A loopback bind is not
/// a boundary a browser respects: while `serve --manage` is running, any page the
/// operator visits can POST a form to `http://127.0.0.1:8080/...`. The browser
/// sends it, and because the write routes take `Form`/`Multipart` (simple content
/// types) there is no CORS preflight to stop it. The attacker can't *read* the
/// reply, but by then the collection is already deleted.
pub(super) async fn same_origin_guard(
    policy: &CsrfPolicy,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if same_site_request(req.method(), req.headers(), req.uri(), policy) {
        return next.run(req).await;
    }
    let trusted = policy.site_authority.as_deref();
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("(none)")
        .to_string();
    let site = expected_authority(req.headers(), req.uri(), trusted).unwrap_or("(unknown)");
    (
        StatusCode::FORBIDDEN,
        format!(
            "cross-site request blocked: Origin {origin} does not match this site ({site}). \
             If indice is behind a proxy that rewrites the Host header, start it with \
             --site-url <your public URL>."
        ),
    )
        .into_response()
}

/// Whether this state-changing request was initiated by this same site.
///
/// Three rules, in order:
/// 1. Safe methods (GET/HEAD/OPTIONS/TRACE) change nothing, so they're never gated.
/// 2. If `Origin` is present, its authority must equal ours. `Origin: null`
///    (a sandboxed or `data:` initiator) has no authority and is refused.
/// 3. If `Origin` is absent, fall back to `Sec-Fetch-Site`.
///
/// **Why an absent `Origin` is allowed**, which looks backwards but isn't: per
/// Fetch, browsers append `Origin` to *every* request whose method isn't GET or
/// HEAD — cross-origin form navigations included (Firefox was the last holdout
/// and fixed this in FF 70). Historically the header was omitted only on
/// *same-origin* POSTs, never cross-origin ones. So its absence means the caller
/// isn't a browser — curl, our own `ureq` tests, a health checker — and a
/// non-browser carries no ambient credentials to ride. Refusing on absence would
/// break every non-browser client for no security gain.
///
/// `Sec-Fetch-Site` is only a fallback, never the primary signal: Fetch Metadata
/// headers are sent only to *potentially trustworthy* origins, so a plain-HTTP
/// deployment on a real hostname (the shipped `compose.yaml` default,
/// `SITE_ADDRESS=:80`) never receives them. `same-site` is refused alongside
/// `cross-site` because every form indice renders is served by indice itself, so
/// a sibling subdomain is never a legitimate initiator.
pub(super) fn same_site_request(
    method: &Method,
    headers: &HeaderMap,
    uri: &Uri,
    policy: &CsrfPolicy,
) -> bool {
    if matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    ) {
        return true;
    }
    match headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        Some(origin) => {
            // Compare the authority (`host[:port]`) only, never the scheme: an
            // http page attacking the https same-host site already requires a
            // network MITM, and comparing schemes would break any deployment
            // that terminates TLS without setting X-Forwarded-Proto. Both sides
            // elide the default port for their scheme, so this is exact.
            let Some(theirs) = origin.split_once("://").map(|(_, rest)| rest) else {
                return false; // "null", or anything else without an authority
            };
            let Some(ours) = expected_authority(headers, uri, policy.site_authority.as_deref())
            else {
                return false;
            };
            // Matching Host against Origin is not enough on its own when the
            // browser chose the Host: DNS rebinding defeats it. An attacker who
            // controls evil.example can point it at 127.0.0.1, and the browser
            // then sends a *self-consistent* pair (Host and Origin both
            // evil.example) at the loopback workroom. Local mode is loopback-only
            // anyway — the server refuses to start otherwise — so any authority
            // that isn't a loopback name is bogus by definition.
            if policy.require_loopback && !is_loopback_authority(ours) {
                return false;
            }
            ours.eq_ignore_ascii_case(theirs)
        }
        None => !headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == "cross-site" || v == "same-site"),
    }
}

/// How this server decides whether a write was same-site.
#[derive(Clone, Default)]
pub(super) struct CsrfPolicy {
    /// The operator-pinned public authority (`--site-url`), when set.
    pub site_authority: Option<String>,
    /// Whether to additionally require the request's authority to be a loopback
    /// name. True exactly in local (loopback-trust) mode with no `--site-url`,
    /// where `Host` comes straight from the browser and so can be rebound.
    pub require_loopback: bool,
}

/// Whether an authority names this machine. Accepts `localhost`, the IPv4
/// loopback block (`127.0.0.0/8`), and IPv6 `::1` in its bracketed form.
fn is_loopback_authority(authority: &str) -> bool {
    // Strip the port. An IPv6 literal is bracketed, so split after the bracket.
    let host = match authority.rsplit_once(']') {
        Some((bracketed, _)) => bracketed.trim_start_matches('[').to_string(),
        None => authority
            .rsplit_once(':')
            .map_or(authority, |(h, _)| h)
            .to_string(),
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// The authority (`host[:port]`) a browser would have used to reach us.
///
/// `--site-url` wins when set, then `X-Forwarded-Host` (which Caddy sets and both
/// shipped Caddyfiles rely on), then `Host`, then the URI's authority (HTTP/2,
/// where the authority is a pseudo-header rather than `Host`). Trusting
/// `X-Forwarded-Host` is safe *for this purpose*: a CSRF attacker drives a
/// browser, and script cannot set `Host`, `Origin`, `Sec-*`, or `X-Forwarded-*`.
fn expected_authority<'a>(
    headers: &'a HeaderMap,
    uri: &'a Uri,
    trusted: Option<&'a str>,
) -> Option<&'a str> {
    if trusted.is_some() {
        return trusted;
    }
    headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(axum::http::header::HOST))
        .and_then(|v| v.to_str().ok())
        // Chained proxies (CDN in front of Caddy, or two reverse proxies) append
        // rather than replace, so this can arrive as a list —
        // `archive.example.org, indice:8080`. The client-facing hop is first,
        // and that is the one the browser's Origin reflects. Without this a
        // chained deployment 403s every management write with nothing
        // misconfigured on the operator's side.
        .map(|v| v.split(',').next().unwrap_or(v).trim())
        .filter(|v| !v.is_empty())
        .or_else(|| uri.authority().map(|a| a.as_str()))
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

/// How a request's identity was established.
///
/// Load-bearing, and the reason this is a type rather than a comment. The
/// display cookie is good enough to *render* the workroom chrome but must never
/// be good enough to *write*: indice issues it itself, so it outlives the
/// proxy's session, and it is the one credential an XSS could ride. That rule
/// used to be enforced only by which router block a route was mounted in —
/// true, but invisible, and silently lost the moment a route moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Evidence {
    /// Loopback `--manage` with no proxy: the machine's operator.
    Loopback,
    /// The trusted proxy injected its identity header *and* the shared secret
    /// on this very request.
    Proxy,
    /// indice's own signed display cookie. Rendering only.
    Cookie,
}

impl Evidence {
    /// Whether this evidence is strong enough to authorize a state change.
    pub(super) fn admits_writes(self) -> bool {
        matches!(self, Evidence::Loopback | Evidence::Proxy)
    }
}

/// Resolve who is making this request, and how we know.
///
/// The single place identity and role are decided. `None` means anonymous —
/// either management is off, or nothing vouched for this request.
pub(super) fn resolve_caller(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<(Principal, Evidence)> {
    if !state.management {
        return None;
    }
    let Some(fa) = &state.forward_auth else {
        // Local mode: loopback-only (enforced at startup), so the operator is
        // the admin and the roster has no authentication to filter.
        return Some((Principal::local_operator(), Evidence::Loopback));
    };
    // The live proxy-injected identity, or — on the ungated public pages the
    // browser can't send proxy credentials to — the display cookie set at login.
    let (raw, evidence) = match check_forward_auth(fa, headers) {
        Some(u) => (u, Evidence::Proxy),
        None => (session_cookie_user(fa, headers)?, Evidence::Cookie),
    };
    // `parse_remote`: a proxy identity must never resolve to the local operator.
    let id = SubjectId::parse_remote(&raw)?;
    Some((state.users.resolve(id), evidence))
}

/// Whether this request may use management affordances, plus the signed-in user.
///
/// Rendering only — it deliberately accepts cookie evidence, so a signed-in
/// admin sees the workroom chrome on the ungated public pages. Authorization
/// for an actual write goes through the [`Curator`]/[`Admin`] extractors, which
/// additionally require [`Evidence::admits_writes`].
pub(super) fn admin_ctx(state: &AppState, headers: &HeaderMap) -> (bool, Option<String>) {
    match resolve_caller(state, headers) {
        // Local mode has no login and no distinct identities, so there is no
        // "signed in as" to show — matching what the appbar did before roles.
        Some((p, Evidence::Loopback)) => (p.role().can_curate(), None),
        Some((p, _)) => (p.role().can_curate(), Some(p.display_name().to_string())),
        None => (false, None),
    }
}

// ── Capability tokens ───────────────────────────────────────────────────────
//
// `Curator` and `Admin` are axum extractors *and* witness types: the inner
// field is private to this module, so nothing outside `auth.rs` can build one.
// Holding a `Curator` is therefore proof that the check ran.
//
// That is what makes the privilege un-forgettable. A handler declares what it
// needs in its own signature, and the privileged internal helpers take a
// `&Curator`/`&Admin` they can't be called without — so adding a route and
// forgetting to gate it is a *type error*, not a silent exposure. Same move as
// `CollectionId`'s private field + `parse()`, applied to a permission instead
// of a path component.

/// Proof that this request may accession and describe: create collections, add
/// and upload crawls, edit finding aids, run imports, annotate.
pub(super) struct Curator(Principal);

/// Proof that this request may do the irreversible, shared things: delete a
/// crawl, delete a collection, moderate anyone's notes.
pub(super) struct Admin(Principal);

impl Curator {
    pub(super) fn principal(&self) -> &Principal {
        &self.0
    }
}

impl Admin {
    pub(super) fn principal(&self) -> &Principal {
        &self.0
    }
}

/// Record who performed a state change.
///
/// Its own tracing target so an operator can route `indice::audit` somewhere
/// durable. This is the minimum viable audit trail — before it, indice kept no
/// record at all of who added or deleted anything, which is a poor look for a
/// tool whose whole proposition is provenance. A real append-only event log is
/// bead `rustyweb-audit-log-dnon`; this is the one-line down payment.
pub(super) fn audit(actor: &Principal, action: &str, target: &str) {
    tracing::info!(
        target: "indice::audit",
        actor = %actor.id(),
        role = ?actor.role(),
        action,
        target,
    );
}

/// Why a request was refused. All 403 — indice never issues a challenge (the
/// front proxy owns login), so a `WWW-Authenticate` header would be a lie.
pub(super) enum Denied {
    Unauthenticated,
    StaleEvidence,
    Insufficient(&'static str),
}

impl IntoResponse for Denied {
    fn into_response(self) -> Response {
        let msg: String = match self {
            Denied::Unauthenticated => {
                "forbidden: this action requires signing in via the front proxy".into()
            }
            Denied::StaleEvidence => {
                "forbidden: your sign-in has lapsed — reload the page to log in again".into()
            }
            Denied::Insufficient(what) => {
                format!("forbidden: your account is not authorized for {what}")
            }
        };
        (StatusCode::FORBIDDEN, msg).into_response()
    }
}

/// The shared gate: the evidence must be strong enough to authorize a write,
/// *and* the role must be high enough.
fn require(
    state: &AppState,
    headers: &HeaderMap,
    need: Role,
    what: &'static str,
) -> Result<Principal, Denied> {
    let Some((principal, evidence)) = resolve_caller(state, headers) else {
        return Err(Denied::Unauthenticated);
    };
    if !evidence.admits_writes() {
        // A display cookie alone. The user looks signed in and the chrome
        // rendered, but the proxy hasn't vouched for *this* request.
        return Err(Denied::StaleEvidence);
    }
    match principal.role() >= need {
        true => Ok(principal),
        false => Err(Denied::Insufficient(what)),
    }
}

impl axum::extract::FromRequestParts<Arc<AppState>> for Curator {
    type Rejection = Denied;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        require(
            state,
            &parts.headers,
            Role::Curator,
            "curating this archive",
        )
        .map(Curator)
    }
}

impl axum::extract::FromRequestParts<Arc<AppState>> for Admin {
    type Rejection = Denied;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        require(
            state,
            &parts.headers,
            Role::Admin,
            "deleting from this archive",
        )
        .map(Admin)
    }
}

/// Whether to offer a "Log in" link: forward-auth is configured but this request
/// is anonymous. Centralizes the rule the four page handlers share (`who` is the
/// identity from [`admin_ctx`]).
pub(super) fn login_available(state: &AppState, who: &Option<String>) -> bool {
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
pub(super) fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
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
pub(super) fn sign_session(secret: &str, user: &str, exp: u64) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = format!("{}|{}", b64.encode(user), exp);
    let sig = b64.encode(hmac_sha256(secret.as_bytes(), payload.as_bytes()));
    format!("{payload}|{sig}")
}

/// Verify a display-cookie value against `secret` at time `now`; returns the
/// identity iff the signature matches (constant-time) and it hasn't expired.
pub(super) fn verify_session(secret: &str, value: &str, now: u64) -> Option<String> {
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
pub(super) fn clear_session_cookie(res: &mut Response, secure: bool) {
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
pub(super) async fn manage_login(headers: HeaderMap) -> Response {
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
pub(super) fn local_redirect_target(referer: &str) -> Option<String> {
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
pub(super) async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
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
pub(super) async fn html_no_cache(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
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
