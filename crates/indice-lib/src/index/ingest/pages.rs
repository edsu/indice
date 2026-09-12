//! Reading one WACZ into page documents: the read/extract/merge engine.
//!
//! [`index`] is the only door — give it a [`WaczAccess`] and it picks the right
//! strategy (flatten a nested multi-WACZ, CDX-guided streaming, or a full scan),
//! caches a thumbnail, and returns the [`CrawlStats`] the manifest needs. The
//! rest of the module is that machinery: fetching the records the CDX points at,
//! turning WARC records into [`RawRecord`]s, and merging them into one document
//! per URL.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rayon::prelude::*;
use tracing::{debug, info};

use crate::http_range::{RangeFetch, RangeReader};
use crate::index::IndexProgress;
use crate::search::{extract_html_text, SearchIndex};
use crate::wacz::{extract_warc_from_wacz, iter_warc_paths};
use crate::warc::{iter_records, WarcRecord, Warcinfo};

use super::acquire::local_warcs_streamable;
use super::WaczAccess;

/// Everything the page-indexing pass needs beyond the WACZ itself: which crawl
/// and collection the pages belong to, where to write them, how wide to fan out,
/// and where a representative thumbnail should land.
/// Which crawl (and collection) the documents being written belong to, and the
/// index they go into.
///
/// Every page-indexing function needs exactly this quartet, so it travels as one
/// named value rather than four parallel arguments — three of which are `&str`
/// in a fixed order, an easy thing to transpose and (because the manifest would
/// still be correct) an easy thing not to notice. Naming the fields at each
/// construction site is what makes that mistake visible; the guard is
/// `tests/integration.rs::a_page_document_is_tagged_with_its_own_crawl_and_collection`.
///
/// `Copy`, so a callee can destructure it and keep using the bare names.
#[derive(Clone, Copy)]
pub(super) struct Docs<'a> {
    /// The crawl's id, tagged on each page as `crawl_id`.
    pub crawl_id: &'a str,
    /// The crawl's display name, tagged as `crawl_name`.
    pub crawl_name: &'a str,
    /// The curated collection id (slug) the crawl belongs to.
    pub collection: &'a str,
    pub search: &'a Mutex<SearchIndex>,
}

pub(super) struct Ctx<'a> {
    pub docs: Docs<'a>,
    pub workers: usize,
    pub thumbs_dir: &'a Path,
    pub pinned_thumb: &'a Path,
    pub main_page_url: Option<&'a str>,
    pub progress: &'a dyn IndexProgress,
}

/// Index every page in one WACZ.
///
/// A nested multi-WACZ is detected and flattened first (see [`index_nested`]).
/// Otherwise the WACZ is read CDX-guided when its WARCs are Stored — remotely
/// over HTTP ranges, locally over the file — and only falls back to scanning
/// every WARC record when it can't be.
pub(super) fn index(access: &WaczAccess, ctx: &Ctx) -> Result<CrawlStats> {
    if let Some(nested) = index_nested(access, ctx)? {
        return Ok(nested);
    }
    match access {
        WaczAccess::Stream { url } => {
            info!(url = %url, "streaming remote WACZ index (no download)");
            let fetch = crate::http_range::HttpFetch::open(url)?;
            stream_with_thumbnail(fetch, url, ctx)
        }
        WaczAccess::Local { path, .. } => {
            // CDX-guided when the WARCs are Stored (the WACZ spec's SHOULD, always
            // true for Browsertrix output) so a CDX offset maps to a byte
            // position; otherwise fall back to a full scan of every WARC record.
            if local_warcs_streamable(path).unwrap_or(false) {
                let fetch = crate::http_range::FileFetch::open(path)
                    .with_context(|| format!("opening {} for CDX-guided index", path.display()))?;
                stream_with_thumbnail(fetch, &path.display().to_string(), ctx)
            } else {
                // The scan path has no cheap up-front record total, so it stays on
                // the spinner (no determinate bar). Label it "scanning" - it reads
                // every WARC record, unlike the CDX-guided path.
                ctx.progress.phase("scanning");
                index_wacz(path, &ctx.docs)
            }
        }
    }
}

/// The CDX-guided path, shared by a remote stream and a local file — they differ
/// only in the [`RangeFetch`] behind them, so index and thumbnail once, here.
fn stream_with_thumbnail<F>(fetch: F, label: &str, ctx: &Ctx) -> Result<CrawlStats>
where
    F: RangeFetch + Clone + Send + Sync,
{
    let stats = index_wacz_streaming(fetch.clone(), &ctx.docs, label, ctx.workers, ctx.progress)?;
    cache_thumbnail(
        fetch,
        ctx.thumbs_dir,
        ctx.docs.crawl_id,
        ctx.main_page_url,
        ctx.pinned_thumb,
    );
    Ok(stats)
}

/// A raw contribution to a page's search document, parsed from one WARC record.
enum RawRecord {
    /// An HTML response: source of the page title, description, headings, and a
    /// scraped-text fallback body. (PDF responses reuse this variant with just a
    /// title and body.)
    Html {
        url: String,
        timestamp: String,
        title: String,
        body: String,
        description: String,
        headings: String,
        keywords: String,
        author: String,
        /// `"html"` or `"pdf"`.
        media_type: String,
        /// `<html lang>` value (empty for PDFs).
        lang: String,
        /// HTTP response status code, if known.
        status: Option<u16>,
        /// Year from the HTTP `Last-Modified` header, if present.
        modified_year: Option<u64>,
    },
    /// Fully rendered (post-JS) page text - Browsertrix's `urn:text:` resource
    /// record, or the `text` field from `pages/*.jsonl`. Richer than scraped
    /// HTML, especially for SPAs. `title` is set only for the pages.jsonl source
    /// (used as a fallback when the HTML capture has no title).
    Text {
        url: String,
        timestamp: String,
        text: String,
        title: Option<String>,
    },
}

/// Accumulated per-URL data merged from all WARC records for that page.
#[derive(Default)]
struct MergedPage {
    timestamp: String,
    title: Option<String>,
    html_body: Option<String>,
    rendered_text: Option<String>,
    description: Option<String>,
    headings: Option<String>,
    keywords: Option<String>,
    author: Option<String>,
    media_type: Option<String>,
    lang: Option<String>,
    status: Option<u16>,
    modified_year: Option<u64>,
}

/// Provenance and capture stats gathered during one WACZ indexing pass, so the
/// manifest can record them without re-reading the WARCs.
#[derive(Default, Debug)]
pub(super) struct CrawlStats {
    pub pages: u64,
    pub earliest_capture: Option<String>,
    pub latest_capture: Option<String>,
    pub warcinfo: Option<Warcinfo>,
    /// For a nested multi-WACZ: how many inner WACZs were flattened into this
    /// crawl. `None` for an ordinary (flat) WACZ.
    pub nested_waczs: Option<u64>,
    /// HTTP status-code histogram tallied from the CDX (every capture, including
    /// the bodyless 4xx/5xx that never become search documents) — the derived
    /// "capture quality" / Appraisal signal.
    pub status_counts: BTreeMap<u16, u64>,
}

/// Tally HTTP status codes across every CDX record — the capture-quality signal.
fn tally_status(cdx: &[crate::wacz::CdxjRecord]) -> BTreeMap<u16, u64> {
    let mut counts = BTreeMap::new();
    for rec in cdx {
        if rec.status != 0 {
            *counts.entry(rec.status).or_insert(0) += 1;
        }
    }
    counts
}

/// Detect + index a **nested multi-WACZ** (a WACZ whose payload is other WACZ
/// files; see [`crate::wacz::nested_wacz_locations`]) — e.g. Browsertrix's
/// combined collection `/download`. Each inner `.wacz` is indexed under this
/// (outer) crawl's id, so the whole thing stays one manifest entry (flatten).
/// Returns `None` when the WACZ isn't nested, so the caller falls through to
/// ordinary indexing. Replay is unaffected — wabac.js already resolves nesting.
///
/// A Stored inner WACZ is a contiguous byte window of the outer file, so it's
/// streamed **in place** via a [`SubRangeFetch`] — no extraction, and a remote
/// outer fetches only the ranges it needs. (Only a compressed inner entry, or an
/// inner whose own WARCs aren't Stored, has to be materialized to a temp file —
/// not the Browsertrix case.)
///
/// [`SubRangeFetch`]: crate::http_range::SubRangeFetch
fn index_nested(access: &WaczAccess, ctx: &Ctx) -> Result<Option<CrawlStats>> {
    match access {
        WaczAccess::Local { path, .. } => index_nested_from(
            crate::http_range::FileFetch::open(path)?,
            &ctx.docs,
            ctx.workers,
            ctx.progress,
        ),
        WaczAccess::Stream { url } => index_nested_from(
            crate::http_range::HttpFetch::open(url)?,
            &ctx.docs,
            ctx.workers,
            ctx.progress,
        ),
    }
}

/// Core of [`index_nested`], generic over the outer WACZ's byte source
/// (`FileFetch` locally, `HttpFetch` remotely).
pub(super) fn index_nested_from<F: RangeFetch + Clone + Send + Sync>(
    outer: F,
    docs: &Docs,
    workers: usize,
    progress: &dyn IndexProgress,
) -> Result<Option<CrawlStats>> {
    let inners = {
        let mut zip = zip::ZipArchive::new(RangeReader::new(outer.clone()))
            .context("opening WACZ to check for nesting")?;
        crate::wacz::nested_wacz_locations(&mut zip)
    };
    if inners.is_empty() {
        return Ok(None);
    }
    info!(count = inners.len(), "indexing a nested multi-WACZ");

    let mut agg = CrawlStats::default();
    for (i, inner) in inners.iter().enumerate() {
        progress.phase(&format!("nested WACZ {}/{}", i + 1, inners.len()));
        let stats = match inner.inline {
            // Stored: read it in place as a window of the outer file.
            Some((base, len)) => index_inner(
                crate::http_range::SubRangeFetch::new(outer.clone(), base, len),
                &inner.name,
                docs,
                workers,
                progress,
            )?,
            // Compressed inner entry: extract it (decompressing) to a temp file,
            // then full-scan it. Rare — not produced by Browsertrix.
            None => {
                let mut zip = zip::ZipArchive::new(RangeReader::new(outer.clone()))?;
                let mut tmp =
                    tempfile::NamedTempFile::new().context("temp for compressed nested WACZ")?;
                std::io::copy(&mut zip.by_name(&inner.name)?, tmp.as_file_mut())
                    .with_context(|| format!("extracting nested {}", inner.name))?;
                index_wacz(tmp.path(), docs)?
            }
        };
        agg.pages += stats.pages;
        merge_min(&mut agg.earliest_capture, stats.earliest_capture);
        merge_max(&mut agg.latest_capture, stats.latest_capture);
        if agg.warcinfo.is_none() {
            agg.warcinfo = stats.warcinfo;
        }
        for (code, n) in stats.status_counts {
            *agg.status_counts.entry(code).or_insert(0) += n;
        }
    }
    agg.nested_waczs = Some(inners.len() as u64);
    Ok(Some(agg))
}

/// Index one inner WACZ presented as a [`RangeFetch`] window. CDX-guided
/// (streaming, no extraction) when its WARCs are Stored; otherwise the window is
/// materialized to a temp file and full-scanned (rare).
fn index_inner<F: RangeFetch + Clone + Send + Sync>(
    fetch: F,
    label: &str,
    docs: &Docs,
    workers: usize,
    progress: &dyn IndexProgress,
) -> Result<CrawlStats> {
    let streamable = zip::ZipArchive::new(RangeReader::new(fetch.clone()))
        .ok()
        .map(|mut z| crate::wacz::warcs_stored(&mut z).unwrap_or(false))
        .unwrap_or(false);
    if streamable {
        index_wacz_streaming(fetch, docs, label, workers, progress)
    } else {
        let tmp = materialize_fetch(&fetch).context("materializing nested WACZ for scan")?;
        index_wacz(tmp.path(), docs)
    }
}

/// Copy an entire [`RangeFetch`] to a temp file (chunked), for the fallback
/// paths that need a seekable local file.
fn materialize_fetch<F: RangeFetch>(fetch: &F) -> Result<tempfile::NamedTempFile> {
    use std::io::Write;
    const CHUNK: u64 = 4 * 1024 * 1024;
    let total = fetch.total_len();
    let mut tmp = tempfile::NamedTempFile::new()?;
    let mut pos = 0u64;
    while pos < total {
        let end = (pos + CHUNK).min(total);
        tmp.as_file_mut().write_all(&fetch.fetch(pos, end)?)?;
        pos = end;
    }
    Ok(tmp)
}

/// Keep the smaller of two optional 14-digit capture timestamps (they sort
/// lexicographically), for aggregating a nested WACZ's capture range.
fn merge_min(acc: &mut Option<String>, v: Option<String>) {
    if let Some(v) = v {
        if acc.as_ref().is_none_or(|a| v < *a) {
            *acc = Some(v);
        }
    }
}
/// Keep the larger of two optional 14-digit capture timestamps.
fn merge_max(acc: &mut Option<String>, v: Option<String>) {
    if let Some(v) = v {
        if acc.as_ref().is_none_or(|a| v > *a) {
            *acc = Some(v);
        }
    }
}

/// Index all WARC entries inside a WACZ file into the Tantivy full-text index.
///
/// Records are collected across every inner WARC (rendered `urn:text:` records
/// often live in a separate WARC from the HTML response), merged into one
/// document per URL, and indexed once. The body prefers Browsertrix's rendered
/// text and falls back to scraped HTML; the title comes from the HTML.
pub(super) fn index_wacz(wacz_path: &Path, docs: &Docs) -> Result<CrawlStats> {
    let warc_paths: Vec<_> = iter_warc_paths(wacz_path)?
        .collect::<Result<Vec<_>>>()
        .with_context(|| format!("listing WARC entries in {}", wacz_path.display()))?;

    let per_warc: Vec<(Vec<RawRecord>, Option<Warcinfo>)> = warc_paths
        .par_iter()
        .map(|entry_name| {
            let tmp = extract_warc_from_wacz(wacz_path, entry_name).with_context(|| {
                format!("extracting {} from {}", entry_name, wacz_path.display())
            })?;
            collect_page_records(tmp.path())
        })
        .collect::<Result<Vec<_>>>()?;

    // Flatten to all records + the first warcinfo (warcinfo leads its WARC, so
    // the first WARC's is the crawl-level record), then merge and index.
    let mut warcinfo: Option<Warcinfo> = None;
    let mut raws: Vec<RawRecord> = Vec::new();
    for (r, wi) in per_warc {
        if warcinfo.is_none() {
            warcinfo = wi;
        }
        raws.extend(r);
    }
    // Fold in pages.jsonl/extraPages.jsonl extracted text (see the streaming
    // path); some crawls store rendered text only there, not in the WARCs. Read
    // the CDX here too for the capture-quality status tally (every capture,
    // including bodyless 4xx/5xx that never became RawRecords).
    let mut status_counts = BTreeMap::new();
    if let Ok(file) = std::fs::File::open(wacz_path) {
        if let Ok(mut zip) = zip::ZipArchive::new(file) {
            raws.extend(
                crate::wacz::read_page_texts(&mut zip)
                    .into_iter()
                    .map(|pt| RawRecord::Text {
                        url: pt.url,
                        timestamp: pt.ts,
                        text: pt.text,
                        title: pt.title,
                    }),
            );
            if let Ok(cdx) = crate::wacz::cdx_records(&mut zip) {
                status_counts = tally_status(&cdx);
            }
        }
    }
    index_merged(
        raws,
        warcinfo,
        status_counts,
        docs,
        &wacz_path.display().to_string(),
    )
}

/// Index a WACZ by CDX-guided/streaming extraction over a `Read + Seek` source
/// (a local file or an HTTP range reader): read only the page-relevant records
/// the CDX points at, rather than scanning every WARC record. Produces the same
/// index as [`index_wacz`] (both share [`record_to_raw`] and [`index_merged`]).
pub(super) fn index_wacz_streaming<F>(
    fetch: F,
    docs: &Docs,
    label: &str,
    concurrency: usize,
    progress: &dyn IndexProgress,
) -> Result<CrawlStats>
where
    F: crate::http_range::RangeFetch + Clone + Send + Sync,
{
    let (raws, warcinfo, status_counts) =
        collect_page_records_via_cdx(fetch, concurrency, progress)?;
    index_merged(raws, warcinfo, status_counts, docs, label)
}

/// Merge per-record contributions into one document per URL and index them.
/// Shared by the scan-everything ([`index_wacz`]) and CDX-guided
/// ([`index_wacz_streaming`]) paths.
fn index_merged(
    raws: Vec<RawRecord>,
    warcinfo: Option<Warcinfo>,
    status_counts: BTreeMap<u16, u64>,
    docs: &Docs,
    label: &str,
) -> Result<CrawlStats> {
    // The only phase that reads the tags rather than forwarding them; the
    // others just pass `docs` along.
    let Docs {
        crawl_id,
        crawl_name,
        collection,
        search,
    } = *docs;
    let build_start = std::time::Instant::now();
    let mut pages: HashMap<String, MergedPage> = HashMap::new();
    {
        for raw in raws {
            match raw {
                RawRecord::Html {
                    url,
                    timestamp,
                    title,
                    body,
                    description,
                    headings,
                    keywords,
                    author,
                    media_type,
                    lang,
                    status,
                    modified_year,
                } => {
                    let e = pages.entry(url).or_default();
                    // The HTML capture is the authoritative timestamp for replay.
                    e.timestamp = timestamp;
                    if !title.is_empty() {
                        e.title = Some(title);
                    }
                    if !body.is_empty() {
                        e.html_body = Some(body);
                    }
                    if !description.is_empty() {
                        e.description = Some(description);
                    }
                    if !headings.is_empty() {
                        e.headings = Some(headings);
                    }
                    if !keywords.is_empty() {
                        e.keywords = Some(keywords);
                    }
                    if !author.is_empty() {
                        e.author = Some(author);
                    }
                    if !media_type.is_empty() {
                        e.media_type = Some(media_type);
                    }
                    if !lang.is_empty() {
                        e.lang = Some(lang);
                    }
                    if status.is_some() {
                        e.status = status;
                    }
                    if modified_year.is_some() {
                        e.modified_year = modified_year;
                    }
                }
                RawRecord::Text {
                    url,
                    timestamp,
                    text,
                    title,
                } => {
                    let e = pages.entry(url).or_default();
                    if e.timestamp.is_empty() {
                        e.timestamp = timestamp;
                    }
                    e.rendered_text = Some(text);
                    // Rendered text always comes from an HTML page.
                    e.media_type.get_or_insert_with(|| "html".to_string());
                    // A pages.jsonl title fills in only when the HTML capture
                    // gave none - the scraped HTML <title> wins when present.
                    if let Some(t) = title {
                        if !t.is_empty() {
                            e.title.get_or_insert(t);
                        }
                    }
                }
            }
        }
    }

    let mut count = 0u64;
    let mut earliest: Option<String> = None;
    let mut latest: Option<String> = None;
    {
        use crate::search::Page;
        let mut s = search.lock().unwrap();
        for (url, m) in pages {
            // Prefer the fully rendered text; fall back to scraped HTML.
            let body = m.rendered_text.or(m.html_body).unwrap_or_default();
            let title = m.title.unwrap_or_default();
            let description = m.description.unwrap_or_default();
            let headings = m.headings.unwrap_or_default();
            let keywords = m.keywords.unwrap_or_default();
            let author = m.author.unwrap_or_default();
            let media_type = m.media_type.unwrap_or_default();
            let lang = m.lang.unwrap_or_default();
            if title.is_empty() && body.is_empty() && description.is_empty() {
                continue;
            }
            s.index_page(&Page {
                url: &url,
                timestamp: &m.timestamp,
                title: &title,
                body: &body,
                description: &description,
                headings: &headings,
                keywords: &keywords,
                author: &author,
                media_type: &media_type,
                lang: &lang,
                status: m.status,
                modified_year: m.modified_year,
                crawl_id,
                crawl_name,
                collection,
            })?;
            count += 1;
            // Track the capture date range (14-digit timestamps sort
            // chronologically as plain strings).
            if !m.timestamp.is_empty() {
                if earliest.as_deref().is_none_or(|e| m.timestamp.as_str() < e) {
                    earliest = Some(m.timestamp.clone());
                }
                if latest.as_deref().is_none_or(|l| m.timestamp.as_str() > l) {
                    latest = Some(m.timestamp.clone());
                }
            }
        }
    }

    debug!(build_ms = build_start.elapsed().as_millis() as u64, wacz = %label, "built index");
    info!(pages = count, wacz = %label, "indexed pages from WACZ");
    Ok(CrawlStats {
        pages: count,
        earliest_capture: earliest,
        latest_capture: latest,
        warcinfo,
        // Set by index_nested for a multi-WACZ; a single WACZ isn't nested.
        nested_waczs: None,
        status_counts,
    })
}

/// Parse an extracted WARC file into raw per-record contributions (HTML
/// responses and `urn:text:` rendered-text resources). Other record types
/// (images, JS, CSS, redirects, other `urn:` pseudo-records) are ignored.
fn collect_page_records(warc_path: &Path) -> Result<(Vec<RawRecord>, Option<Warcinfo>)> {
    let records: Vec<WarcRecord> = iter_records(warc_path)
        .with_context(|| format!("reading {}", warc_path.display()))?
        .collect::<Result<Vec<_>>>()?;

    let mut out = Vec::new();
    let mut warcinfo: Option<Warcinfo> = None;
    for record in &records {
        // Capture the crawl's warcinfo (checked before the URI gate in
        // record_to_raw, since warcinfo records carry no WARC-Target-URI).
        if warcinfo.is_none() {
            if let Some(info) = Warcinfo::from_record(record) {
                if !info.is_empty() {
                    warcinfo = Some(info);
                }
            }
        }
        if let Some(raw) = record_to_raw(record) {
            out.push(raw);
        }
    }

    Ok((out, warcinfo))
}

/// Page records read from a WACZ, the crawl-level `warcinfo`, and the HTTP status
/// tally over the CDX (the capture-quality signal).
type PageRecords = (Vec<RawRecord>, Option<Warcinfo>, BTreeMap<u16, u64>);

/// CDX-guided extraction over a `Read + Seek` WACZ: read the CDX, fetch only the
/// page-relevant records (HTML/PDF responses and `urn:text:` rendered text) by
/// seeking to `data_start + offset`, and transform each with [`record_to_raw`].
/// Images/JS/JSON/pageinfo/thumbnail captures are never fetched. Streaming
/// indexes exactly what the CDX lists (authoritative for Browsertrix WACZs).
fn collect_page_records_via_cdx<F>(
    fetch: F,
    concurrency: usize,
    progress: &dyn IndexProgress,
) -> Result<PageRecords>
where
    F: crate::http_range::RangeFetch + Clone + Send + Sync,
{
    use crate::wacz;
    use std::sync::atomic::{AtomicU64, Ordering};

    progress.phase("reading index");
    let read_start = std::time::Instant::now();

    // Setup (serial): read the ZIP central directory, the CDX, each WARC's
    // data-start, and the warcinfo over a buffered range reader.
    let mut zip = zip::ZipArchive::new(crate::http_range::RangeReader::new(fetch.clone()))
        .context("opening WACZ ZIP")?;
    wacz::ensure_warcs_stored(&mut zip)?;
    let cdx = wacz::cdx_records(&mut zip)?;
    let status_counts = tally_status(&cdx);
    let starts = wacz::warc_data_starts(&mut zip)?;
    let warcinfo = wacz::find_warcinfo_streaming(&mut zip)?;
    // Extracted page text from pages.jsonl/extraPages.jsonl (read once, while the
    // ZIP is open). Some crawls store rendered text only here - not as urn:text:
    // WARC records the CDX points at - so without this that text is unsearchable.
    let page_texts = wacz::read_page_texts(&mut zip);
    drop(zip);

    // Records that can become a page; skip media/pseudo-records. The count is the
    // determinate-bar total (each is one fetch + extract).
    let wanted: Vec<&crate::wacz::CdxjRecord> = cdx
        .iter()
        .filter(|c| {
            c.length != 0
                && (c.url.starts_with("urn:text:")
                    || c.mime.contains("html")
                    || c.mime.contains("pdf"))
        })
        .collect();
    progress.set_total(wanted.len() as u64);

    // Fetch + extract each wanted record concurrently. The CDX gives every record
    // an independent (offset, length), so fetches don't depend on each other:
    // fanning out hides per-record round-trip latency (the win for remote WACZs)
    // and parallelizes HTML/PDF text extraction (CPU) across cores.
    let done = AtomicU64::new(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(concurrency)
        .build()
        .context("building record-fetch thread pool")?;
    let mut out: Vec<RawRecord> = pool.install(|| {
        wanted
            .par_iter()
            .flat_map_iter(|c| {
                let base = c.filename.rsplit('/').next().unwrap_or(&c.filename);
                let raws: Vec<RawRecord> = match starts.get(base) {
                    Some(&start) => {
                        let (from, len) = (start + c.offset, c.length);
                        match fetch
                            .fetch(from, from + len)
                            .map_err(anyhow::Error::from)
                            .and_then(|bytes| wacz::records_from_slice(&bytes, from, len))
                        {
                            Ok(records) => records.iter().filter_map(record_to_raw).collect(),
                            Err(e) => {
                                tracing::warn!(url = %c.url, "skipping unreadable CDX record: {e:#}");
                                Vec::new()
                            }
                        }
                    }
                    None => Vec::new(),
                };
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                progress.set_records(n);
                raws
            })
            .collect()
    });

    // Fold in the pages.jsonl/extraPages.jsonl text as rendered-text records;
    // index_merged merges them into the matching page docs by URL (most already
    // exist from their HTML response, so this enriches rather than duplicates).
    let page_text_count = page_texts.len();
    out.extend(page_texts.into_iter().map(|pt| RawRecord::Text {
        url: pt.url,
        timestamp: pt.ts,
        text: pt.text,
        title: pt.title,
    }));

    debug!(
        records = wanted.len(),
        page_texts = page_text_count,
        read_ms = read_start.elapsed().as_millis() as u64,
        concurrency,
        "read page records via CDX"
    );
    // Records read. The merge + Tantivy indexing that follows has no per-record
    // total, so drop the determinate bar back to a spinner - otherwise it sits at
    // 100% with a decaying rate/ETA during the slow tail (very visible for a local
    // file, where reads are near-instant and the tail dominates).
    progress.phase("building index");
    Ok((out, warcinfo, status_counts))
}

/// Cache a representative thumbnail for a crawl (best-effort). Any failure - no
/// main page, no `og:image`, or an image we can't fetch/decode - is logged at
/// debug and ignored; the UI falls back to a CSS placeholder.
fn cache_thumbnail<F>(
    fetch: F,
    thumbs_dir: &Path,
    crawl_id: &str,
    main_page_url: Option<&str>,
    pinned_dest: &Path,
) where
    F: crate::http_range::RangeFetch + Clone + Send + Sync,
{
    let Some(url) = main_page_url else {
        return;
    };
    match crate::thumbnail::generate(fetch, thumbs_dir, crawl_id, url, pinned_dest) {
        Ok(true) => debug!(crawl_id, "cached representative thumbnail"),
        Ok(false) => {}
        Err(e) => debug!(crawl_id, "thumbnail generation failed: {e:#}"),
    }
}

/// Per-host ceiling on fetch concurrency. Even when a user asks for more (or the
/// local core count is very high), we never run more than this many concurrent
/// range requests against a single WACZ's host — a proactive politeness cap that
/// bounds simultaneous load and defends against a mis-typed `--concurrency`. 64 is
/// generous for object stores like S3 while still a hard bound for small servers.
pub(super) const MAX_CONCURRENCY: usize = 64;

/// Default worker count for the concurrent CDX-guided record fetch+extract, when
/// `--concurrency` isn't given.
///
/// Remote defaults to a deliberately gentle 4: a single WACZ's requests all hit
/// one host, and indice is meant to be pointed at arbitrary (often small)
/// servers, so it's polite by default while still ~4x faster than serial. Users
/// hitting an object store (e.g. S3) can raise it with `--concurrency`.
///
/// Local defaults to the core count: it's your own disk (no politeness concern)
/// and the work is CPU-bound text extraction, so cores are the sweet spot.
pub(super) fn default_concurrency(remote: bool) -> usize {
    if remote {
        4
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    }
}

/// Transform one WARC record into an indexable [`RawRecord`], or `None` if it is
/// not a page: warcinfo, `dns:`, other `urn:` pseudo-records (pageinfo,
/// thumbnail, …), non-HTML/PDF responses, or empty payloads. Shared by the
/// scan-everything path ([`collect_page_records`]) and the CDX-guided path
/// ([`collect_page_records_via_cdx`]) so both index identically.
fn record_to_raw(record: &WarcRecord) -> Option<RawRecord> {
    let uri = record.target_uri.as_str();
    if uri.is_empty() || uri.starts_with("dns:") {
        return None;
    }

    // Browsertrix stores fully rendered page text as a `urn:text:<url>` resource
    // record (WARC-Type: resource). Map it back to the real URL as the body.
    if let Some(real_url) = uri.strip_prefix("urn:text:") {
        let text = String::from_utf8_lossy(&record.payload).trim().to_string();
        if text.is_empty() {
            return None;
        }
        return Some(RawRecord::Text {
            url: real_url.to_string(),
            timestamp: record.timestamp.clone(),
            text,
            title: None,
        });
    }

    // Skip other urn: pseudo-records (pageinfo, thumbnail, view, …).
    if uri.starts_with("urn:") || !record.warc_type.eq_ignore_ascii_case("response") {
        return None;
    }
    let mime = record.content_type.to_ascii_lowercase();

    // PDF responses: extract text as the body, title from the URL's filename.
    if mime.contains("pdf") {
        if record.payload.is_empty() {
            return None;
        }
        let Some(text) = crate::pdf::extract_pdf_text(&record.payload) else {
            debug!(url = uri, "PDF text extraction yielded nothing; skipping");
            return None;
        };
        return Some(RawRecord::Html {
            url: uri.to_string(),
            timestamp: record.timestamp.clone(),
            title: pdf_title_from_url(uri),
            body: text,
            description: String::new(),
            headings: String::new(),
            keywords: String::new(),
            author: String::new(),
            media_type: "pdf".to_string(),
            lang: String::new(),
            status: record.http_status,
            modified_year: last_modified_year(&record.http_headers),
        });
    }

    if !mime.contains("html") || record.payload.is_empty() {
        return None;
    }
    let html = extract_html_text(&record.payload);
    if html.title.is_empty() && html.body.is_empty() && html.description.is_empty() {
        return None;
    }
    Some(RawRecord::Html {
        url: uri.to_string(),
        timestamp: record.timestamp.clone(),
        title: html.title,
        body: html.body,
        description: html.description,
        headings: html.headings,
        keywords: html.keywords,
        author: html.author,
        media_type: "html".to_string(),
        lang: html.lang,
        status: record.http_status,
        modified_year: last_modified_year(&record.http_headers),
    })
}

/// The year from an HTTP `Last-Modified` header, or `None` if the header is
/// absent or unparseable. Only the modern IMF-fixdate form (RFC 7231, e.g.
/// `Wed, 21 Oct 2015 07:28:00 GMT`) is parsed — the two obsolete HTTP-date
/// formats (RFC 850 and asctime) are rare and yield `None`.
pub(super) fn last_modified_year(headers: &[(String, String)]) -> Option<u64> {
    let value = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("last-modified"))
        .map(|(_, v)| v.as_str())?;
    // IMF-fixdate is RFC 2822-compatible (chrono accepts the "GMT" zone).
    let dt = chrono::DateTime::parse_from_rfc2822(value.trim()).ok()?;
    let year = chrono::Datelike::year(&dt);
    (year > 0).then_some(year as u64)
}

/// Derive a page title for a PDF from the last path segment of its URL
/// (e.g. `https://x.org/docs/report.pdf` -> `report.pdf`), falling back to the
/// full URL when there is no usable segment.
fn pdf_title_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/')
        .find(|seg| !seg.is_empty())
        .unwrap_or(url)
        .to_string()
}
