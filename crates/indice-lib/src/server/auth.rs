//! Authentication for management mode: the forward-auth middleware, the
//! signed session cookie, and login/logout.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};

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
    trusted: Option<&str>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if same_site_request(req.method(), req.headers(), req.uri(), trusted) {
        return next.run(req).await;
    }
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
    trusted: Option<&str>,
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
            expected_authority(headers, uri, trusted)
                .is_some_and(|ours| ours.eq_ignore_ascii_case(theirs))
        }
        None => !headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == "cross-site" || v == "same-site"),
    }
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

/// Whether this request may use management affordances, plus the signed-in user.
/// Local mode trusts every request (it's loopback-only); forward-auth defers to
/// [`check_forward_auth`]. Used by the read handlers to decide whether to render
/// the workroom chrome and edit-in-place controls.
pub(super) fn admin_ctx(state: &AppState, headers: &HeaderMap) -> (bool, Option<String>) {
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
