//! Read a remote WACZ over HTTP range requests as a `Read + Seek` stream, so the
//! `zip` crate and the CDX-guided extractor can operate on it **without
//! downloading the whole file**. Only the bytes actually needed — the ZIP
//! central directory, a few local headers, the CDX, and the selected page
//! records — are fetched. This is the same primitive wabac.js uses for replay.

use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

use anyhow::{Context, Result};

// ── Where an outbound fetch is allowed to land ──────────────────────────────

/// Whether a fetch may reach addresses on this machine or this network.
///
/// indice fetches a WACZ from wherever it is told to, which is a feature when
/// the operator is doing the telling and a server-side request forgery
/// primitive when a curator is. `POST /api/archives` takes a location straight
/// from the request body, so on a server the thing choosing the address may be
/// an approved stranger rather than the person who runs the machine.
///
/// What that buys an attacker is modest but real. The response has to parse as
/// a zip before indexing gets anywhere, so this is a blind request-proxy and
/// port-scanner rather than a way to read data back: the useful signal is in
/// which addresses answer and how fast. It still reaches things nothing else
/// can, and a university network has far more behind it than a rented VM.
/// Cloud metadata sits on 169.254.169.254 at most providers.
///
/// Default [`Unrestricted`](Self::Unrestricted), so the CLI, the test suite
/// (which indexes from a local server on 127.0.0.1) and a workstation all
/// behave exactly as before. The server opts in to [`PublicOnly`](Self::PublicOnly)
/// when it is running behind an auth proxy, which is the deployment where the
/// caller and the operator stop being the same person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FetchPolicy {
    /// Anything the host can route to. The operator chose the address.
    #[default]
    Unrestricted,
    /// Refuse loopback, link-local, private and other non-public addresses.
    PublicOnly,
}

/// Whether an address belongs to this machine or this network rather than the
/// public internet.
///
/// Deliberately conservative: anything not clearly out on the internet counts
/// as internal, because the cost of refusing an odd public address is an error
/// message and the cost of allowing an internal one is the whole point of the
/// check.
///
/// The IPv4-mapped case is the one worth knowing about. `::ffff:127.0.0.1` is a
/// perfectly ordinary way to write the loopback address in IPv6, and an
/// `Ipv6Addr` says `false` to `is_loopback()` for it, so a check that forgot to
/// unmap would wave through the exact address it was written to stop.
pub fn is_internal_addr(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // Multicast, 224.0.0.0/4. 224.0.0.1 is every host on this link.
                || v4.is_multicast()
                // 0.0.0.0/8, "this network".
                || o[0] == 0
                // 100.64.0.0/10, carrier-grade NAT. Not private by RFC 1918,
                // not routable on the public internet either.
                || (o[0] == 100 && (64..128).contains(&o[1]))
                // 192.0.0.0/24, IETF protocol assignments.
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                // 198.18.0.0/15, benchmarking.
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                // 240.0.0.0/4, reserved. `is_broadcast` catches only the last
                // address of it.
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            // An IPv4 address wearing an IPv6 hat, in either spelling.
            // `to_ipv4_mapped` handles ::ffff:a.b.c.d; the deprecated
            // IPv4-compatible form ::a.b.c.d is not mapped and would otherwise
            // match none of the checks below, so ::127.0.0.1 would read as a
            // public address.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal_addr(IpAddr::V4(v4));
            }
            if seg[..6].iter().all(|&h| h == 0) && !v6.is_loopback() && !v6.is_unspecified() {
                return is_internal_addr(IpAddr::V4(std::net::Ipv4Addr::new(
                    v6.octets()[12],
                    v6.octets()[13],
                    v6.octets()[14],
                    v6.octets()[15],
                )));
            }
            // 64:ff9b::/96 and 64:ff9b:1::/48, the NAT64 prefixes: on a NAT64
            // network 64:ff9b::a9fe:a9fe reaches 169.254.169.254.
            if seg[0] == 0x0064 && (seg[1] == 0xff9b) {
                let tail = v6.octets();
                return is_internal_addr(IpAddr::V4(std::net::Ipv4Addr::new(
                    tail[12], tail[13], tail[14], tail[15],
                )));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                // ff00::/8, which includes the interface- and link-local
                // all-nodes groups ff01::1 and ff02::1.
                || v6.is_multicast()
                // fe80::/10, link-local unicast. `is_unicast_link_local` is
                // still unstable, so spell it out.
                || (seg[0] & 0xffc0) == 0xfe80
                // fc00::/7, unique local. `is_unique_local` is unstable too.
                || (v6.octets()[0] & 0xfe) == 0xfc
                // 2001:db8::/32, documentation.
                || (seg[0] == 0x2001 && seg[1] == 0x0db8)
                // 100::/64, the discard-only prefix.
                || (seg[0] == 0x0100 && seg[1..4].iter().all(|&h| h == 0))
        }
    }
}

/// Whether this error is a [`FetchPolicy`] refusal rather than a failure.
///
/// The distinction matters to callers that would otherwise retry or fall back:
/// a refusal is a decision about an address, so trying a second route to the
/// same address only probes it again.
pub fn is_policy_refusal(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<io::Error>()
            .is_some_and(|io| io.kind() == io::ErrorKind::PermissionDenied)
    })
}

/// Whether this error is a host that stalled: it took longer than the
/// timeouts below allow, or sent its body too slowly.
///
/// Callers that skip a failed read and go on to the next one must stop
/// instead: the next read goes to the same host, and waits as long.
pub fn is_stall(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<io::Error>()
            .is_some_and(|io| io.kind() == io::ErrorKind::TimedOut)
    })
}

/// Whether a ureq error is one of its timeouts. A body read reports one as an
/// `io::Error` wrapping `ureq::Error::Timeout`, and [`MinRate`] as `TimedOut`.
fn is_timeout(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Timeout(_) => true,
        ureq::Error::Io(io) => is_timeout_io(io),
        _ => false,
    }
}

fn is_timeout_io(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::TimedOut
        || e.get_ref()
            .and_then(|inner| inner.downcast_ref::<ureq::Error>())
            .is_some_and(is_timeout)
}

/// A stall as the `TimedOut` error [`is_stall`] looks for, saying which host.
fn stalled(url: &str, why: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("giving up on {url}, which stopped responding: {why}"),
    )
}

/// The message from a [`FetchPolicy`] refusal, if that is what this error is.
///
/// A refusal is a decision, not a failure: retrying it changes nothing and the
/// backoff only delays telling the operator what happened.
fn policy_refusal(e: &ureq::Error) -> Option<io::Error> {
    match e {
        ureq::Error::Io(inner) if inner.kind() == io::ErrorKind::PermissionDenied => Some(
            // Rebuilt rather than cloned (io::Error is not Clone), keeping the
            // kind: callers further up test for PermissionDenied to tell a
            // refusal from a host that was merely unreachable.
            io::Error::new(io::ErrorKind::PermissionDenied, inner.to_string()),
        ),
        _ => None,
    }
}

/// A ureq resolver that refuses to hand back an internal address.
///
/// Checking in the resolver rather than before the request is what closes the
/// DNS rebinding window. A name that answers with a public address when indice
/// validates it and a private one when indice connects would defeat any
/// check-then-fetch arrangement, because the second lookup is the one that
/// decides where the socket goes. Here there is only one lookup, and the
/// addresses it returns are the addresses ureq connects to.
///
/// One caveat worth recording: ureq documents the resolver module as not
/// following semver yet, so a patch bump can change this trait. That surfaces
/// as a build failure rather than a silently disabled guard, which is the
/// acceptable direction for it to break.
#[derive(Debug, Default)]
struct PublicOnlyResolver {
    inner: ureq::unversioned::resolver::DefaultResolver,
}

impl ureq::unversioned::resolver::Resolver for PublicOnlyResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> std::result::Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let addrs = self.inner.resolve(uri, config, timeout)?;
        // Refuse the whole name if *any* answer is internal, rather than
        // filtering to the public ones. A hostname that resolves to both is not
        // a hostname with a stray record; it is the shape of someone trying
        // this on purpose, and connecting to the half that passed would be
        // doing them a favour.
        if let Some(bad) = addrs.iter().find(|a| is_internal_addr(a.ip())) {
            // PermissionDenied, not a generic IO error: `with_retry` has to be
            // able to tell a policy refusal from a connection that failed.
            // Retrying a decision five times wastes seconds and then reports
            // "gave up after 5 attempts", which tells the operator nothing
            // about why. Found by the test for this guard.
            return Err(ureq::Error::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to fetch {}: it resolves to {}, which is on this machine or this \
                 network. indice fetches curator-supplied locations only from public \
                 addresses when it runs behind an auth proxy.",
                    uri.host().unwrap_or("that host"),
                    bad.ip(),
                ),
            )));
        }
        Ok(addrs)
    }
}

/// Read-ahead window: small reads (ZIP headers, CDX lines) are amortized into
/// one request; a read larger than this fetches exactly what's asked.
const CHUNK: u64 = 256 * 1024;

/// The end of a ZIP holds the EOCD and the central directory, which the reader
/// touches repeatedly (once per entry) while also reading local headers scattered
/// across the file. Cache this many bytes of the tail once so those end-of-file
/// reads never re-fetch and don't get evicted by the rolling buffer.
const TAIL_CACHE: u64 = 1024 * 1024;

/// Fetches byte ranges of a resource. Abstracted from the buffering/seek logic
/// so that logic can be unit-tested against an in-memory source.
pub trait RangeFetch {
    fn total_len(&self) -> u64;
    /// Fetch bytes `[start, end)` (end exclusive); `end <= total_len`.
    fn fetch(&self, start: u64, end: u64) -> io::Result<Vec<u8>>;
    /// Whether each fetch crosses a network, so that reading the same bytes
    /// twice costs a second download rather than a second disk read. Indexing
    /// copies what it must read twice to a local file when this is true.
    fn is_remote(&self) -> bool {
        false
    }
}

/// A `Read + Seek` over a [`RangeFetch`] with a single read-ahead buffer.
pub struct RangeReader<F: RangeFetch> {
    fetch: F,
    pos: u64,
    // Rolling forward buffer for reads in the body of the file.
    buf: Vec<u8>,
    buf_start: u64,
    // Cached tail (end-of-file region: EOCD + central directory), fetched once.
    tail: Option<Vec<u8>>,
    tail_start: u64,
}

impl<F: RangeFetch> RangeReader<F> {
    pub fn new(fetch: F) -> Self {
        let tail_start = fetch.total_len().saturating_sub(TAIL_CACHE);
        Self {
            fetch,
            pos: 0,
            buf: Vec::new(),
            buf_start: 0,
            tail: None,
            tail_start,
        }
    }

    pub fn total_len(&self) -> u64 {
        self.fetch.total_len()
    }

    /// Offset into `buf` for `pos`, if `pos` is currently buffered.
    fn buffered_offset(&self, pos: u64) -> Option<usize> {
        let end = self.buf_start + self.buf.len() as u64;
        (pos >= self.buf_start && pos < end).then(|| (pos - self.buf_start) as usize)
    }
}

impl<F: RangeFetch> Read for RangeReader<F> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let total = self.fetch.total_len();
        if self.pos >= total || out.is_empty() {
            return Ok(0);
        }

        // Reads in the tail region (EOCD + central directory) come from a cache
        // fetched once, so they don't thrash against scattered body reads.
        if self.pos >= self.tail_start {
            if self.tail.is_none() {
                self.tail = Some(self.fetch.fetch(self.tail_start, total)?);
            }
            let tail = self.tail.as_ref().unwrap();
            let off = (self.pos - self.tail_start) as usize;
            let n = (tail.len() - off).min(out.len());
            out[..n].copy_from_slice(&tail[off..off + n]);
            self.pos += n as u64;
            return Ok(n);
        }

        let off = match self.buffered_offset(self.pos) {
            Some(o) => o,
            None => {
                // Fetch a window at pos: at least the requested size, otherwise a
                // read-ahead chunk, capped at the tail (or file end).
                let want = (out.len() as u64).max(CHUNK).min(total - self.pos);
                let end = (self.pos + want).min(total);
                self.buf = self.fetch.fetch(self.pos, end)?;
                self.buf_start = self.pos;
                0
            }
        };
        let avail = &self.buf[off..];
        let n = avail.len().min(out.len());
        out[..n].copy_from_slice(&avail[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<F: RangeFetch> Seek for RangeReader<F> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let (base, delta) = match from {
            SeekFrom::Start(p) => (0i64, p as i64),
            SeekFrom::End(p) => (self.fetch.total_len() as i64, p),
            SeekFrom::Current(p) => (self.pos as i64, p),
        };
        let np = base + delta;
        if np < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        self.pos = np as u64;
        Ok(self.pos)
    }
}

// ── Transient-failure retry / politeness ────────────────────────────────────
//
// Remote fetches (range GETs and downloads) retry transient failures — network
// errors and HTTP 429/502/503/504 — with capped exponential backoff + jitter,
// honoring a server `Retry-After`. This makes long ingests survive blips, and is
// *polite*: when a host pushes back we wait instead of hammering it, so we're far
// less likely to overload a small server or get IP-blocked — which matters
// because a single WACZ's concurrent record fetches all hit one host.

const MAX_ATTEMPTS: u32 = 5;
const BASE_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Outcome of one HTTP attempt.
enum Attempt<T> {
    Done(T),
    /// Transient failure; optional server-suggested delay (`Retry-After`).
    Retry(Option<Duration>),
    Fatal(io::Error),
}

/// Run `attempt`, retrying transient failures with capped exponential backoff +
/// jitter (or a server-suggested `Retry-After`), up to [`MAX_ATTEMPTS`].
fn with_retry<T>(label: &str, mut attempt: impl FnMut() -> Attempt<T>) -> io::Result<T> {
    let mut backoff = BASE_BACKOFF;
    for n in 1..=MAX_ATTEMPTS {
        match attempt() {
            Attempt::Done(v) => return Ok(v),
            Attempt::Fatal(e) => return Err(e),
            Attempt::Retry(after) => {
                if n == MAX_ATTEMPTS {
                    return Err(io::Error::other(format!(
                        "{label}: gave up after {MAX_ATTEMPTS} attempts"
                    )));
                }
                let base = after.unwrap_or(backoff).min(MAX_BACKOFF);
                let wait = base + jitter(base);
                tracing::debug!("{label}: transient failure, retry {n}/{MAX_ATTEMPTS} in {wait:?}");
                std::thread::sleep(wait);
                backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
            }
        }
    }
    unreachable!("the last iteration returns")
}

/// Up to ~20% of `base`, to spread concurrent workers' retries so they don't all
/// re-hit a rate-limited host in lockstep (thundering herd).
fn jitter(base: Duration) -> Duration {
    let span = base.as_millis() as u64 / 5;
    if span == 0 {
        return Duration::ZERO;
    }
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(n % (span + 1))
}

/// HTTP statuses worth retrying: rate-limit and transient server errors.
fn is_transient_status(code: u16) -> bool {
    matches!(code, 429 | 502 | 503 | 504)
}

/// Parse `Retry-After` as delta-seconds (the HTTP-date form is ignored, falling
/// back to computed backoff). Generic over the body type to avoid naming it.
fn retry_after<B>(resp: &ureq::http::Response<B>) -> Option<Duration> {
    resp.headers()
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

// ── How long a remote host may take ─────────────────────────────────────────
//
// The host is a curator's choice, or whoever controls it now, and a fetch holds
// the index lock for the whole ingest: while a host stalls, no other crawl is
// added and nothing is deleted. ureq has no timeouts by default, so without
// these a host that accepts the connection and never answers holds the lock
// until the process is killed, and a SIGTERM waits on the job as well.
//
// A timeout is not retried: a host that stalled once is given up on for the
// whole ingest (see `HttpFetch::stalled`). Retried, and with the indexer
// skipping a record it cannot read, a host that stalls on every record costs
// five timeouts per record, for every record in the crawl.

/// How long to wait for the host's name to resolve. The name and its
/// nameserver are as much the curator's choice as the host.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait, once the request is sent, for the response's headers.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a single read may wait for any bytes at all.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The least a body must deliver in each [`RATE_WINDOW`], so a host that
/// sends a byte just inside [`READ_TIMEOUT`], forever, is stopped too. About
/// 1 KiB/s: far below any working host, and a gigabyte download still has no
/// deadline.
const MIN_BYTES_PER_WINDOW: u64 = 64 * 1024;
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// The waits above as one value, so a test can shorten them.
#[derive(Clone, Copy)]
struct Timeouts {
    resolve: Duration,
    connect: Duration,
    response: Duration,
    read: Duration,
}

const TIMEOUTS: Timeouts = Timeouts {
    resolve: RESOLVE_TIMEOUT,
    connect: CONNECT_TIMEOUT,
    response: RESPONSE_TIMEOUT,
    read: READ_TIMEOUT,
};

/// A body reader that fails once a [`RATE_WINDOW`] passes with less than
/// [`MIN_BYTES_PER_WINDOW`] read in it. A read that blocks is bounded by
/// [`READ_TIMEOUT`], so this only has to judge the reads that return.
///
/// The clock runs between reads as well as during them, so whoever holds one
/// must read it through without stopping, as every caller does by copying it
/// straight to memory or disk. A caller that paused for a minute between
/// reads would be told the host was too slow.
struct MinRate<R> {
    inner: R,
    window: Duration,
    min_bytes: u64,
    window_start: std::time::Instant,
    window_bytes: u64,
}

impl<R> MinRate<R> {
    fn new(inner: R) -> Self {
        Self::with_limits(inner, RATE_WINDOW, MIN_BYTES_PER_WINDOW)
    }

    fn with_limits(inner: R, window: Duration, min_bytes: u64) -> Self {
        Self {
            inner,
            window,
            min_bytes,
            window_start: std::time::Instant::now(),
            window_bytes: 0,
        }
    }
}

impl<R: Read> Read for MinRate<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.window_bytes += n as u64;
        // At the end of the body there is nothing left to be slow about.
        if n > 0 && self.window_start.elapsed() >= self.window {
            if self.window_bytes < self.min_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "the host sent {} bytes in {}s, too slowly to go on reading",
                        self.window_bytes,
                        self.window_start.elapsed().as_secs()
                    ),
                ));
            }
            self.window_start = std::time::Instant::now();
            self.window_bytes = 0;
        }
        Ok(n)
    }
}

/// The shared HTTP agent. `http_status_as_error(false)` returns 4xx/5xx as a
/// normal response we can inspect (status + `Retry-After`), rather than an opaque
/// error — needed to classify 429/503 for retry.
///
/// Under [`FetchPolicy::PublicOnly`] the agent is built with a resolver that
/// refuses internal addresses; see [`PublicOnlyResolver`]. Both policies get
/// the timeouts above: an operator's own host can stall as well.
fn http_agent(policy: FetchPolicy) -> ureq::Agent {
    http_agent_with(policy, TIMEOUTS)
}

fn http_agent_with(policy: FetchPolicy, t: Timeouts) -> ureq::Agent {
    let builder = || {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_resolve(Some(t.resolve))
            .timeout_connect(Some(t.connect))
            .timeout_recv_response(Some(t.response))
            .timeout_per_read(Some(t.read))
    };
    match policy {
        FetchPolicy::Unrestricted => builder().build().new_agent(),
        FetchPolicy::PublicOnly => {
            // An HTTP proxy turns this guard off, so refuse to use one.
            //
            // ureq's default config picks a proxy up from ALL_PROXY/HTTPS_PROXY/
            // HTTP_PROXY. When one is set, `run.rs` hands the connector
            // `resolver.empty()` instead of calling the resolver, because the
            // proxy resolves the target itself (`default_resolve_target()` is
            // false for http and https proxies). The address we care about is
            // then never looked at here, and the only one our resolver does see
            // is the *proxy's* — so on a host with HTTP_PROXY set the check
            // silently passes everything, and on one with a loopback proxy
            // sidecar it refuses everything. Both found in review.
            //
            // Clearing it means a site that reaches the internet only through
            // an egress proxy cannot add archives by URL. That is the right way
            // round: the alternative is a guard that quietly does nothing on an
            // institutional network, which is the deployment it exists for.
            // `--allow-internal-fetch` is the way out, and it says plainly that
            // the operator is taking responsibility for the address.
            let config = builder().proxy(None).build();
            // Once, not once per ranged read.
            if !PROXY_IGNORED.swap(true, std::sync::atomic::Ordering::Relaxed)
                && ureq::Proxy::try_from_env().is_some()
            {
                tracing::warn!(
                    "ignoring the proxy from the environment when fetching a \
                     curator-supplied URL: a proxy resolves the address itself, which \
                     would bypass the check that it is public. Pass \
                     --allow-internal-fetch if this instance must fetch through a proxy."
                );
            }
            ureq::Agent::with_parts(
                config,
                ureq::unversioned::transport::DefaultConnector::new(),
                PublicOnlyResolver::default(),
            )
        }
    }
}

/// So the proxy warning above is logged once rather than per ranged read.
static PROXY_IGNORED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A [`RangeFetch`] backed by HTTP range GETs via `ureq`, with transient-failure
/// retry/backoff. `Clone` is cheap (the `ureq::Agent` is `Arc`-backed) and
/// `fetch` takes `&self`, so a single `HttpFetch` can drive many concurrent range
/// requests (see the parallel CDX-guided extractor).
#[derive(Clone)]
pub struct HttpFetch {
    agent: ureq::Agent,
    url: String,
    len: u64,
    /// Set by the first fetch that times out, and shared by every clone, so
    /// each later fetch fails at once instead of waiting on the host again.
    /// Without it a caller that skips the record it could not read, as the
    /// indexer does, would wait the timeout out once per record.
    stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl HttpFetch {
    /// Probe the resource for its total size and confirm the server honors range
    /// requests (a `206` with `Content-Range`). Retries transient failures;
    /// errors otherwise so the caller can fall back to downloading.
    pub fn open(url: &str, policy: FetchPolicy) -> Result<Self> {
        Self::open_with(url, policy, TIMEOUTS)
    }

    fn open_with(url: &str, policy: FetchPolicy, timeouts: Timeouts) -> Result<Self> {
        let agent = http_agent_with(policy, timeouts);
        let len = with_retry(&format!("HTTP range probe of {url}"), || {
            let resp = match agent.get(url).header("Range", "bytes=0-0").call() {
                Ok(r) => r,
                Err(e) if policy_refusal(&e).is_some() => {
                    return Attempt::Fatal(policy_refusal(&e).unwrap())
                }
                Err(e) if is_timeout(&e) => return Attempt::Fatal(stalled(url, e)),
                Err(_) => return Attempt::Retry(None),
            };
            let code = resp.status().as_u16();
            if is_transient_status(code) {
                return Attempt::Retry(retry_after(&resp));
            }
            if code != 206 {
                return Attempt::Fatal(io::Error::other(format!(
                    "{url} did not honor a range request (HTTP {code}); the server must \
                     support HTTP range requests to stream-index it — use --download instead"
                )));
            }
            // Content-Range: "bytes 0-0/<total>"
            match resp
                .headers()
                .get("Content-Range")
                .and_then(|cr| cr.to_str().ok())
                .and_then(|cr| cr.rsplit('/').next())
                .map(str::trim)
                .and_then(|n| n.parse::<u64>().ok())
            {
                Some(len) => Attempt::Done(len),
                None => Attempt::Fatal(io::Error::other(format!(
                    "no total length in Content-Range from {url}"
                ))),
            }
        })?;
        Ok(Self {
            agent,
            url: url.to_string(),
            len,
            stalled: Default::default(),
        })
    }
}

impl RangeFetch for HttpFetch {
    fn total_len(&self) -> u64 {
        self.len
    }

    fn is_remote(&self) -> bool {
        true
    }

    fn fetch(&self, start: u64, end: u64) -> io::Result<Vec<u8>> {
        use std::sync::atomic::Ordering;
        if self.stalled.load(Ordering::Relaxed) {
            return Err(stalled(&self.url, "an earlier request timed out"));
        }
        let give_up = |e: &dyn std::fmt::Display| {
            self.stalled.store(true, Ordering::Relaxed);
            Attempt::Fatal(stalled(&self.url, e))
        };
        let range = format!("bytes={}-{}", start, end - 1);
        with_retry(&format!("range GET of {}", self.url), || {
            let resp = match self.agent.get(&self.url).header("Range", &range).call() {
                Ok(r) => r,
                Err(e) if policy_refusal(&e).is_some() => {
                    return Attempt::Fatal(policy_refusal(&e).unwrap())
                }
                Err(e) if is_timeout(&e) => return give_up(&e),
                Err(_) => return Attempt::Retry(None),
            };
            let code = resp.status().as_u16();
            if is_transient_status(code) {
                return Attempt::Retry(retry_after(&resp));
            }
            // Require 206: `open` confirmed range support and every fetch asks for
            // a sub-file slice, so a 200 means the server ignored the Range and is
            // sending the *whole file* — reject it rather than read a multi-GB body
            // into memory for one record.
            if code != 206 {
                return Attempt::Fatal(io::Error::other(format!(
                    "range GET of {} returned HTTP {code} (expected 206)",
                    self.url
                )));
            }
            // At most what was asked for, plus one byte to notice more: a host
            // that answers a small range with an endless body would otherwise
            // be read into memory until it stopped.
            let want = end - start;
            let mut v = Vec::with_capacity(want as usize);
            match MinRate::new(resp.into_body().into_reader())
                .take(want + 1)
                .read_to_end(&mut v)
            {
                Ok(_) if v.len() as u64 > want => Attempt::Fatal(io::Error::other(format!(
                    "range GET of {} returned more than the {want} bytes asked for",
                    self.url
                ))),
                Ok(_) => Attempt::Done(v),
                Err(e) if is_timeout_io(&e) => give_up(&e),
                // Any other mid-stream read failure is usually transient; retry
                // the range. The policy refusal cannot reach here: it happens
                // during name resolution, so the connection never opened.
                Err(_) => Attempt::Retry(None),
            }
        })
    }
}

/// A [`RangeFetch`] over a local file — the file counterpart of [`HttpFetch`], so
/// the CDX-guided extractor can read record byte-ranges the same way whether the
/// WACZ is local or remote. Each `fetch` opens the file and reads the slice, so it
/// needs no `&mut` and is safe to call concurrently from many threads.
#[derive(Clone)]
pub struct FileFetch {
    path: std::path::PathBuf,
    len: u64,
}

impl FileFetch {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let len = std::fs::metadata(path)
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        Ok(Self {
            path: path.to_path_buf(),
            len,
        })
    }
}

impl RangeFetch for FileFetch {
    fn total_len(&self) -> u64 {
        self.len
    }

    fn fetch(&self, start: u64, end: u64) -> io::Result<Vec<u8>> {
        let mut f = std::fs::File::open(&self.path)?;
        f.seek(SeekFrom::Start(start))?;
        let mut v = vec![0u8; (end - start) as usize];
        f.read_exact(&mut v)?;
        Ok(v)
    }
}

/// A [`RangeFetch`] over a byte window `[base, base + len)` of another fetch.
///
/// Used to read a **Stored** (uncompressed) inner WACZ of a nested multi-WACZ in
/// place: its bytes are a contiguous slice of the outer WACZ, so a window over
/// the outer's fetch *is* the inner WACZ — the streaming indexer can read it
/// (CDX + only the page records) without extracting it to a temp file, and a
/// remote outer still fetches only the ranges it needs. Cheap to clone when `F`
/// is (both `HttpFetch` and `FileFetch` are).
#[derive(Clone)]
pub struct SubRangeFetch<F> {
    inner: F,
    base: u64,
    len: u64,
}

impl<F: RangeFetch> SubRangeFetch<F> {
    pub fn new(inner: F, base: u64, len: u64) -> Self {
        Self { inner, base, len }
    }
}

impl<F: RangeFetch> RangeFetch for SubRangeFetch<F> {
    fn total_len(&self) -> u64 {
        self.len
    }
    fn fetch(&self, start: u64, end: u64) -> io::Result<Vec<u8>> {
        // Translate window-relative offsets to the outer fetch, clamped to the
        // window so a read can't stray past the inner WACZ.
        let end = end.min(self.len);
        let start = start.min(end);
        self.inner.fetch(self.base + start, self.base + end)
    }
    fn is_remote(&self) -> bool {
        self.inner.is_remote()
    }
}

/// A retried whole-file GET returning the response body reader (for downloads).
/// Retries the request start on transient failures; a mid-stream connection drop
/// is not resumed.
pub fn get_reader(url: &str, policy: FetchPolicy) -> Result<impl Read> {
    let agent = http_agent(policy);
    let resp = with_retry(&format!("HTTP GET {url}"), || {
        let resp = match agent.get(url).call() {
            Ok(r) => r,
            Err(e) if policy_refusal(&e).is_some() => {
                return Attempt::Fatal(policy_refusal(&e).unwrap())
            }
            Err(e) if is_timeout(&e) => return Attempt::Fatal(stalled(url, e)),
            Err(_) => return Attempt::Retry(None),
        };
        let code = resp.status().as_u16();
        if is_transient_status(code) {
            Attempt::Retry(retry_after(&resp))
        } else if (200..300).contains(&code) {
            Attempt::Done(resp)
        } else {
            Attempt::Fatal(io::Error::other(format!("HTTP {code} for {url}")))
        }
    })?;
    Ok(MinRate::new(resp.into_body().into_reader()))
}

/// Open a remote WACZ as a `Read + Seek` HTTP range stream.
pub fn open_remote(url: &str, policy: FetchPolicy) -> Result<RangeReader<HttpFetch>> {
    Ok(RangeReader::new(HttpFetch::open(url, policy)?))
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    /// Both kinds of agent give up on a host that stalls: before connecting,
    /// before its headers, and between reads. Without these a stalled host holds
    /// the index lock for as long as it likes.
    #[test]
    fn every_agent_has_timeouts() {
        for policy in [FetchPolicy::Unrestricted, FetchPolicy::PublicOnly] {
            let agent = http_agent(policy);
            let t = agent.config().timeouts();
            assert_eq!(t.resolve, Some(RESOLVE_TIMEOUT), "{policy:?}");
            assert_eq!(t.connect, Some(CONNECT_TIMEOUT), "{policy:?}");
            assert_eq!(t.recv_response, Some(RESPONSE_TIMEOUT), "{policy:?}");
            assert_eq!(
                agent.config().timeout_per_read(),
                Some(READ_TIMEOUT),
                "{policy:?}"
            );
        }
    }

    /// A host that answers the range probe and then accepts every connection
    /// and never answers it, counting the connections.
    fn stalling_host() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/a.wacz", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = seen.clone();
        // Left running: it blocks on accept until the test binary exits.
        std::thread::spawn(move || {
            let mut silent = Vec::new();
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let n = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n > 0 {
                    silent.push(stream);
                    continue;
                }
                for line in std::io::BufReader::new(&stream).lines() {
                    if line.unwrap().is_empty() {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/100\r\n\
                     Content-Length: 1\r\nConnection: close\r\n\r\nx"
                )
                .unwrap();
            }
        });
        (url, seen)
    }

    /// A host that stops answering is given up on at the first timeout, not
    /// retried, and every later fetch fails without going back to it. The
    /// indexer skips a record it cannot read, so a host retried per record
    /// would hold the index for as long as the crawl has records.
    #[test]
    fn a_host_that_stalls_is_given_up_on_once() {
        let (url, seen) = stalling_host();
        let short = Duration::from_millis(300);
        let timeouts = Timeouts {
            resolve: short,
            connect: short,
            response: short,
            read: short,
        };
        let fetch = HttpFetch::open_with(&url, FetchPolicy::Unrestricted, timeouts).unwrap();
        let started = std::time::Instant::now();
        let err = fetch.fetch(0, 10).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert!(err.to_string().contains("stopped responding"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "retried: {:?}",
            started.elapsed()
        );
        let again = fetch.clone().fetch(10, 20).unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::TimedOut, "{again}");
        assert!(is_stall(&anyhow::Error::from(again)));
        // The probe, then the one range that stalled; nothing after.
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// A reader that hands back one byte per call, after a pause.
    struct Trickle(Duration);
    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(self.0);
            buf[0] = b'x';
            Ok(1)
        }
    }

    #[test]
    fn a_body_that_trickles_is_stopped() {
        let mut r = MinRate::with_limits(
            Trickle(Duration::from_millis(10)),
            Duration::from_millis(50),
            100,
        );
        let err = io::copy(&mut r, &mut io::sink()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
    }

    #[test]
    fn a_body_that_keeps_up_is_read_to_the_end() {
        let body = vec![0u8; 1 << 20];
        let mut r = MinRate::with_limits(&body[..], Duration::ZERO, 1);
        assert_eq!(
            io::copy(&mut r, &mut io::sink()).unwrap(),
            body.len() as u64
        );
    }
}

#[cfg(test)]
mod internal_addr_tests {
    use super::is_internal_addr;
    use std::net::IpAddr;

    fn internal(s: &str) -> bool {
        is_internal_addr(s.parse::<IpAddr>().unwrap())
    }

    #[test]
    fn refuses_this_machine_and_this_network() {
        for addr in [
            "127.0.0.1",
            "127.1.2.3",
            "0.0.0.0",
            "10.0.0.5",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.1",
            // The cloud metadata endpoint, which is the whole reason the
            // link-local range is on this list.
            "169.254.169.254",
            "255.255.255.255",
            "100.64.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
        ] {
            assert!(internal(addr), "{addr} should be refused");
        }
    }

    #[test]
    fn allows_the_public_internet() {
        for addr in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            // Just outside the private ranges, to check the boundaries rather
            // than the middles.
            "11.0.0.1",
            "172.15.255.255",
            "172.32.0.0",
            "192.167.255.255",
            "192.169.0.0",
            "100.63.255.255",
            "100.128.0.0",
            "2606:4700::1111",
            "2001:4860:4860::8888",
        ] {
            assert!(!internal(addr), "{addr} should be allowed");
        }
    }

    #[test]
    fn an_ipv4_address_cannot_hide_inside_ipv6() {
        // The bypass worth having a test for. `::ffff:127.0.0.1` is loopback
        // written the long way, and `Ipv6Addr::is_loopback` says false for it,
        // so a check that forgot to unmap would wave through exactly what it
        // was written to stop.
        for addr in [
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "::ffff:192.168.0.1",
            // The deprecated IPv4-compatible spelling. `to_ipv4_mapped` returns
            // None for these, so without their own unwrapping they match
            // nothing below and read as public.
            "::127.0.0.1",
            "::169.254.169.254",
            // NAT64: on such a network this reaches 169.254.169.254.
            "64:ff9b::a9fe:a9fe",
        ] {
            assert!(internal(addr), "{addr} is an internal v4 address in a hat");
        }
        assert!(
            !internal("::ffff:8.8.8.8"),
            "a mapped public address is fine"
        );
        assert!(
            !internal("64:ff9b::808:808"),
            "NAT64 of a public address is still public"
        );
    }

    #[test]
    fn refuses_the_ranges_a_denylist_forgets() {
        // Every one of these was missed by the first version of this function
        // and found in review. They are why the doc comment promises
        // conservatism: each reaches this machine, this link, or nothing at
        // all, and none is "clearly out on the internet".
        for addr in [
            "224.0.0.1",       // all hosts on this link
            "239.255.255.250", // SSDP
            "ff01::1",         // interface-local all nodes
            "ff02::1",         // link-local all nodes
            "240.0.0.1",       // reserved /4
            "198.18.0.1",      // benchmarking /15
            "192.0.0.1",       // IETF protocol assignments
            "2001:db8::1",     // documentation
            "100::1",          // discard-only
        ] {
            assert!(internal(addr), "{addr} should be refused");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory RangeFetch for testing the buffering/seek logic.
    struct MemFetch(Vec<u8>);
    impl RangeFetch for MemFetch {
        fn total_len(&self) -> u64 {
            self.0.len() as u64
        }
        fn fetch(&self, start: u64, end: u64) -> io::Result<Vec<u8>> {
            Ok(self.0[start as usize..end as usize].to_vec())
        }
    }

    /// Read exactly up to `n` bytes (looping over partial reads).
    fn read_n<R: Read>(r: &mut R, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut tmp = vec![0u8; n];
        while out.len() < n {
            let got = r.read(&mut tmp[..n - out.len()]).unwrap();
            if got == 0 {
                break;
            }
            out.extend_from_slice(&tmp[..got]);
        }
        out
    }

    #[test]
    fn range_reader_reads_and_seeks_like_a_cursor() {
        // 5000 bytes crosses the (much larger) chunk boundary logic; use a size
        // below CHUNK plus a case that reads more than CHUNK by shrinking CHUNK
        // conceptually via a large read.
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mut rr = RangeReader::new(MemFetch(data.clone()));
        let mut cur = std::io::Cursor::new(data.clone());

        for &(pos, n) in &[
            (0u64, 10usize),
            (4990, 50),
            (1234, 300),
            (0, 5000),
            (2500, 2500),
        ] {
            rr.seek(SeekFrom::Start(pos)).unwrap();
            cur.seek(SeekFrom::Start(pos)).unwrap();
            assert_eq!(
                read_n(&mut rr, n),
                read_n(&mut cur, n),
                "at pos {pos} len {n}"
            );
        }

        // SeekFrom::End and Current.
        assert_eq!(rr.seek(SeekFrom::End(-5)).unwrap(), 4995);
        assert_eq!(read_n(&mut rr, 100), data[4995..].to_vec());
        rr.seek(SeekFrom::Start(100)).unwrap();
        rr.seek(SeekFrom::Current(50)).unwrap();
        assert_eq!(read_n(&mut rr, 10), data[150..160].to_vec());
    }

    #[test]
    fn range_reader_large_read_exceeding_chunk() {
        // A read larger than CHUNK must still work (fetches exactly what's asked).
        let n = (CHUNK * 2 + 123) as usize;
        let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
        let mut rr = RangeReader::new(MemFetch(data.clone()));
        assert_eq!(read_n(&mut rr, n), data);
    }

    #[test]
    fn range_reader_tail_and_body_regions_match_cursor() {
        // Larger than the tail cache, so both the tail and the rolling body
        // buffer are exercised.
        let n = (TAIL_CACHE + 300_000) as usize;
        let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
        let mut rr = RangeReader::new(MemFetch(data.clone()));
        let mut cur = std::io::Cursor::new(data.clone());
        for &(pos, len) in &[
            (0u64, 1000usize),
            (500_000, 4096),
            (n as u64 - 50, 50),           // tail
            (n as u64 - 300_000, 200_000), // spans body into tail
        ] {
            rr.seek(SeekFrom::Start(pos)).unwrap();
            cur.seek(SeekFrom::Start(pos)).unwrap();
            assert_eq!(
                read_n(&mut rr, len),
                read_n(&mut cur, len),
                "pos {pos} len {len}"
            );
        }
    }

    /// Counts fetches so we can assert the tail cache prevents re-fetching.
    struct CountingFetch {
        data: Vec<u8>,
        count: std::rc::Rc<std::cell::Cell<usize>>,
    }
    impl RangeFetch for CountingFetch {
        fn total_len(&self) -> u64 {
            self.data.len() as u64
        }
        fn fetch(&self, start: u64, end: u64) -> io::Result<Vec<u8>> {
            self.count.set(self.count.get() + 1);
            Ok(self.data[start as usize..end as usize].to_vec())
        }
    }

    #[test]
    fn tail_cache_avoids_thrashing_between_end_and_body() {
        // Reproduces the ZIP pattern: read the central directory (end) and a
        // local header (body), alternating. Each region must be fetched once.
        let n = (TAIL_CACHE + 4_000_000) as usize;
        let count = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let mut rr = RangeReader::new(CountingFetch {
            data: vec![9u8; n],
            count: count.clone(),
        });
        for _ in 0..20 {
            rr.seek(SeekFrom::End(-100)).unwrap();
            read_n(&mut rr, 40); // "central directory" read (tail)
            rr.seek(SeekFrom::Start(2_000_000)).unwrap();
            read_n(&mut rr, 30); // "local header" read (body)
        }
        assert!(
            count.get() <= 3,
            "tail cache should make ~2 fetches, not 40; got {}",
            count.get()
        );
    }

    // Zero-delay retries keep these instant (no real sleeps).
    #[test]
    fn with_retry_succeeds_after_transient_failures() {
        let calls = std::cell::Cell::new(0u32);
        let out: io::Result<u32> = with_retry("t", || {
            let n = calls.get() + 1;
            calls.set(n);
            if n < 3 {
                Attempt::Retry(Some(Duration::ZERO))
            } else {
                Attempt::Done(n)
            }
        });
        assert_eq!(out.unwrap(), 3);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn with_retry_gives_up_after_max_attempts() {
        let calls = std::cell::Cell::new(0u32);
        let out: io::Result<u32> = with_retry("t", || {
            calls.set(calls.get() + 1);
            Attempt::Retry(Some(Duration::ZERO))
        });
        assert!(out.is_err());
        assert_eq!(calls.get(), MAX_ATTEMPTS);
    }

    #[test]
    fn with_retry_returns_fatal_without_retrying() {
        let calls = std::cell::Cell::new(0u32);
        let out: io::Result<u32> = with_retry("t", || {
            calls.set(calls.get() + 1);
            Attempt::Fatal(io::Error::other("nope"))
        });
        assert!(out.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn transient_statuses_are_rate_limit_and_5xx() {
        for c in [429, 502, 503, 504] {
            assert!(is_transient_status(c), "{c} should be transient");
        }
        for c in [200, 206, 301, 404, 416, 500, 501] {
            assert!(!is_transient_status(c), "{c} should not be transient");
        }
    }
}

#[cfg(test)]
mod body_cap_tests {
    use super::*;

    /// A host that honours the range probe and then answers every range with
    /// fifty bytes, whatever was asked for.
    fn overlong_host() -> (String, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/a.wacz", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut stream = stream.unwrap();
                let mut probe = false;
                for line in std::io::BufReader::new(&stream).lines() {
                    let line = line.unwrap();
                    if line.eq_ignore_ascii_case("range: bytes=0-0") {
                        probe = true;
                    }
                    if line.is_empty() {
                        break;
                    }
                }
                let body: &[u8] = if probe { b"x" } else { &[b'y'; 50] };
                let range = if probe {
                    "bytes 0-0/100"
                } else {
                    "bytes 0-9/100"
                };
                write!(
                    stream,
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: {range}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });
        (url, handle)
    }

    /// A range comes back no longer than it was asked to be: a host that
    /// sends more is refused rather than read until it stops.
    #[test]
    fn a_range_longer_than_asked_for_is_refused() {
        let (url, host) = overlong_host();
        let fetch = HttpFetch::open(&url, FetchPolicy::Unrestricted).unwrap();
        let err = fetch.fetch(0, 10).unwrap_err();
        assert!(err.to_string().contains("more than the 10 bytes"), "{err}");
        host.join().unwrap();
    }
}
