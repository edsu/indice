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
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // 100.64.0.0/10, carrier-grade NAT. Not private by the RFC1918
                // definition, not routable on the public internet either.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                // 0.0.0.0/8, "this network".
                || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            // An IPv4 address wearing an IPv6 hat. Unmap and judge it as what
            // it is, or ::ffff:169.254.169.254 walks straight past everything
            // below.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_internal_addr(IpAddr::V4(mapped));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                // fe80::/10, link-local unicast. `Ipv6Addr::is_unicast_link_local`
                // is still unstable, so spell it out.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // fc00::/7, unique local. `is_unique_local` is unstable too.
                || (v6.octets()[0] & 0xfe) == 0xfc
        }
    }
}

/// The message from a [`FetchPolicy`] refusal, if that is what this error is.
///
/// A refusal is a decision, not a failure: retrying it changes nothing and the
/// backoff only delays telling the operator what happened.
fn policy_refusal(e: &ureq::Error) -> Option<String> {
    match e {
        ureq::Error::Io(io) if io.kind() == io::ErrorKind::PermissionDenied => Some(io.to_string()),
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

/// The shared HTTP agent. `http_status_as_error(false)` returns 4xx/5xx as a
/// normal response we can inspect (status + `Retry-After`), rather than an opaque
/// error — needed to classify 429/503 for retry.
///
/// Under [`FetchPolicy::PublicOnly`] the agent is built with a resolver that
/// refuses internal addresses; see [`PublicOnlyResolver`].
fn http_agent(policy: FetchPolicy) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build();
    match policy {
        FetchPolicy::Unrestricted => config.new_agent(),
        FetchPolicy::PublicOnly => ureq::Agent::with_parts(
            config,
            ureq::unversioned::transport::DefaultConnector::new(),
            PublicOnlyResolver::default(),
        ),
    }
}

/// A [`RangeFetch`] backed by HTTP range GETs via `ureq`, with transient-failure
/// retry/backoff. `Clone` is cheap (the `ureq::Agent` is `Arc`-backed) and
/// `fetch` takes `&self`, so a single `HttpFetch` can drive many concurrent range
/// requests (see the parallel CDX-guided extractor).
#[derive(Clone)]
pub struct HttpFetch {
    agent: ureq::Agent,
    url: String,
    len: u64,
}

impl HttpFetch {
    /// Probe the resource for its total size and confirm the server honors range
    /// requests (a `206` with `Content-Range`). Retries transient failures;
    /// errors otherwise so the caller can fall back to downloading.
    pub fn open(url: &str, policy: FetchPolicy) -> Result<Self> {
        let agent = http_agent(policy);
        let len = with_retry(&format!("HTTP range probe of {url}"), || {
            let resp = match agent.get(url).header("Range", "bytes=0-0").call() {
                Ok(r) => r,
                Err(e) if policy_refusal(&e).is_some() => {
                    return Attempt::Fatal(io::Error::other(policy_refusal(&e).unwrap()))
                }
                Err(e) if policy_refusal(&e).is_some() => {
                    return Attempt::Fatal(io::Error::other(policy_refusal(&e).unwrap()))
                }
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
        })
    }
}

impl RangeFetch for HttpFetch {
    fn total_len(&self) -> u64 {
        self.len
    }

    fn fetch(&self, start: u64, end: u64) -> io::Result<Vec<u8>> {
        let range = format!("bytes={}-{}", start, end - 1);
        with_retry(&format!("range GET of {}", self.url), || {
            let resp = match self.agent.get(&self.url).header("Range", &range).call() {
                Ok(r) => r,
                Err(e) if policy_refusal(&e).is_some() => {
                    return Attempt::Fatal(io::Error::other(policy_refusal(&e).unwrap()))
                }
                Err(e) if policy_refusal(&e).is_some() => {
                    return Attempt::Fatal(io::Error::other(policy_refusal(&e).unwrap()))
                }
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
            let mut v = Vec::with_capacity((end - start) as usize);
            match resp.into_body().into_reader().read_to_end(&mut v) {
                Ok(_) => Attempt::Done(v),
                // A mid-stream read failure is usually transient; retry the
                // range. The policy refusal cannot reach here: it happens
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
                return Attempt::Fatal(io::Error::other(policy_refusal(&e).unwrap()))
            }
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
    Ok(resp.into_body().into_reader())
}

/// Open a remote WACZ as a `Read + Seek` HTTP range stream.
pub fn open_remote(url: &str, policy: FetchPolicy) -> Result<RangeReader<HttpFetch>> {
    Ok(RangeReader::new(HttpFetch::open(url, policy)?))
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
        ] {
            assert!(internal(addr), "{addr} is an internal v4 address in a hat");
        }
        assert!(
            !internal("::ffff:8.8.8.8"),
            "a mapped public address is fine"
        );
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
