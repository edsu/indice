//! How much one client, and everyone together, may ask of a server at once.
//!
//! Two kinds of route need different limits, and the numbers come from watching
//! a real replay rather than guessing (the measurement is described on
//! [`STREAMS_PER_CLIENT`]).
//!
//! - **Quick routes** answer from memory, the index or an embedded asset: pages,
//!   search, the APIs, static files. They get a global cap on how many may be in
//!   flight, which answers `503` at once when saturated rather than queueing,
//!   and a per-client rate, which answers `429`. A replay makes about twenty of
//!   these, so the rate is set well clear of it.
//! - **`/files/{id}`** streams archive bytes, and one replay of a heavy page made
//!   226 of these requests in seconds, so a request rate would break replay. What
//!   it needs bounding is the number of streams open at once: each holds a file
//!   descriptor for as long as its body lives, and a client reading slowly can
//!   hold one for as long as it likes. It gets a per-client and a global cap on
//!   open streams, counted until the body is dropped rather than until the
//!   handler returns, since a streaming handler returns at once.
//!
//! Only a server is limited. A workstation has one user and nothing to protect
//! them from. On a server, curators and admins are exempt from the per-client
//! limits, so an institution's own script that reads hard needs a roster entry
//! rather than a setting; the global caps apply to everyone, because they are
//! what keeps the process alive.
//!
//! Hand-rolled rather than `tower-governor` plus `tower`'s limit layers: the
//! stream cap has to hold its slot in the response body, which no off-the-shelf
//! layer does, and once that mechanism exists the rest is a few lines on top of
//! it with no new dependency.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes, HttpBody};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::AppState;

/// Quick requests in flight across all clients. These answer in milliseconds, so
/// reaching this means the server is being asked for far more than any
/// audience of readers produces, and refusing at once beats queueing: a queue
/// of index reads is memory and latency for everyone, attacker or not.
const QUICK_CONCURRENCY: usize = 256;

/// Sustained quick requests per second, per client.
const RATE_PER_SECOND: f64 = 20.0;

/// Quick requests a client may make in a burst before the rate applies. A page
/// and its assets arrive together; a replay's quick requests total about
/// twenty. Five times that leaves a person clicking around nowhere near it.
const BURST: f64 = 100.0;

/// Open `/files` streams per client.
///
/// Measured, not guessed: a real Chrome replaying the StoryMaps fixture held at
/// most **6** `/files` bodies open at once, out of 226 requests. Six is Chrome's
/// connection limit per host over HTTP/1.1, so the browser is the bound there.
/// Behind an HTTP/2 proxy a browser can multiplex more, and that was not
/// measured, so this is ten times the observed number.
const STREAMS_PER_CLIENT: usize = 64;

/// Open `/files` streams across all clients. Each is a file descriptor; this
/// sits well below the usual descriptor limits, so a crowd of slow readers runs
/// out of streams before the process runs out of descriptors.
const STREAM_CONCURRENCY: usize = 512;

/// Clients whose rate is tracked at once. A bucket that has refilled is the same
/// as no bucket, so those are swept when this is reached. Past it, a new client
/// goes untracked rather than refused: the global caps still bound the total,
/// and refusing every newcomer would hand an attacker with enough addresses a
/// way to lock out everyone else.
const MAX_TRACKED_CLIENTS: usize = 50_000;

/// How long a refusal is summarised over before it is reported again.
const REPORT_WINDOW: Duration = Duration::from_secs(60);

/// How often the sweeper looks for windows that have ended.
const SWEEP_EVERY: Duration = Duration::from_secs(10);

/// Who is asking, for per-client limits.
///
/// IPv6 is keyed by its /64. A host is normally given a whole /64, so keying by
/// the full address would let one machine rotate through billions of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum ClientKey {
    V4(u32),
    V6(u64),
}

impl ClientKey {
    fn of(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => ClientKey::V4(u32::from(v4)),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => ClientKey::V4(u32::from(v4)),
                None => ClientKey::V6((u128::from(v6) >> 64) as u64),
            },
        }
    }
}

/// The client's address: the TCP peer, or, behind a proxy, the address the
/// proxy says it is forwarding for.
///
/// `X-Forwarded-For` is only believed from a peer on this machine or this
/// network, which is where a reverse proxy sits. From anywhere else it is just a
/// header the client wrote, and believing it would let one client present a new
/// address on every request. The same `is_internal_addr` that decides what a
/// curator's URL may fetch decides this.
///
/// The entry used is the rightmost one that is not itself internal: each proxy
/// appends the address it received from, so entries to the left of that were
/// written by the client and prove nothing. If every entry is internal the
/// client is on the private network too, and the leftmost is it.
pub(super) fn client_ip(peer: IpAddr, headers: &HeaderMap) -> IpAddr {
    use crate::http_range::is_internal_addr;
    if !is_internal_addr(peer) {
        return peer;
    }
    let forwarded: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(parse_forwarded)
        .collect();
    forwarded
        .iter()
        .rev()
        .find(|ip| !is_internal_addr(**ip))
        .or_else(|| forwarded.first())
        .copied()
        .unwrap_or(peer)
}

/// One `X-Forwarded-For` entry. Usually a bare address; some proxies add a port,
/// which is `[v6]:port` or `v4:port`.
fn parse_forwarded(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    entry
        .parse::<IpAddr>()
        .ok()
        .or_else(|| entry.parse::<SocketAddr>().ok().map(|s| s.ip()))
}

impl std::fmt::Display for ClientKey {
    /// The address that was charged: an IPv4 address, or an IPv6 /64.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientKey::V4(v) => write!(f, "{}", std::net::Ipv4Addr::from(*v)),
            ClientKey::V6(prefix) => write!(
                f,
                "{}/64",
                std::net::Ipv6Addr::from(u128::from(*prefix) << 64)
            ),
        }
    }
}

/// Whose refusal is being reported: one client's, or the server's as a whole
/// when a global cap is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Subject {
    Client(ClientKey),
    Server,
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Subject::Client(key) => write!(f, "{key}"),
            Subject::Server => f.write_str("the server"),
        }
    }
}

/// Which limit refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Reason {
    Rate,
    Streams,
    Busy,
    StreamsBusy,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reason::Rate => write!(
                f,
                "past its burst of {BURST} requests and then {RATE_PER_SECOND} a second"
            ),
            Reason::Streams => {
                write!(
                    f,
                    "already holding {STREAMS_PER_CLIENT} open archive streams"
                )
            }
            Reason::Busy => write!(
                f,
                "{QUICK_CONCURRENCY} requests already in flight across all clients"
            ),
            Reason::StreamsBusy => write!(
                f,
                "{STREAM_CONCURRENCY} archive streams already open across all clients"
            ),
        }
    }
}

/// One subject's refusals since the first, for the summary.
#[derive(Debug, Clone, Copy)]
struct Window {
    started: Instant,
    count: u64,
}

/// What the operator is told about refusals.
///
/// The request log already has a line per response, but a line per refused
/// request is the wrong signal on every count. It names the TCP peer, which
/// behind a proxy is the proxy, not the client the limit charged. It does not
/// say which limit fired. And it is one line per refusal, so a flood that the
/// limits correctly turn away still fills the log at full speed, which makes
/// the log itself the lever. So a refusal is reported once when it starts,
/// naming the address that was charged and the limit, and then once more when
/// its window ends, with the count; the per-request line is dropped to `debug`.
/// That is at most a line or two per offending client per minute however hard
/// it is pushed.
///
/// It does log the addresses of clients it refuses, which is narrower than a
/// request log that records every visitor's: an abuse signal, not a visitor
/// record. See `rustyweb-open-web-hardening-u9y6.8` for the request log itself.
#[derive(Default)]
struct Reporter {
    windows: HashMap<(Subject, Reason), Window>,
    /// Whether a sweeper task is running. One is started by the first refusal
    /// and stops once there is nothing left to report, so an idle server runs
    /// no background task for this.
    sweeping: bool,
}

/// The line to log for a refusal, if it is the first of a window.
fn note_refusal(
    r: &mut Reporter,
    subject: Subject,
    reason: Reason,
    now: Instant,
) -> Option<String> {
    let key = (subject, reason);
    if let Some(w) = r.windows.get_mut(&key) {
        w.count += 1;
        return None;
    }
    // Bounded like the buckets, for the same reason: past the cap, a new
    // subject goes unreported rather than growing the map without end.
    if r.windows.len() >= MAX_TRACKED_CLIENTS {
        return None;
    }
    r.windows.insert(
        key,
        Window {
            started: now,
            count: 1,
        },
    );
    Some(format!("refusing {subject}: {reason}"))
}

/// Remove the windows that have ended, returning a summary line for each that
/// saw more than its first refusal.
fn drain_ended(r: &mut Reporter, now: Instant) -> Vec<String> {
    let mut lines = Vec::new();
    r.windows.retain(|(subject, reason), w| {
        if now.saturating_duration_since(w.started) < REPORT_WINDOW {
            return true;
        }
        if w.count > 1 {
            lines.push(format!(
                "refused {subject} {} times in {}s ({reason})",
                w.count,
                REPORT_WINDOW.as_secs()
            ));
        }
        false
    });
    lines
}

/// Report one refusal, starting the sweeper if none is running.
fn report(reporter: &Arc<Mutex<Reporter>>, subject: Subject, reason: Reason) {
    let mut r = reporter.lock().unwrap();
    if let Some(line) = note_refusal(&mut r, subject, reason, Instant::now()) {
        // Outside the request's span, whose `client_ip` is the TCP peer: behind
        // a proxy that is the proxy, and printing it beside the address the
        // limit charged is the confusion this report exists to remove.
        tracing::warn!(parent: None, "{line}");
    }
    if !r.sweeping && !r.windows.is_empty() {
        r.sweeping = true;
        let reporter = Arc::clone(reporter);
        // Started from inside a request, so a runtime is always there.
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SWEEP_EVERY).await;
                let mut r = reporter.lock().unwrap();
                for line in drain_ended(&mut r, Instant::now()) {
                    tracing::warn!("{line}");
                }
                if r.windows.is_empty() {
                    r.sweeping = false;
                    return;
                }
            }
        });
    }
}

/// Marks a response refused by a limit, so the request log can record it at
/// `debug` instead of once per refusal at `warn`. See [`Reporter`].
#[derive(Debug, Clone, Copy)]
pub(super) struct LimitRefusal;

/// A token bucket: up to [`BURST`] requests at once, refilling at
/// [`RATE_PER_SECOND`].
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn refilled(self, now: Instant) -> Self {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        Bucket {
            tokens: (self.tokens + elapsed * RATE_PER_SECOND).min(BURST),
            at: now,
        }
    }

    /// Spend one request, or say how long until one is available.
    fn take(&mut self, now: Instant) -> Result<(), Duration> {
        *self = self.refilled(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64(
                (1.0 - self.tokens) / RATE_PER_SECOND,
            ))
        }
    }
}

/// The limits' shared state. Present only on a server.
pub(super) struct RequestLimits {
    quick: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    buckets: Mutex<HashMap<ClientKey, Bucket>>,
    open_streams: Arc<Mutex<HashMap<ClientKey, usize>>>,
    reporter: Arc<Mutex<Reporter>>,
}

impl RequestLimits {
    pub(super) fn new() -> Self {
        Self {
            quick: Arc::new(Semaphore::new(QUICK_CONCURRENCY)),
            streams: Arc::new(Semaphore::new(STREAM_CONCURRENCY)),
            buckets: Mutex::new(HashMap::new()),
            open_streams: Arc::new(Mutex::new(HashMap::new())),
            reporter: Arc::new(Mutex::new(Reporter::default())),
        }
    }

    /// Spend one of this client's quick requests.
    fn take_quick(&self, key: ClientKey, now: Instant) -> Result<(), Duration> {
        let mut buckets = self.buckets.lock().unwrap();
        if !buckets.contains_key(&key) && buckets.len() >= MAX_TRACKED_CLIENTS {
            buckets.retain(|_, b| b.refilled(now).tokens < BURST);
            if buckets.len() >= MAX_TRACKED_CLIENTS {
                return Ok(());
            }
        }
        buckets
            .entry(key)
            .or_insert(Bucket {
                tokens: BURST,
                at: now,
            })
            .take(now)
    }

    /// Open one stream for this client, or refuse if it has too many.
    fn open_stream(&self, key: ClientKey) -> Option<StreamSlot> {
        let mut open = self.open_streams.lock().unwrap();
        let n = open.entry(key).or_insert(0);
        if *n >= STREAMS_PER_CLIENT {
            return None;
        }
        *n += 1;
        Some(StreamSlot {
            key,
            open: Arc::clone(&self.open_streams),
        })
    }
}

/// One client's open stream, released when the body that holds it is dropped.
struct StreamSlot {
    key: ClientKey,
    open: Arc<Mutex<HashMap<ClientKey, usize>>>,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut open = self.open.lock().unwrap();
        if let Some(n) = open.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                open.remove(&self.key);
            }
        }
    }
}

/// A response body that holds its limit slots until it is finished with.
///
/// Delegates everything to the body inside, including the size hint, which is
/// what lets hyper send `Content-Length` rather than chunk a ranged read.
struct HeldBody {
    inner: Body,
    _held: (OwnedSemaphorePermit, Option<StreamSlot>),
}

impl HttpBody for HeldBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, axum::Error>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// Whether this caller is exempt from the per-client limits: a curator or an
/// admin, vouched for by the proxy. A display cookie does not count, for the
/// reason it never counts — it renders, it does not authorize.
fn exempt(state: &AppState, headers: &HeaderMap) -> bool {
    super::auth::resolve_caller(state, headers)
        .is_some_and(|(p, evidence)| evidence.admits_writes() && p.role().can_curate())
}

/// The client to charge, or `None` when no limits apply or the peer is unknown.
fn charge(state: &AppState, req: &axum::extract::Request) -> Option<ClientKey> {
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()?
        .0
        .ip();
    if exempt(state, req.headers()) {
        return None;
    }
    Some(ClientKey::of(client_ip(peer, req.headers())))
}

fn busy() -> Response {
    let mut res = (
        StatusCode::SERVICE_UNAVAILABLE,
        "indice is busy; try again in a moment",
    )
        .into_response();
    res.headers_mut().insert(
        axum::http::header::RETRY_AFTER,
        HeaderValue::from_static("1"),
    );
    res.extensions_mut().insert(LimitRefusal);
    res
}

fn too_many(wait: Duration, what: &str) -> Response {
    let secs = wait.as_secs_f64().ceil().max(1.0) as u64;
    let mut res = (
        StatusCode::TOO_MANY_REQUESTS,
        format!("too many {what} from this address; try again in {secs}s"),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
        res.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
    }
    res.extensions_mut().insert(LimitRefusal);
    res
}

/// Middleware for the quick routes.
pub(super) async fn limit_quick(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let Some(limits) = &state.limits else {
        return next.run(req).await;
    };
    let Ok(permit) = Arc::clone(&limits.quick).try_acquire_owned() else {
        report(&limits.reporter, Subject::Server, Reason::Busy);
        return busy();
    };
    if let Some(key) = charge(&state, &req) {
        if let Err(wait) = limits.take_quick(key, Instant::now()) {
            report(&limits.reporter, Subject::Client(key), Reason::Rate);
            return too_many(wait, "requests");
        }
    }
    let (parts, body) = next.run(req).await.into_parts();
    Response::from_parts(
        parts,
        Body::new(HeldBody {
            inner: body,
            _held: (permit, None),
        }),
    )
}

/// Middleware for `/files/{id}`.
pub(super) async fn limit_streams(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let Some(limits) = &state.limits else {
        return next.run(req).await;
    };
    let Ok(permit) = Arc::clone(&limits.streams).try_acquire_owned() else {
        report(&limits.reporter, Subject::Server, Reason::StreamsBusy);
        return busy();
    };
    let slot = match charge(&state, &req) {
        Some(key) => match limits.open_stream(key) {
            Some(slot) => Some(slot),
            None => {
                report(&limits.reporter, Subject::Client(key), Reason::Streams);
                return too_many(Duration::from_secs(1), "open archive streams");
            }
        },
        None => None,
    };
    let (parts, body) = next.run(req).await.into_parts();
    Response::from_parts(
        parts,
        Body::new(HeldBody {
            inner: body,
            _held: (permit, slot),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn xff(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn forwarded_for_is_believed_only_from_a_proxy_on_this_network() {
        // From the open internet the header is the client's own writing. (Real
        // public addresses here: the documentation ranges count as internal,
        // since `is_internal_addr` treats anything not clearly public that way.)
        assert_eq!(client_ip(ip("1.0.0.1"), &xff("4.4.4.4")), ip("1.0.0.1"));
        // From a proxy on the docker network or this machine, it is the client.
        assert_eq!(client_ip(ip("172.18.0.3"), &xff("8.8.8.8")), ip("8.8.8.8"));
        assert_eq!(client_ip(ip("127.0.0.1"), &xff("8.8.8.8")), ip("8.8.8.8"));
        // No header behind a proxy: the proxy is all there is to go on.
        assert_eq!(
            client_ip(ip("127.0.0.1"), &HeaderMap::new()),
            ip("127.0.0.1")
        );
    }

    #[test]
    fn a_client_cannot_choose_its_address_by_prepending_one() {
        // The client sends `X-Forwarded-For: 1.1.1.1` hoping to be someone else;
        // the proxy appends the address it actually received from. The rightmost
        // non-internal entry is the proxy's account, so that is the one used.
        let h = xff("1.1.1.1, 9.9.9.9");
        assert_eq!(client_ip(ip("172.18.0.3"), &h), ip("9.9.9.9"));
        // A chain of internal proxies: skip them to reach the client.
        let h = xff("9.9.9.9, 10.0.0.2, 172.18.0.5");
        assert_eq!(client_ip(ip("127.0.0.1"), &h), ip("9.9.9.9"));
        // Ports and IPv6 forms some proxies write.
        assert_eq!(
            client_ip(ip("127.0.0.1"), &xff("9.9.9.9:4711")),
            ip("9.9.9.9")
        );
        assert_eq!(
            client_ip(ip("127.0.0.1"), &xff("[2606:4700::1]:443")),
            ip("2606:4700::1")
        );
    }

    #[test]
    fn ipv6_clients_are_keyed_by_their_64() {
        // Two addresses in one /64 are one host, as far as limits go.
        assert_eq!(
            ClientKey::of(ip("2001:db8:1:2::1")),
            ClientKey::of(ip("2001:db8:1:2:ffff:ffff:ffff:ffff"))
        );
        assert_ne!(
            ClientKey::of(ip("2001:db8:1:2::1")),
            ClientKey::of(ip("2001:db8:1:3::1"))
        );
        // A mapped IPv4 address is that IPv4 client.
        assert_eq!(
            ClientKey::of(ip("::ffff:9.9.9.9")),
            ClientKey::of(ip("9.9.9.9"))
        );
    }

    #[test]
    fn the_rate_allows_a_burst_then_refills() {
        let limits = RequestLimits::new();
        let key = ClientKey::of(ip("9.9.9.9"));
        let t0 = Instant::now();
        for _ in 0..BURST as usize {
            assert!(limits.take_quick(key, t0).is_ok(), "the burst is allowed");
        }
        let wait = limits.take_quick(key, t0).unwrap_err();
        assert!(wait > Duration::ZERO, "past the burst, a client waits");
        // A different client is unaffected.
        assert!(limits.take_quick(ClientKey::of(ip("8.8.8.8")), t0).is_ok());
        // One request's worth of time later, one more is allowed.
        let later = t0 + Duration::from_secs_f64(1.0 / RATE_PER_SECOND);
        assert!(limits.take_quick(key, later).is_ok());
    }

    #[test]
    fn a_refusal_is_reported_once_then_summarised() {
        let mut r = Reporter::default();
        let client = Subject::Client(ClientKey::of(ip("9.9.9.9")));
        let t0 = Instant::now();

        let first = note_refusal(&mut r, client, Reason::Rate, t0).expect("the first is logged");
        assert!(
            first.contains("9.9.9.9") && first.contains("burst of 100"),
            "names the client charged and the limit: {first}"
        );
        // The rest of the window is counted, not logged.
        for _ in 0..198 {
            assert!(note_refusal(&mut r, client, Reason::Rate, t0).is_none());
        }
        // A different limit on the same client is its own report.
        assert!(note_refusal(&mut r, client, Reason::Streams, t0).is_some());

        // Before the window ends, nothing more.
        assert!(drain_ended(&mut r, t0 + Duration::from_secs(30)).is_empty());
        // After, one summary with the total, and the windows are gone.
        let lines = drain_ended(&mut r, t0 + REPORT_WINDOW);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("9.9.9.9") && l.contains("199 times")),
            "the summary carries the count: {lines:?}"
        );
        assert!(r.windows.is_empty(), "ended windows are removed");
        // A single refusal needs no summary; its first line said it all.
        assert_eq!(lines.len(), 1, "the one-off Streams refusal adds nothing");
    }

    #[test]
    fn an_ipv6_client_is_reported_as_its_64() {
        let key = ClientKey::of(ip("2001:db8:1:2::abcd"));
        assert_eq!(key.to_string(), "2001:db8:1:2::/64");
    }

    #[test]
    fn a_stream_slot_is_released_when_dropped() {
        let limits = RequestLimits::new();
        let key = ClientKey::of(ip("9.9.9.9"));
        let slots: Vec<_> = (0..STREAMS_PER_CLIENT)
            .map(|_| limits.open_stream(key).expect("within the cap"))
            .collect();
        assert!(limits.open_stream(key).is_none(), "past the cap is refused");
        drop(slots);
        assert!(
            limits.open_stream(key).is_some(),
            "freed slots can be reused"
        );
        assert!(
            limits.open_streams.lock().unwrap().len() <= 1,
            "a client with no open streams is forgotten"
        );
    }
}
