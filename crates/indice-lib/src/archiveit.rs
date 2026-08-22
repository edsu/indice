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
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Collection {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Every other collection field (descriptive `metadata`, `topics`, dates, …),
    /// captured so the metadata mapper and `collection_provenance` allowlist can
    /// read them without this struct hard-coding keys.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// Allowlists of the fields we embed in a built WACZ's `datapackage.json`, one
// per Archive-It record. These are allowlists, not denylists, on purpose: the
// WACZ is meant to be shared/committed, and the raw collection record carries a
// `private_access_token` (grants private-replay access) plus operator PII
// (`created_by`/`last_updated_by`) and account internals. Copying only the
// fields named here means a *new* upstream field — including a future secret —
// can never leak, whereas "dump everything then strip the token" breaks silently
// the moment Archive-It adds another sensitive field.
const COLLECTION_FIELDS: &[&str] = &[
    "id",
    "name",
    "state",
    "publicly_visible",
    "topics",
    "metadata",
    "created_date",
    "last_crawl_date",
];
const CRAWL_FIELDS: &[&str] = &[
    "id",
    "status",
    "type",
    "test_crawl_state",
    "collection",
    "original_start_date",
    "end_date",
];

/// The `archiveit.collection` provenance object embedded in a built WACZ.
fn collection_provenance(c: &Collection) -> serde_json::Value {
    allowlisted(c, COLLECTION_FIELDS)
}

/// The `archiveit.crawl` provenance object embedded in a built WACZ.
fn crawl_provenance(j: &CrawlJob) -> serde_json::Value {
    allowlisted(j, CRAWL_FIELDS)
}

/// Serialize an Archive-It record and keep only the named `fields` (dropping
/// nulls). An allowlist copy: any key not listed — secrets, PII, unknown future
/// fields — is left out by construction. See [`COLLECTION_FIELDS`].
fn allowlisted<T: Serialize>(record: &T, fields: &[&str]) -> serde_json::Value {
    let full = serde_json::to_value(record).unwrap_or(serde_json::Value::Null);
    let src = full.as_object();
    let kept = fields.iter().filter_map(|&k| {
        let v = src?.get(k).filter(|v| !v.is_null())?;
        Some((k.to_string(), v.clone()))
    });
    serde_json::Value::Object(kept.collect())
}

/// Archive-It metadata for enriching built WACZs — the crawl_job and collection
/// records keyed by id — passed to [`import_crawls`] so each crawl's WACZ can
/// embed its `archiveit.crawl` + `archiveit.collection`.
#[derive(Default)]
pub struct Catalog {
    pub crawl_jobs: std::collections::HashMap<i64, CrawlJob>,
    pub collections: std::collections::HashMap<i64, Collection>,
}

/// A crawl job (Partner API `/api/crawl_job`) — the source of truth for a
/// crawl's status and whether it was deleted (WASAPI has neither). `id` matches
/// WASAPI's `crawl`. Only the fields we filter/date on are typed; the rest are
/// ignored.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CrawlJob {
    pub id: i64,
    /// Run outcome: `FINISHED`, `FINISHED_ABORTED`, `FINISHED_TIME_LIMIT`, or a
    /// non-finished state (e.g. running) — `None` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Test-crawl lifecycle; `DELETED` marks a crawl deleted in Archive-It.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_crawl_state: Option<String>,
    /// Crawl type, e.g. `TEST_DELETED`, `TEST_SAVED`.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_start_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_date: Option<String>,
    /// Every other crawl_job field, kept so status/deletion checks can read
    /// fields this struct doesn't type. (Only allowlisted fields are embedded in
    /// the WACZ — see `crawl_provenance`.)
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl CrawlJob {
    /// Deleted in Archive-It (its WARCs may linger in WASAPI, but it shouldn't be
    /// imported).
    pub fn is_deleted(&self) -> bool {
        self.test_crawl_state.as_deref() == Some("DELETED")
            || self
                .kind
                .as_deref()
                .is_some_and(|k| k.ends_with("_DELETED"))
    }
    /// The crawl ran to a finished state. Lenient: an absent status isn't treated
    /// as unfinished (so an unexpected shape doesn't silently drop everything).
    pub fn is_finished(&self) -> bool {
        self.status
            .as_deref()
            .is_none_or(|s| s.starts_with("FINISHED"))
    }
    /// Worth importing by default: finished and not deleted.
    pub fn importable(&self) -> bool {
        !self.is_deleted() && self.is_finished()
    }
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

    /// The account's crawl jobs (optionally filtered to one collection) — for
    /// status/deletion filtering and crawl dates. A fast Partner-API metadata
    /// listing (one row per crawl), unlike the WASAPI per-WARC listing.
    pub fn crawl_jobs(&self, collection: Option<i64>) -> Result<Vec<CrawlJob>> {
        let mut path = String::from("/api/crawl_job?format=json&limit=-1");
        if let Some(c) = collection {
            path.push_str(&format!("&collection={c}"));
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
        tracing::debug!("download {url} → {}", dest.display());
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
        tracing::debug!("GET {url}");
        let (status, body) = self.transport.get(url, Some(&self.auth))?;
        tracing::debug!("GET {url} → HTTP {status} ({} bytes)", body.len());
        if !(200..300).contains(&status) {
            bail!("GET {url} failed (HTTP {status}): {}", body_snippet(&body));
        }
        serde_json::from_slice(&body).with_context(|| format!("parsing response from {url}"))
    }
}

// ── Import orchestration (shared by the CLI and the server job) ────────────────

/// One crawl's WARC files, grouped for building a single WACZ.
#[derive(Debug, Clone)]
pub struct CrawlPlan {
    pub crawl_id: i64,
    pub files: Vec<WarcFile>,
}

/// Group WASAPI files by their `crawl` (job) id (files without one bucket under
/// `0`), sorted by crawl id — the per-crawl WACZ build units.
pub fn plan_crawls(files: Vec<WarcFile>) -> Vec<CrawlPlan> {
    let mut map: std::collections::BTreeMap<i64, Vec<WarcFile>> = std::collections::BTreeMap::new();
    for f in files {
        map.entry(f.crawl.unwrap_or(0)).or_default().push(f);
    }
    map.into_iter()
        .map(|(crawl_id, files)| CrawlPlan { crawl_id, files })
        .collect()
}

/// Summary of an import run.
#[derive(Debug, Default)]
pub struct ImportOutcome {
    pub imported: u64,
    pub skipped: u64,
    /// `(indice crawl id, display name)` for each newly imported crawl.
    pub crawls: Vec<(String, String)>,
}

/// Pull every string value for a Dublin-Core `metadata` key out of an Archive-It
/// collection record. The Partner API nests descriptive metadata under a
/// `metadata` object keyed by capitalized DC terms (`Description`, `Subject`, …),
/// each mapping to an array of `{ "value": "…" }` entries; this reads that shape
/// but also tolerates a bare string or an array of strings, and matches the key
/// case-insensitively so account-specific casing still resolves.
fn metadata_values(c: &Collection, key: &str) -> Vec<String> {
    let Some(md) = c.extra.get("metadata").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let Some(field) = md
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
    else {
        return Vec::new();
    };
    let one = |v: &serde_json::Value| -> Option<String> {
        // `{ "value": "…" }` (Archive-It's shape) or a bare string.
        v.get("value")
            .and_then(|x| x.as_str())
            .or_else(|| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    match field {
        serde_json::Value::Array(arr) => arr.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}

/// Turn an Archive-It `topics` category code (camelCase, e.g.
/// `"artsAndHumanities"`) into a readable subject (`"Arts and Humanities"`):
/// split on case boundaries, capitalize the first word, and lowercase short
/// connectives. A value that isn't camelCase is left as-is (just trimmed).
fn humanize_topic(code: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in code.trim().chars() {
        if ch.is_uppercase() && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        cur.extend(ch.to_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            // Keep connectives lowercase mid-phrase; capitalize everything else.
            if i > 0 && matches!(w.as_str(), "and" | "of" | "the" | "for") {
                w.clone()
            } else {
                let mut c = w.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().chain(c).collect(),
                    None => w.clone(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Map an Archive-It [`Collection`]'s descriptive metadata into indice
/// finding-aid [`CollectionFields`] (fill-gaps via `seed_collection`). Reads the
/// Partner-API `metadata` sub-object (Dublin Core) plus the top-level `topics`
/// list, defensively — unknown or empty fields simply stay unmapped.
pub fn collection_fields(c: &Collection) -> crate::collections::CollectionFields {
    let first = |key: &str| metadata_values(c, key).into_iter().next();

    // Subjects come from DC `Subject` metadata and the collection's `topics`.
    // `topics` is Archive-It's coarse category vocabulary, delivered as a single
    // camelCase code (e.g. "artsAndHumanities") — humanize it so it reads as a
    // subject rather than a raw code. It's usually null (empty account-wide in
    // practice); we still accept a list defensively.
    let mut subjects = metadata_values(c, "Subject");
    match c.extra.get("topics") {
        Some(serde_json::Value::Array(arr)) => {
            subjects.extend(arr.iter().filter_map(|v| v.as_str()).map(humanize_topic))
        }
        Some(serde_json::Value::String(s)) if !s.is_empty() => subjects.push(humanize_topic(s)),
        _ => {}
    }
    let subjects = (!subjects.is_empty()).then_some(subjects);

    crate::collections::CollectionFields {
        // DC Description → narrative (Scope & Content); Title → short abstract.
        narrative: first("Description"),
        description: first("Title"),
        creator: first("Creator").or_else(|| first("Collector")),
        rights: first("Rights"),
        dates: None, // filled from crawl times by the caller when available
        subjects,
        curator: None,
    }
}

/// The calendar-year span (e.g. `2019–2023`) across a set of crawls' WARC times,
/// for the collection's `dates` finding-aid field. `None` if no crawl-time is
/// known. Shared by the CLI and the management-UI import so both seed `dates`
/// the same way.
pub fn crawl_year_range(plans: &[CrawlPlan]) -> Option<String> {
    let years: Vec<String> = plans
        .iter()
        .flat_map(|p| &p.files)
        .filter_map(|f| f.crawl_time.as_deref())
        .filter_map(crate::index::year_prefix)
        .collect();
    let min = years.iter().min()?;
    let max = years.iter().max()?;
    Some(if min == max {
        min.clone()
    } else {
        format!("{min}\u{2013}{max}")
    })
}

/// Removes a directory (best-effort) when dropped — so a crawl's download
/// staging is cleaned up even if the build/index step errors out mid-crawl.
struct DirGuard(std::path::PathBuf);
impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Import a set of grouped crawls into one indice collection `into`: for each
/// crawl not already imported (unless `force`), download its WARCs, build one
/// WACZ, index it, and record provenance (keyed by the crawl's *own* Archive-It
/// collection id, derived from its files — `0` for an uncollected crawl). Then
/// seed `into`'s finding aid from `fields`. Shared by the CLI and the server job
/// so the download→build→index logic lives once.
#[allow(clippy::too_many_arguments)]
pub fn import_crawls<T: Transport>(
    client: &Client<T>,
    home: &Path,
    into: &str,
    plans: &[CrawlPlan],
    fields: &crate::collections::CollectionFields,
    catalog: &Catalog,
    force: bool,
    progress: Option<&dyn crate::index::IndexProgress>,
) -> Result<ImportOutcome> {
    use crate::collections::{slugify, wacz_id, Manifest, Source};

    let host = client.host().to_string();
    // Incremental: crawls already imported (by (host, collection, crawl)).
    let seen: std::collections::HashSet<(String, i64, i64)> =
        Manifest::open(&crate::index::index_dir(home))
            .map(|m| {
                m.waczs
                    .iter()
                    .filter_map(|w| w.archive_it.as_ref())
                    .map(|r| (r.host.clone(), r.collection_id, r.crawl_id))
                    .collect()
            })
            .unwrap_or_default();

    let slug = slugify(into);
    let dest_dir = crate::index::archive_dir(home).join(&slug);
    let mut out = ImportOutcome::default();

    for plan in plans {
        // The crawl's own Archive-It collection (0 = uncollected) — the dedup key
        // and the provenance we record, independent of which indice collection
        // `into` it lands in.
        let ait_collection_id = plan.files.iter().find_map(|f| f.collection).unwrap_or(0);
        if !force && seen.contains(&(host.clone(), ait_collection_id, plan.crawl_id)) {
            out.skipped += 1;
            continue;
        }
        // Stage this crawl's WARC downloads under <home> (same volume as
        // archive/, so building the WACZ files it in place with no cross-device
        // copy, and large crawls use the operator's disk rather than /tmp).
        // `_staging_guard` removes it per crawl even if the build/index errors.
        let out_name = slugify(&format!("ait-{ait_collection_id}-{}", plan.crawl_id));
        let staging = home.join(".import-tmp").join(&out_name);
        let _ = std::fs::remove_dir_all(&staging); // clear a leftover from a prior run
        std::fs::create_dir_all(&staging)
            .with_context(|| format!("creating staging dir {}", staging.display()))?;
        let _staging_guard = DirGuard(staging.clone());

        // Name the crawl after its Archive-It collection title when we have one
        // (far more descriptive than the indice collection it lands in); fall back
        // to `into` otherwise. This title is baked into the WACZ and shown as the
        // crawl heading.
        let coll_title = catalog
            .collections
            .get(&ait_collection_id)
            .map(|c| c.name.trim())
            .filter(|n| !n.is_empty());
        // Narrate the downloads (the slow part): start the spinner *before* the
        // first fetch — `phase()` only updates an already-active bar — and also
        // log at INFO for the no-bar (piped/CI) case.
        let display = format!("{} - crawl {}", coll_title.unwrap_or(into), plan.crawl_id);
        if let Some(p) = progress {
            p.begin(&display);
        }
        let total_files = plan.files.len();
        let mut warcs = Vec::new();
        for (i, f) in plan.files.iter().enumerate() {
            let Some(loc) = f.locations.first() else {
                tracing::warn!(file = %f.filename, "no download location; skipping file");
                continue;
            };
            let status = format!(
                "downloading {} ({}) [{}/{}]",
                f.filename,
                human_size(f.size),
                i + 1,
                total_files
            );
            if let Some(p) = progress {
                p.phase(&status);
            }
            tracing::info!(crawl = plan.crawl_id, "{status}");
            let path = staging.join(&f.filename);
            client.download(loc, &path)?;
            warcs.push(path);
        }
        if warcs.is_empty() {
            tracing::warn!(
                crawl = plan.crawl_id,
                "crawl had no downloadable WARCs; skipping"
            );
            out.skipped += 1;
            continue;
        }

        // Build one WACZ per crawl, directly into the collection's archive dir
        // (so it's indexed in place and its id is deterministic), then index it.
        let created = plan.files.iter().filter_map(|f| f.crawl_time.clone()).min();
        // Persist an allowlisted subset of the source crawl + collection records
        // inside datapackage.json under a custom `archiveit` object (Frictionless
        // allows custom properties), so the provenance travels in the file indice
        // parses and can be surfaced later. Allowlisted (not the raw record) so no
        // secret/PII field can leak into a shareable WACZ — see
        // `collection_provenance`/`crawl_provenance`.
        let mut archiveit = serde_json::Map::new();
        if let Some(job) = catalog.crawl_jobs.get(&plan.crawl_id) {
            archiveit.insert("crawl".to_string(), crawl_provenance(job));
        }
        if let Some(coll) = catalog.collections.get(&ait_collection_id) {
            archiveit.insert("collection".to_string(), collection_provenance(coll));
        }
        let mut datapackage_extra = serde_json::Map::new();
        if !archiveit.is_empty() {
            datapackage_extra.insert(
                "archiveit".to_string(),
                serde_json::Value::Object(archiveit),
            );
        }
        let meta = crate::wacz_build::WaczBuildMeta {
            title: Some(display.clone()),
            created,
            software: Some(format!("Archive-It; indice {}", env!("CARGO_PKG_VERSION"))),
            creator: fields.creator.clone(),
            datapackage_extra,
            ..Default::default()
        };
        if let Some(p) = progress {
            p.phase("building WACZ");
        }
        tracing::info!(crawl = plan.crawl_id, warcs = warcs.len(), "building WACZ");
        let built = crate::wacz_build::build_wacz(&warcs, &meta, &dest_dir, &out_name)?;
        crate::index::index_location(
            &built.path.to_string_lossy(),
            home,
            Some(&display),
            into,
            false, // download
            true,  // force: the importer already made the skip decision
            None,
            progress,
        )?;
        // The filed WACZ is in place under archive/<slug>/; its id is stable.
        let abs = built.path.canonicalize().unwrap_or(built.path.clone());
        let crawl_indice_id = wacz_id(&Source::for_file(&abs, home));
        crate::index::set_archiveit_provenance_by_id(
            home,
            &crawl_indice_id,
            &host,
            ait_collection_id,
            plan.crawl_id,
            plan.files.len() as u64,
            coll_title.unwrap_or(""),
        )?;
        // `index_location` already emits the persistent "✓ indexed N pages" line
        // and INFO log (with the accurate distinct-page count), so don't
        // double-report here — `built.pages` is the pre-dedup seed count and
        // would disagree confusingly.
        out.imported += 1;
        out.crawls.push((crawl_indice_id, display));
    }

    // Remove the staging parent if now empty (best-effort tidy-up).
    let _ = std::fs::remove_dir(home.join(".import-tmp"));

    // Seed the collection finding aid (fill-gaps; curator edits survive).
    crate::index::seed_collection(home, into, fields)?;
    Ok(out)
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

/// A compact human-readable byte size, for progress/log messages.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut n = bytes as f64;
    let mut u = 0;
    while n >= 1024.0 && u < UNITS.len() - 1 {
        n /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{n:.1} {}", UNITS[u])
    }
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

    /// A [`Transport`] returning canned `(status, bytes)` for exact URLs.
    #[derive(Default)]
    struct FakeTransport {
        responses: HashMap<String, (u16, Vec<u8>)>,
    }
    impl FakeTransport {
        fn with(mut self, url: &str, status: u16, body: &str) -> Self {
            self.responses
                .insert(url.to_string(), (status, body.as_bytes().to_vec()));
            self
        }
        fn with_bytes(mut self, url: &str, status: u16, body: Vec<u8>) -> Self {
            self.responses.insert(url.to_string(), (status, body));
            self
        }
        fn lookup(&self, url: &str) -> Result<(u16, Vec<u8>)> {
            self.responses
                .get(url)
                .map(|(s, b)| (*s, b.clone()))
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
    fn collection_fields_maps_dublin_core_and_topics() {
        // Archive-It nests descriptive metadata under `metadata`, keyed by
        // capitalized DC terms whose values are arrays of `{value}` objects.
        // `topics` is a single camelCase category code (the real shape); the DC
        // `metadata` block here is synthetic (accounts that populate it).
        let c: Collection = serde_json::from_str(
            r#"{
                "id":18491,
                "name":"Stephen Ratcliffe Papers",
                "state":"INACTIVE",
                "topics":"artsAndHumanities",
                "metadata":{
                    "Description":[{"value":"Papers of the poet."}],
                    "Title":[{"value":"Ratcliffe"}],
                    "Creator":[{"value":"Ratcliffe, Stephen"}],
                    "Rights":[{"value":"In copyright"}],
                    "Subject":[{"value":"American poetry"},{"value":"Experimental literature"}]
                }
            }"#,
        )
        .unwrap();
        let f = collection_fields(&c);
        assert_eq!(f.narrative.as_deref(), Some("Papers of the poet."));
        assert_eq!(f.description.as_deref(), Some("Ratcliffe"));
        assert_eq!(f.creator.as_deref(), Some("Ratcliffe, Stephen"));
        assert_eq!(f.rights.as_deref(), Some("In copyright"));
        assert_eq!(
            f.subjects,
            Some(vec![
                "American poetry".to_string(),
                "Experimental literature".to_string(),
                "Arts and Humanities".to_string(), // humanized topic code
            ])
        );

        // Empty metadata + null topics (the common real-world case) maps to
        // nothing — matching every collection in the test account.
        let empty: Collection =
            serde_json::from_str(r#"{"id":1,"name":"Empty","metadata":{},"topics":null}"#).unwrap();
        let f = collection_fields(&empty);
        assert!(f.narrative.is_none());
        assert!(f.subjects.is_none());
    }

    #[test]
    fn crawl_jobs_parse_and_flag_deleted() {
        // Real /api/crawl_job output (a Stanford account): all three are deleted
        // test crawls that nonetheless FINISHED — so status alone wouldn't exclude
        // them; test_crawl_state/type does.
        let body = r#"[
            {"id":1342639,"type":"TEST_DELETED","test_crawl_state":"DELETED","status":"FINISHED_ABORTED","collection":15659},
            {"id":1342657,"type":"TEST_DELETED","test_crawl_state":"DELETED","status":"FINISHED","collection":15659},
            {"id":1343658,"type":"TEST_DELETED","test_crawl_state":"DELETED","status":"FINISHED_TIME_LIMIT","collection":15659}
        ]"#;
        let t = FakeTransport::default().with(
            "https://partner.example/api/crawl_job?format=json&limit=-1&collection=15659",
            200,
            body,
        );
        let jobs = client(t).crawl_jobs(Some(15659)).unwrap();
        assert_eq!(jobs.len(), 3);
        for j in &jobs {
            assert!(j.is_deleted(), "crawl {} should be flagged deleted", j.id);
            assert!(j.is_finished(), "crawl {} finished", j.id);
            assert!(
                !j.importable(),
                "a deleted crawl is not importable by default"
            );
        }

        // A finished, non-deleted crawl is importable; a running one isn't.
        let saved: CrawlJob = serde_json::from_str(
            r#"{"id":9,"type":"TEST_SAVED","test_crawl_state":"SAVED","status":"FINISHED"}"#,
        )
        .unwrap();
        assert!(saved.importable());
        let running: CrawlJob = serde_json::from_str(r#"{"id":10,"status":"RUNNING"}"#).unwrap();
        assert!(!running.is_finished());
        assert!(!running.importable());
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

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn import_crawls_downloads_builds_indexes_and_is_incremental() {
        // Two WARC files under one crawl; their download locations serve a real
        // fixture WARC's bytes. Import → one WACZ built per crawl, indexed,
        // provenance recorded; a re-run skips it.
        let warc_bytes = std::fs::read(fixture("simple.warc.gz")).unwrap();
        let t = FakeTransport::default()
            .with_bytes("https://warcs.example/one.warc.gz", 200, warc_bytes.clone())
            .with_bytes("https://warcs.example/two.warc.gz", 200, warc_bytes);
        let client = client(t);

        let files = vec![
            WarcFile {
                filename: "one.warc.gz".into(),
                size: 10,
                checksums: Checksums::default(),
                collection: Some(8232),
                crawl: Some(304244),
                crawl_time: Some("2017-05-31T22:15:40Z".into()),
                locations: vec!["https://warcs.example/one.warc.gz".into()],
            },
            WarcFile {
                filename: "two.warc.gz".into(),
                size: 10,
                checksums: Checksums::default(),
                collection: Some(8232),
                crawl: Some(304244),
                crawl_time: Some("2017-05-31T23:00:00Z".into()),
                locations: vec!["https://warcs.example/two.warc.gz".into()],
            },
        ];
        let plans = plan_crawls(files);
        assert_eq!(plans.len(), 1, "both files group under one crawl");

        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let fields = crate::collections::CollectionFields {
            narrative: Some("A test collection".into()),
            ..Default::default()
        };
        // Catalog: the crawl_job + collection records → embedded under
        // datapackage.json `archiveit`, with the collection's secret redacted.
        let mut catalog = Catalog::default();
        catalog.crawl_jobs.insert(
            304244i64,
            serde_json::from_str::<CrawlJob>(
                // `doc_rate` is an unvetted extra: it must NOT reach the WACZ.
                r#"{"id":304244,"type":"TEST_SAVED","status":"FINISHED","collection":8232,"doc_rate":"1.5"}"#,
            )
            .unwrap(),
        );
        // Collection title differs from the indice collection (`into`) below, so
        // the assertions prove the title flows from the Archive-It collection.
        catalog.collections.insert(
            8232i64,
            serde_json::from_str::<Collection>(
                // `private_access_token` (secret) and `created_by` (PII) are the
                // kind of fields the allowlist must keep out of the WACZ.
                r#"{"id":8232,"name":"City Government Archive","state":"INACTIVE","private_access_token":"SECRET","created_by":"alice@example.edu","topics":null}"#,
            )
            .unwrap(),
        );
        let out = import_crawls(
            &client, home, "City Gov", &plans, &fields, &catalog, false, None,
        )
        .unwrap();
        assert_eq!(out.imported, 1);
        assert_eq!(out.skipped, 0);
        // The crawl is named after its Archive-It collection title, not `into`.
        assert_eq!(out.crawls[0].1, "City Government Archive - crawl 304244");

        // The source crawl + collection records travel inside datapackage.json
        // under `archiveit`; the collection's private_access_token is redacted.
        let wacz_path = crate::index::archive_dir(home)
            .join("city-gov")
            .join("ait-8232-304244.wacz");
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&wacz_path).unwrap()).unwrap();
        let mut dp = String::new();
        std::io::Read::read_to_string(
            &mut zip.by_name("datapackage.json").expect("datapackage.json"),
            &mut dp,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&dp).unwrap();
        let crawl = &v["archiveit"]["crawl"];
        assert_eq!(crawl["id"], 304244);
        assert_eq!(crawl["type"], "TEST_SAVED");
        assert_eq!(crawl["status"], "FINISHED");
        assert!(
            crawl.get("doc_rate").is_none(),
            "unvetted crawl fields are not embedded (allowlist, not denylist)"
        );
        let coll = &v["archiveit"]["collection"];
        assert_eq!(coll["id"], 8232);
        assert_eq!(coll["name"], "City Government Archive");
        assert_eq!(coll["state"], "INACTIVE");
        assert!(
            coll.get("private_access_token").is_none(),
            "collection secret must never reach the WACZ"
        );
        assert!(
            coll.get("created_by").is_none(),
            "operator PII is not embedded (allowlist keeps out unnamed fields)"
        );

        // One WACZ filed under the collection's archive dir, and it indexed.
        let wacz = crate::index::archive_dir(home)
            .join("city-gov")
            .join("ait-8232-304244.wacz");
        assert!(
            wacz.exists(),
            "per-crawl WACZ built in place: {}",
            wacz.display()
        );
        let si = crate::search::SearchIndex::open(&crate::index::index_dir(home).join("full_text"))
            .unwrap();
        assert!(si.num_docs().unwrap() >= 1, "imported crawl indexed a page");

        // Provenance recorded.
        let manifest = crate::collections::Manifest::open(&crate::index::index_dir(home)).unwrap();
        let ait = manifest.waczs[0]
            .archive_it
            .as_ref()
            .expect("archive_it provenance");
        assert_eq!(ait.collection_id, 8232);
        assert_eq!(ait.crawl_id, 304244);
        assert_eq!(ait.warc_count, 2);
        assert_eq!(ait.collection_title, "City Government Archive");

        // A re-run skips the already-imported crawl.
        let again = import_crawls(
            &client, home, "City Gov", &plans, &fields, &catalog, false, None,
        )
        .unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.skipped, 1);
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
