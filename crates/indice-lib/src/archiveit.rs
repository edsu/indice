//! A small client for Archive-It, the Internet Archive's subscription
//! web-archiving service. Unlike Browsertrix, Archive-It does not serve WACZ
//! files — it serves **WARC files** (via WASAPI), with descriptive metadata via
//! its **Partner API**. The importer downloads a crawl's WARCs and builds a WACZ
//! from them (see [`crate::wacz_build`]); this module is just the API client.
//!
//! Two APIs, both over HTTP Basic auth on the same host, linked by collection id
//! and crawl (job) id:
//!   * **Partner API** (`/api/…`) — collections and their descriptive metadata.
//!   * **WASAPI** (`/wasapi/v1/webdata`) — the WARC file records (with download
//!     `locations`, checksums, size, and the `crawl` id + `crawl-time`).
//!
//! HTTP is abstracted behind the [`Transport`] trait (mirroring
//! [`crate::browsertrix::Transport`]) so auth / pagination / JSON parsing are
//! unit-tested against canned responses, with no live server dependency.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Deserialize;

/// Default Archive-It host. Both the Partner API and WASAPI live here.
pub const DEFAULT_HOST: &str = "https://partner.archive-it.org";

/// Safety valve for the WASAPI pagination loop: far above any real listing, but
/// bounds the loop if a server never signals the last page.
const MAX_PAGES: usize = 100_000;

// ── Transport ────────────────────────────────────────────────────────────────

/// The subset of HTTP the client performs, behind a trait so the client's
/// pagination / parsing logic can be tested against canned responses. `auth` is
/// the full `Authorization` header value (e.g. `Basic dXNlcjpwYXNz`), or `None`
/// for an unauthenticated request.
pub trait Transport {
    fn get(&self, url: &str, auth: Option<&str>) -> Result<(u16, Vec<u8>)>;
    fn get_stream(&self, url: &str, auth: Option<&str>) -> Result<Box<dyn Read + Send>>;
}

/// A [`Transport`] backed by `ureq`. 4xx/5xx come back as ordinary responses so
/// the client can read the API's status + message (matching `browsertrix.rs`).
#[derive(Clone)]
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .new_agent(),
        }
    }
}

impl Transport for UreqTransport {
    fn get(&self, url: &str, auth: Option<&str>) -> Result<(u16, Vec<u8>)> {
        let mut req = self.agent.get(url);
        if let Some(value) = auth {
            req = req.header("Authorization", value);
        }
        let resp = req.call().with_context(|| format!("GET {url}"))?;
        let status = resp.status().as_u16();
        let mut body = Vec::new();
        resp.into_body().into_reader().read_to_end(&mut body)?;
        Ok((status, body))
    }

    fn get_stream(&self, url: &str, auth: Option<&str>) -> Result<Box<dyn Read + Send>> {
        let mut req = self.agent.get(url);
        if let Some(value) = auth {
            req = req.header("Authorization", value);
        }
        let resp = req.call().with_context(|| format!("GET {url}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            bail!("GET {url} failed: HTTP {status}");
        }
        Ok(Box::new(resp.into_body().into_reader()))
    }
}

// ── API types ─────────────────────────────────────────────────────────────────

/// An Archive-It collection (Partner API `/api/collection`). `id`, `name`, and
/// `state` are the fields we rely on; all other attributes (including the
/// descriptive-metadata block, whose exact shape varies by account) are captured
/// in `extra` so the metadata mapper can read them defensively without this
/// struct hard-coding keys that must be confirmed against a live account.
#[derive(Debug, Clone, Deserialize)]
pub struct Collection {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A WASAPI WARC file record (`/wasapi/v1/webdata` → `files[]`).
#[derive(Debug, Clone, Deserialize)]
pub struct WarcFile {
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub checksums: Checksums,
    #[serde(default)]
    pub collection: Option<i64>,
    #[serde(default)]
    pub crawl: Option<i64>,
    #[serde(rename = "crawl-time", default)]
    pub crawl_time: Option<String>,
    /// Download URLs (any one works); account-scoped, so fetched with auth.
    #[serde(default)]
    pub locations: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Checksums {
    #[serde(default)]
    pub md5: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
}

/// The WASAPI `webdata` envelope: `count`, cursor `next` (an absolute URL or
/// null), and the page's `files`.
#[derive(Debug, Deserialize)]
struct WebdataPage {
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    files: Vec<WarcFile>,
}

/// Filters for a WASAPI `webdata` query. All optional; an empty query lists every
/// WARC the account can see.
#[derive(Debug, Default, Clone)]
pub struct WasapiQuery<'a> {
    pub collection: Option<i64>,
    pub crawl: Option<i64>,
    pub crawl_time_after: Option<&'a str>,
    pub crawl_time_before: Option<&'a str>,
}

// ── Provider ───────────────────────────────────────────────────────────────────

/// Supplies an authenticated [`Client`] for the management UI's Archive-It import
/// flow. Implemented by the **binary**, which holds the credentials and host
/// (env `ARCHIVEIT_*`), keeping auth/config out of the library — the same
/// boundary as [`crate::browsertrix::BrowsertrixProvider`].
pub trait ArchiveItProvider: Send + Sync {
    fn client(&self) -> Result<Client>;
}

// ── Client ───────────────────────────────────────────────────────────────────

/// An authenticated Archive-It API client (Partner API + WASAPI). Generic over
/// the [`Transport`] so tests can drive it with canned responses.
pub struct Client<T: Transport = UreqTransport> {
    transport: T,
    host: String,
    auth: String, // full `Authorization` header value (Basic …)
}

impl Client<UreqTransport> {
    /// A client authenticating to `host` with HTTP Basic auth.
    pub fn new(host: &str, username: &str, password: &str) -> Self {
        Self::with_transport(UreqTransport::default(), host, username, password)
    }
}

impl<T: Transport> Client<T> {
    /// Build a client on a caller-provided transport (tests inject a fake one).
    pub fn with_transport(transport: T, host: &str, username: &str, password: &str) -> Self {
        let creds =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        Self {
            transport,
            host: host.trim_end_matches('/').to_string(),
            auth: format!("Basic {creds}"),
        }
    }

    /// The host this client is bound to (no trailing slash).
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The account's collections. `active_only` filters to `state=ACTIVE`.
    pub fn collections(&self, active_only: bool) -> Result<Vec<Collection>> {
        // Partner API returns a JSON array; `limit=-1` disables the 100-row cap.
        let mut path = String::from("/api/collection?format=json&limit=-1");
        if active_only {
            path.push_str("&state=ACTIVE");
        }
        self.get_json(&path)
    }

    /// The WARC files matching a WASAPI query, following `next` pagination.
    pub fn webdata(&self, query: &WasapiQuery) -> Result<Vec<WarcFile>> {
        let mut files = Vec::new();
        let mut url = format!("{}/wasapi/v1/webdata{}", self.host, wasapi_query(query));
        for _ in 0..MAX_PAGES {
            let page: WebdataPage = self.get_json_url(&url)?;
            files.extend(page.files);
            match page.next {
                Some(next) if !next.is_empty() => url = next,
                _ => return Ok(files),
            }
        }
        bail!("WASAPI pagination did not terminate after {MAX_PAGES} pages");
    }

    /// Download `url` (a WASAPI `locations` URL) to `dest`, authenticated,
    /// streaming to a `.part` file then renaming so a partial download never
    /// looks complete. Returns bytes written.
    pub fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        let tmp = dest.with_extension("part");
        let mut reader = self
            .transport
            .get_stream(url, Some(&self.auth))
            .with_context(|| format!("downloading {url}"))?;
        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        let mut buf = [0u8; 65536];
        let mut written = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            written += n as u64;
        }
        file.flush()?;
        drop(file);
        std::fs::rename(&tmp, dest)
            .with_context(|| format!("renaming {} to {}", tmp.display(), dest.display()))?;
        Ok(written)
    }

    /// GET + parse a JSON endpoint (path begins with `/`), authenticated.
    fn get_json<D: DeserializeOwned>(&self, path: &str) -> Result<D> {
        self.get_json_url(&format!("{}{path}", self.host))
    }

    fn get_json_url<D: DeserializeOwned>(&self, url: &str) -> Result<D> {
        let (status, body) = self.transport.get(url, Some(&self.auth))?;
        if !(200..300).contains(&status) {
            bail!("GET {url} failed (HTTP {status}): {}", body_snippet(&body));
        }
        serde_json::from_slice(&body).with_context(|| format!("parsing response from {url}"))
    }
}

/// Build the `?a=b&…` query string for a WASAPI `webdata` request (empty when no
/// filters). Values are percent-encoded.
fn wasapi_query(q: &WasapiQuery) -> String {
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if let Some(c) = q.collection {
        pairs.push(("collection", c.to_string()));
    }
    if let Some(c) = q.crawl {
        pairs.push(("crawl", c.to_string()));
    }
    if let Some(t) = q.crawl_time_after {
        pairs.push(("crawl-time-after", t.to_string()));
    }
    if let Some(t) = q.crawl_time_before {
        pairs.push(("crawl-time-before", t.to_string()));
    }
    if pairs.is_empty() {
        return String::new();
    }
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in &pairs {
        ser.append_pair(k, v);
    }
    format!("?{}", ser.finish())
}

/// A short, printable, char-boundary-safe slice of a response body for errors.
fn body_snippet(body: &[u8]) -> String {
    const MAX: usize = 300;
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.chars().count() > MAX {
        format!("{}…", text.chars().take(MAX).collect::<String>())
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A [`Transport`] returning canned `(status, body)` for exact URLs.
    #[derive(Default)]
    struct FakeTransport {
        responses: HashMap<String, (u16, String)>,
    }
    impl FakeTransport {
        fn with(mut self, url: &str, status: u16, body: &str) -> Self {
            self.responses
                .insert(url.to_string(), (status, body.to_string()));
            self
        }
        fn lookup(&self, url: &str) -> Result<(u16, Vec<u8>)> {
            self.responses
                .get(url)
                .map(|(s, b)| (*s, b.as_bytes().to_vec()))
                .ok_or_else(|| anyhow::anyhow!("unexpected request: {url}"))
        }
    }
    impl Transport for FakeTransport {
        fn get(&self, url: &str, _auth: Option<&str>) -> Result<(u16, Vec<u8>)> {
            self.lookup(url)
        }
        fn get_stream(&self, url: &str, _auth: Option<&str>) -> Result<Box<dyn Read + Send>> {
            let (_s, b) = self.lookup(url)?;
            Ok(Box::new(std::io::Cursor::new(b)))
        }
    }

    const HOST: &str = "https://partner.example";

    fn client(t: FakeTransport) -> Client<FakeTransport> {
        Client::with_transport(t, HOST, "user", "pass")
    }

    #[test]
    fn collections_parse_id_name_state_and_keep_extra() {
        let t = FakeTransport::default().with(
            "https://partner.example/api/collection?format=json&limit=-1&state=ACTIVE",
            200,
            r#"[{"id":8232,"name":"City Government","state":"ACTIVE","description":"Muni sites","account":42}]"#,
        );
        let colls = client(t).collections(true).unwrap();
        assert_eq!(colls.len(), 1);
        assert_eq!(colls[0].id, 8232);
        assert_eq!(colls[0].name, "City Government");
        assert_eq!(colls[0].state.as_deref(), Some("ACTIVE"));
        // Non-core attributes survive in `extra` for the metadata mapper.
        assert_eq!(
            colls[0].extra.get("description").and_then(|v| v.as_str()),
            Some("Muni sites")
        );
    }

    #[test]
    fn webdata_follows_next_pagination() {
        let page1 = r#"{"count":2,"next":"https://partner.example/wasapi/v1/webdata?collection=8232&page=2","files":[
            {"filename":"A.warc.gz","size":10,"checksums":{"md5":"m1","sha1":"s1"},"collection":8232,"crawl":304244,"crawl-time":"2017-05-31T22:15:40Z","locations":["https://warcs.example/A.warc.gz"]}
        ]}"#;
        let page2 = r#"{"count":2,"next":null,"files":[
            {"filename":"B.warc.gz","size":20,"checksums":{"md5":"m2"},"collection":8232,"crawl":304245,"crawl-time":"2017-06-01T00:00:00Z","locations":["https://warcs.example/B.warc.gz"]}
        ]}"#;
        let t = FakeTransport::default()
            .with(
                "https://partner.example/wasapi/v1/webdata?collection=8232",
                200,
                page1,
            )
            .with(
                "https://partner.example/wasapi/v1/webdata?collection=8232&page=2",
                200,
                page2,
            );
        let files = client(t)
            .webdata(&WasapiQuery {
                collection: Some(8232),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(files.len(), 2, "both pages collected");
        assert_eq!(files[0].filename, "A.warc.gz");
        assert_eq!(files[0].crawl, Some(304244));
        assert_eq!(files[0].crawl_time.as_deref(), Some("2017-05-31T22:15:40Z"));
        assert_eq!(files[0].checksums.sha1.as_deref(), Some("s1"));
        assert_eq!(files[1].filename, "B.warc.gz");
        assert_eq!(files[1].crawl, Some(304245));
    }

    #[test]
    fn download_streams_to_dest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("A.warc.gz");
        let t = FakeTransport::default().with("https://warcs.example/A.warc.gz", 200, "WARC-BYTES");
        let n = client(t)
            .download("https://warcs.example/A.warc.gz", &dest)
            .unwrap();
        assert_eq!(n, 10);
        assert_eq!(std::fs::read(&dest).unwrap(), b"WARC-BYTES");
        assert!(
            !dest.with_extension("part").exists(),
            "temp .part cleaned up"
        );
    }

    #[test]
    fn error_status_is_surfaced() {
        let t = FakeTransport::default().with(
            "https://partner.example/api/collection?format=json&limit=-1",
            403,
            "Forbidden",
        );
        let err = client(t).collections(false).unwrap_err();
        assert!(
            format!("{err:#}").contains("403"),
            "status surfaced: {err:#}"
        );
    }
}
