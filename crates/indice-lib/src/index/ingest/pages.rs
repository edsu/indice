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
    /// The most to inflate of any one WACZ member (`index.max_member_mb`). See
    /// [`crate::wacz::DEFAULT_MEMBER_LIMIT`].
    pub read_limit: u64,
}

/// Everything the page-indexing pass needs beyond the WACZ itself: which crawl
/// and collection the pages belong to, where to write them, how wide to fan out,
/// and where a representative thumbnail should land.
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
        WaczAccess::Stream { url, fetch } => {
            info!(url = %url, "streaming remote WACZ index (no download)");
            let stream = crate::http_range::HttpFetch::open(url, *fetch)?;
            stream_with_thumbnail(stream, url, ctx)
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
        ctx.docs.read_limit,
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
pub(in crate::index) struct CrawlStats {
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
        WaczAccess::Stream { url, fetch } => index_nested_from(
            crate::http_range::HttpFetch::open(url, *fetch)?,
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

/// Index a WACZ whose WARCs are compressed inside the zip, so a CDX offset
/// cannot be turned into a position in the WACZ (the WACZ spec says SHOULD be
/// stored; Browsertrix always stores them, some tools do not).
///
/// Each WARC is inflated to a temp file and a CDX is built for it by reading
/// its records once, keeping their positions and types but not their payloads.
/// From there it is indexed like any other WACZ, a page at a time; see
/// [`index_pages`].
pub(super) fn index_wacz(wacz_path: &Path, docs: &Docs) -> Result<CrawlStats> {
    let warc_paths: Vec<_> = iter_warc_paths(wacz_path)?
        .collect::<Result<Vec<_>>>()
        .with_context(|| format!("listing WARC entries in {}", wacz_path.display()))?;
    let limit = docs.read_limit;

    let mut files = Vec::new();
    let mut cdx = Vec::new();
    let mut warcinfo: Option<Warcinfo> = None;
    for (i, entry_name) in warc_paths.iter().enumerate() {
        let tmp = extract_warc_from_wacz(wacz_path, entry_name)
            .with_context(|| format!("extracting {} from {}", entry_name, wacz_path.display()))?;
        let name = format!("warc-{i}");
        for record in iter_records(tmp.path(), limit)? {
            let record = match record {
                Ok(r) => r,
                Err(e) if crate::wacz::is_too_large(&e) => {
                    tracing::warn!(warc = %entry_name, "skipping a record: {e:#}");
                    continue;
                }
                Err(e) => return Err(e.context(format!("reading {entry_name}"))),
            };
            // Captured before the page filter, since warcinfo records carry no
            // WARC-Target-URI.
            if warcinfo.is_none() {
                if let Some(info) = Warcinfo::from_record(&record) {
                    if !info.is_empty() {
                        warcinfo = Some(info);
                    }
                }
            }
            let entry = crate::wacz::CdxjRecord {
                url: record.target_uri,
                timestamp: record.timestamp,
                mime: record.content_type,
                status: record.http_status.unwrap_or(0),
                filename: name.clone(),
                offset: record.offset,
                length: record.record_length,
            };
            if is_page_record(&entry) {
                cdx.push(entry);
            }
        }
        files.push((name, tmp));
    }
    let (fetch, starts) = ConcatFetch::open(&files)?;

    // The status tally reads the WACZ's own CDX, which lists every capture,
    // including the bodyless ones built above would not keep. A nicety, so an
    // unreadable CDX is skipped; one that inflates past the limit is not.
    let mut status_counts = BTreeMap::new();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(wacz_path)?)?;
    match crate::wacz::cdx_records(&mut zip, limit) {
        Ok(all) => status_counts = tally_status(&all),
        Err(e) if crate::wacz::is_too_large(&e) => return Err(e),
        Err(_) => {}
    }

    let label = wacz_path.display().to_string();
    let mut stats = index_pages(
        PageSource {
            fetch,
            cdx: &cdx,
            starts: &starts,
            open_lists: || -> Result<_> {
                Ok(zip::ZipArchive::new(std::fs::File::open(wacz_path)?)?)
            },
        },
        &Run {
            docs,
            label: &label,
            concurrency: default_concurrency(false),
            progress: crate::index::no_progress(),
        },
    )?;
    stats.warcinfo = warcinfo;
    stats.status_counts = status_counts;
    Ok(stats)
}

/// Index a WACZ whose WARCs are stored, over any [`RangeFetch`] (a local file
/// or an HTTP range reader): read its CDX, then index it a page at a time,
/// fetching only the records the CDX says can become a page. See
/// [`index_pages`].
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
    use crate::wacz;
    progress.phase("reading index");
    let limit = docs.read_limit;
    let (cdx, starts, warcinfo) = {
        let mut zip =
            zip::ZipArchive::new(RangeReader::new(fetch.clone())).context("opening WACZ ZIP")?;
        wacz::ensure_warcs_stored(&mut zip)?;
        let cdx = wacz::cdx_records(&mut zip, limit)?;
        let starts = wacz::warc_data_starts(&mut zip)?;
        let warcinfo = wacz::find_warcinfo_streaming(&mut zip, limit)?;
        (cdx, starts, warcinfo)
    };
    let status_counts = tally_status(&cdx);
    let mut stats = index_pages(
        PageSource {
            fetch: fetch.clone(),
            cdx: &cdx,
            starts: &starts,
            open_lists: || -> Result<_> {
                Ok(zip::ZipArchive::new(RangeReader::new(fetch.clone()))?)
            },
        },
        &Run {
            docs,
            label,
            concurrency,
            progress,
        },
    )?;
    stats.warcinfo = warcinfo;
    stats.status_counts = status_counts;
    Ok(stats)
}

/// Whether a CDX record can become a page: an HTML or PDF response, or a
/// `urn:text:` record of rendered text. Media and other pseudo-records are
/// never fetched.
fn is_page_record(c: &crate::wacz::CdxjRecord) -> bool {
    // A content type's case is not significant (`Text/HTML` is HTML), and
    // record_to_raw, which decides in the end, lowercases it too.
    let mime = c.mime.to_ascii_lowercase();
    c.length != 0
        && (c.url.starts_with("urn:text:") || mime.contains("html") || mime.contains("pdf"))
}

/// Index one WACZ a page at a time, from its CDX.
///
/// A page's search document draws on up to three sources that arrive apart:
/// its HTML response record; a `urn:text:` record of Browsertrix's rendered
/// text; and a line of `pages.jsonl`/`extraPages.jsonl`, with rendered text and
/// a title. Rendered text, when there is any, wins over the body scraped from
/// the HTML, and the page lists' last line for a page wins over a `urn:text:`
/// record. Tantivy writes a document once, so a page's sources have to meet
/// before it is written, and this is how they meet without holding the crawl:
///
/// 1. The page lists are streamed once to note which pages have text there,
///    keeping the URL, where the page's last line is, and the first title and
///    timestamp the lines give, but not the text.
/// 2. The CDX's page records are grouped by page (a `urn:text:` record joins
///    its page) and each page's records are fetched and merged together, one
///    page per worker. A page with no page-list text is written there and then.
///    One with page-list text keeps only its metadata, since its body would
///    lose anyway.
/// 3. The page lists are streamed again, and each page's last line is merged
///    with that metadata and written.
///
/// So memory holds the CDX, the listed URLs, the waiting pages' metadata
/// (capped per field, see [`cap`]), and a page per worker: never the crawl's
/// text, which for a large crawl is gigabytes and for a hostile one is
/// whatever it asks for.
fn index_pages<F, L, R>(source: PageSource<'_, F, L>, run: &Run) -> Result<CrawlStats>
where
    F: RangeFetch + Clone + Send + Sync,
    L: Fn() -> Result<zip::ZipArchive<R>>,
    R: std::io::Read + std::io::Seek,
{
    use crate::wacz::{self, PageTextPos};
    use std::sync::atomic::{AtomicU64, Ordering};
    let PageSource {
        fetch,
        cdx,
        starts,
        open_lists,
    } = source;
    let Run {
        docs,
        label,
        concurrency,
        progress,
    } = *run;
    let limit = docs.read_limit;
    let started = std::time::Instant::now();

    // 1. Which pages the page lists give text for.
    #[derive(Default)]
    struct Listed {
        last: PageTextPos,
        title: Option<String>,
        timestamp: String,
    }
    // The page lists are read twice. A remote WACZ's are copied out once
    // first, so the second read is from disk rather than a second download of
    // what is usually the crawl's largest member.
    let spooled = if fetch.is_remote() {
        Some(wacz::spool_page_lists(&mut open_lists()?, limit)?)
    } else {
        None
    };
    let each_page_text =
        |f: &mut dyn FnMut(PageTextPos, wacz::PageText) -> Result<()>| match &spooled {
            Some(files) => wacz::for_each_spooled_page_text(files, f),
            None => wacz::for_each_page_text(&mut open_lists()?, limit, f),
        };
    let mut listed: HashMap<String, Listed> = HashMap::new();
    each_page_text(&mut |pos, page| {
        let e = listed.entry(page.url).or_default();
        e.last = pos;
        if e.title.is_none() {
            e.title = page.title.map(|t| cap(t, TITLE_CAP));
        }
        if e.timestamp.is_empty() {
            e.timestamp = page.ts;
        }
        Ok(())
    })?;

    // 2. The CDX's page records, grouped by page in CDX order, which within a
    //    page is capture order, so a later capture's fields win as they should.
    let wanted: Vec<&crate::wacz::CdxjRecord> = cdx.iter().filter(|c| is_page_record(c)).collect();
    progress.set_total(wanted.len() as u64);
    let mut slot: HashMap<&str, usize> = HashMap::new();
    let mut groups: Vec<(&str, Vec<&crate::wacz::CdxjRecord>)> = Vec::new();
    for c in &wanted {
        let page = c.url.strip_prefix("urn:text:").unwrap_or(&c.url);
        match slot.get(page) {
            Some(&i) => groups[i].1.push(c),
            None => {
                slot.insert(page, groups.len());
                groups.push((page, vec![c]));
            }
        }
    }

    let writer = PageWriter::new(docs);
    let waiting: Mutex<HashMap<String, MergedPage>> = Mutex::new(HashMap::new());
    // Taken by a worker whose record grows past SMALL_RECORD, and held until
    // that page is written or set aside; see records_from_slice_gated.
    let large = Mutex::new(());
    let done = AtomicU64::new(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(concurrency)
        .build()
        .context("building record-fetch thread pool")?;
    pool.install(|| {
        groups
            .par_iter()
            .try_for_each(|(page, records)| -> Result<()> {
                let mut merged = MergedPage::default();
                let mut held = None;
                // One gzip member can hold several records, each with its own CDX
                // line at the member's offset; fetch it once.
                let mut fetched = std::collections::HashSet::new();
                for c in records {
                    if fetched.insert((c.filename.as_str(), c.offset)) {
                        let gate = Gate {
                            lock: &large,
                            held: &mut held,
                        };
                        for raw in fetch_raw_records(&fetch, starts, c, limit, gate) {
                            if belongs_to(raw.url(), page, &slot) {
                                merge_raw(&mut merged, raw);
                            }
                        }
                    }
                    progress.set_records(done.fetch_add(1, Ordering::Relaxed) + 1);
                }
                if listed.contains_key(*page) {
                    merged.html_body = None;
                    merged.rendered_text = None;
                    waiting.lock().unwrap().insert(page.to_string(), merged);
                    Ok(())
                } else {
                    writer.write(page, merged)
                }
            })
    })?;

    // 3. The page lists again: each page's last line, with its waiting metadata.
    progress.phase("building index");
    let mut waiting = waiting.into_inner().unwrap();
    let mut completed = 0usize;
    each_page_text(&mut |pos, page| {
        let Some(l) = listed.get(&page.url) else {
            return Ok(());
        };
        if l.last != pos {
            return Ok(());
        }
        let mut merged = waiting.remove(&page.url).unwrap_or_default();
        if merged.timestamp.is_empty() {
            merged.timestamp = l.timestamp.clone();
        }
        merged.rendered_text = Some(page.text);
        // Rendered text always comes from an HTML page.
        merged.media_type.get_or_insert_with(|| "html".to_string());
        // A page-list title fills in only when the HTML capture gave none.
        if let Some(t) = &l.title {
            merged.title.get_or_insert_with(|| t.clone());
        }
        completed += 1;
        writer.write(&page.url, merged)
    })?;
    // The second pass must reach every page the first one listed. It reads the
    // same bytes, so the only way it falls short is a read that failed this
    // time, a remote range after its retries, say; and then the pages it did
    // not reach would be written without their text, or not at all, under a
    // crawl that reported success. Refuse instead, so a retry can finish it.
    if completed != listed.len() {
        anyhow::bail!(
            "the page lists could not be read in full a second time ({completed} of {} \
             pages); nothing from this crawl was committed, so it can be retried",
            listed.len()
        );
    }
    debug_assert!(waiting.is_empty(), "every waiting page is a listed one");

    debug!(
        records = wanted.len(),
        pages = groups.len(),
        listed = listed.len(),
        ms = started.elapsed().as_millis() as u64,
        concurrency,
        "indexed pages a page at a time"
    );
    let stats = writer.finish();
    info!(pages = stats.pages, wacz = %label, "indexed pages from WACZ");
    Ok(stats)
}

/// Whether a record fetched for `page` is that page's to merge. A gzip member
/// can hold another page's record, which that page's own group merges; any
/// other record is this page's, even when its WARC URI is spelled differently
/// from the CDX's key (a different indexer's normalisation, say), which would
/// otherwise drop it unseen.
fn belongs_to(url: &str, page: &str, pages: &HashMap<&str, usize>) -> bool {
    url == page || !pages.contains_key(url)
}

/// What [`index_pages`] reads: the WACZ's bytes, the CDX that says where its
/// records are, each WARC's offset in those bytes, and a way to open the zip
/// again to stream its page lists.
struct PageSource<'a, F, L> {
    fetch: F,
    cdx: &'a [crate::wacz::CdxjRecord],
    starts: &'a HashMap<String, u64>,
    open_lists: L,
}

/// How [`index_pages`] runs: where the documents go, what to call the WACZ in
/// logs, how many records to fetch at once, and where to report progress.
#[derive(Clone, Copy)]
struct Run<'a> {
    docs: &'a Docs<'a>,
    label: &'a str,
    concurrency: usize,
    progress: &'a dyn IndexProgress,
}

/// The most of a page's HTML parsed for its text. See [`record_to_raw`].
const HTML_TEXT_CAP: usize = 16 * 1024 * 1024;

/// How far a record inflates before its worker has to take the shared lock.
/// Nearly every page is far smaller, so records are read in parallel; one that
/// is not waits its turn, so the workers together hold at most this much each
/// plus one record of up to the limit, rather than the limit each.
const SMALL_RECORD: u64 = 16 * 1024 * 1024;

/// The shared lock a worker takes for a large record, and its hold on it,
/// kept until the worker's page is written or set aside.
struct Gate<'a, 'g> {
    lock: &'g Mutex<()>,
    held: &'a mut Option<std::sync::MutexGuard<'g, ()>>,
}

/// Fetch one CDX record and turn what it holds into page contributions. A
/// record that cannot be read, including one that inflates past the limit, is
/// skipped with a warning: one bad record should not cost the crawl.
fn fetch_raw_records<F: RangeFetch>(
    fetch: &F,
    starts: &HashMap<String, u64>,
    c: &crate::wacz::CdxjRecord,
    limit: u64,
    gate: Gate<'_, '_>,
) -> Vec<RawRecord> {
    let base = c.filename.rsplit('/').next().unwrap_or(&c.filename);
    let Some(&start) = starts.get(base) else {
        return Vec::new();
    };
    let (from, len) = (start + c.offset, c.length);
    // The CDX says how long the record is, and the fetch allocates that much
    // before reading, so a length past the limit is refused unfetched.
    if len > limit {
        tracing::warn!(url = %c.url, "skipping a CDX record longer than the limit ({len} bytes)");
        return Vec::new();
    }
    // A record already large as fetched waits its turn before the fetch, not
    // after it: the bytes themselves are what would be held at once.
    if len > SMALL_RECORD && gate.held.is_none() {
        *gate.held = Some(gate.lock.lock().unwrap_or_else(|e| e.into_inner()));
    }
    match fetch
        .fetch(from, from + len)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| {
            crate::wacz::records_from_slice_gated(
                &bytes,
                from,
                len,
                limit,
                SMALL_RECORD,
                gate.lock,
                gate.held,
            )
        }) {
        Ok(records) => records.iter().filter_map(record_to_raw).collect(),
        Err(e) => {
            tracing::warn!(url = %c.url, "skipping unreadable CDX record: {e:#}");
            Vec::new()
        }
    }
}

/// The most of each metadata field a page keeps. A page waits in memory with
/// its metadata until its page-list text arrives, and a hostile page can carry
/// a gigabyte of headings, so each field is capped well above anything a real
/// page has. The body is not capped: it is the page's text, and is indexed in
/// full.
const TITLE_CAP: usize = 2 * 1024;
const DESCRIPTION_CAP: usize = 8 * 1024;
const HEADINGS_CAP: usize = 64 * 1024;
const KEYWORDS_CAP: usize = 4 * 1024;
const AUTHOR_CAP: usize = 1024;
const LANG_CAP: usize = 64;

/// `s`, cut to at most `max` bytes on a character boundary.
fn cap(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// Fold one record's contribution into its page. Later contributions win,
/// field by field, except where noted: the caller feeds them in capture order.
fn merge_raw(e: &mut MergedPage, raw: RawRecord) {
    match raw {
        RawRecord::Html {
            url: _,
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
            // The HTML capture is the authoritative timestamp for replay.
            e.timestamp = timestamp;
            if !title.is_empty() {
                e.title = Some(cap(title, TITLE_CAP));
            }
            if !body.is_empty() {
                e.html_body = Some(body);
            }
            if !description.is_empty() {
                e.description = Some(cap(description, DESCRIPTION_CAP));
            }
            if !headings.is_empty() {
                e.headings = Some(cap(headings, HEADINGS_CAP));
            }
            if !keywords.is_empty() {
                e.keywords = Some(cap(keywords, KEYWORDS_CAP));
            }
            if !author.is_empty() {
                e.author = Some(cap(author, AUTHOR_CAP));
            }
            if !media_type.is_empty() {
                e.media_type = Some(media_type);
            }
            if !lang.is_empty() {
                e.lang = Some(cap(lang, LANG_CAP));
            }
            if status.is_some() {
                e.status = status;
            }
            if modified_year.is_some() {
                e.modified_year = modified_year;
            }
        }
        RawRecord::Text {
            url: _,
            timestamp,
            text,
            title,
        } => {
            if e.timestamp.is_empty() {
                e.timestamp = timestamp;
            }
            e.rendered_text = Some(text);
            // Rendered text always comes from an HTML page.
            e.media_type.get_or_insert_with(|| "html".to_string());
            if let Some(t) = title.filter(|t| !t.is_empty()) {
                e.title.get_or_insert_with(|| cap(t, TITLE_CAP));
            }
        }
    }
}

/// Writes merged pages into the search index, shared by the workers, and
/// keeps the crawl's page count and capture range as it goes.
struct PageWriter<'a> {
    docs: &'a Docs<'a>,
    stats: Mutex<CrawlStats>,
}

impl<'a> PageWriter<'a> {
    fn new(docs: &'a Docs<'a>) -> Self {
        Self {
            docs,
            stats: Mutex::new(CrawlStats::default()),
        }
    }

    fn write(&self, url: &str, m: MergedPage) -> Result<()> {
        use crate::search::Page;
        // Prefer the fully rendered text; fall back to scraped HTML.
        let body = m.rendered_text.or(m.html_body).unwrap_or_default();
        let title = m.title.unwrap_or_default();
        let description = m.description.unwrap_or_default();
        if title.is_empty() && body.is_empty() && description.is_empty() {
            return Ok(());
        }
        self.docs.search.lock().unwrap().index_page(&Page {
            url,
            timestamp: &m.timestamp,
            title: &title,
            body: &body,
            description: &description,
            headings: m.headings.as_deref().unwrap_or_default(),
            keywords: m.keywords.as_deref().unwrap_or_default(),
            author: m.author.as_deref().unwrap_or_default(),
            media_type: m.media_type.as_deref().unwrap_or_default(),
            lang: m.lang.as_deref().unwrap_or_default(),
            status: m.status,
            modified_year: m.modified_year,
            crawl_id: self.docs.crawl_id,
            crawl_name: self.docs.crawl_name,
            collection: self.docs.collection,
        })?;
        let mut stats = self.stats.lock().unwrap();
        stats.pages += 1;
        // 14-digit timestamps sort chronologically as plain strings.
        if !m.timestamp.is_empty() {
            merge_min(&mut stats.earliest_capture, Some(m.timestamp.clone()));
            merge_max(&mut stats.latest_capture, Some(m.timestamp));
        }
        Ok(())
    }

    fn finish(self) -> CrawlStats {
        self.stats.into_inner().unwrap()
    }
}

/// Several files read as one [`RangeFetch`], each at its own base offset, so
/// the CDX built for a WACZ's extracted WARCs can address all of them. A
/// record never spans two files.
#[derive(Clone)]
struct ConcatFetch {
    files: std::sync::Arc<Vec<(u64, crate::http_range::FileFetch)>>,
    total: u64,
}

impl ConcatFetch {
    /// The fetch over `files`, and each file's base offset by name, which is
    /// what [`fetch_raw_records`] adds a record's offset to.
    fn open(files: &[(String, tempfile::NamedTempFile)]) -> Result<(Self, HashMap<String, u64>)> {
        let mut parts = Vec::new();
        let mut starts = HashMap::new();
        let mut base = 0u64;
        for (name, tmp) in files {
            let f = crate::http_range::FileFetch::open(tmp.path())?;
            starts.insert(name.clone(), base);
            let len = f.total_len();
            parts.push((base, f));
            base += len;
        }
        Ok((
            Self {
                files: std::sync::Arc::new(parts),
                total: base,
            },
            starts,
        ))
    }
}

impl RangeFetch for ConcatFetch {
    fn total_len(&self) -> u64 {
        self.total
    }

    fn fetch(&self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        let Some((base, file)) = self.files.iter().rev().find(|(base, _)| *base <= start) else {
            return Err(std::io::Error::other("offset before the first file"));
        };
        if end - base > file.total_len() {
            return Err(std::io::Error::other("a record past the end of its file"));
        }
        file.fetch(start - base, end - base)
    }
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
    read_limit: u64,
) where
    F: crate::http_range::RangeFetch + Clone + Send + Sync,
{
    let Some(url) = main_page_url else {
        return;
    };
    match crate::thumbnail::generate(fetch, thumbs_dir, crawl_id, url, pinned_dest, read_limit) {
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

impl RawRecord {
    /// The page this contributes to.
    fn url(&self) -> &str {
        match self {
            RawRecord::Html { url, .. } | RawRecord::Text { url, .. } => url,
        }
    }
}

/// Transform one WARC record into an indexable [`RawRecord`], or `None` if it is
/// not a page: warcinfo, `dns:`, other `urn:` pseudo-records (pageinfo,
/// thumbnail, …), non-HTML/PDF responses, or empty payloads. Both extraction
/// paths come through here, via [`index_pages`], so both index identically.
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
    // Parsed into a DOM to find the text, which costs many times the page's
    // size: measured, a 100 MiB page of paragraphs took 2.3 GB to read. So no
    // more than HTML_TEXT_CAP of a page is parsed; a real page is far smaller,
    // and text past it in one that is not is simply not searchable.
    let html = &record.payload[..record.payload.len().min(HTML_TEXT_CAP)];
    let html = extract_html_text(html);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fetch that must not be called.
    #[derive(Clone)]
    struct NoFetch;
    impl RangeFetch for NoFetch {
        fn total_len(&self) -> u64 {
            u64::MAX
        }
        fn fetch(&self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
            panic!("fetched {start}..{end}, which should have been refused");
        }
    }

    /// The CDX's length is allocated before reading, so one past the limit is
    /// refused without fetching anything.
    #[test]
    fn a_cdx_length_past_the_limit_is_not_fetched() {
        let starts = HashMap::from([("w.warc.gz".to_string(), 0)]);
        let c = crate::wacz::CdxjRecord {
            url: "https://ex.test/".into(),
            timestamp: String::new(),
            mime: "text/html".into(),
            status: 200,
            filename: "w.warc.gz".into(),
            offset: 0,
            length: 100 * 1024 * 1024 * 1024,
        };
        let lock = Mutex::new(());
        let mut held = None;
        let gate = Gate {
            lock: &lock,
            held: &mut held,
        };
        assert!(fetch_raw_records(&NoFetch, &starts, &c, 1024 * 1024, gate).is_empty());
    }

    #[test]
    fn a_content_types_case_does_not_matter() {
        let record = |mime: &str| crate::wacz::CdxjRecord {
            url: "https://ex.test/".into(),
            timestamp: String::new(),
            mime: mime.into(),
            status: 200,
            filename: "w".into(),
            offset: 0,
            length: 10,
        };
        for mime in ["text/html", "Text/HTML; charset=UTF-8", "APPLICATION/PDF"] {
            assert!(is_page_record(&record(mime)), "{mime}");
        }
        assert!(!is_page_record(&record("image/png")));
    }

    #[test]
    fn a_record_belongs_to_its_page_unless_another_page_claims_it() {
        let pages = HashMap::from([("https://ex.test/a", 0), ("https://ex.test/b", 1)]);
        assert!(belongs_to("https://ex.test/a", "https://ex.test/a", &pages));
        assert!(
            !belongs_to("https://ex.test/b", "https://ex.test/a", &pages),
            "another page's record, fetched in the same member, is that page's"
        );
        assert!(
            belongs_to("https://ex.test/a/", "https://ex.test/a", &pages),
            "a URI spelled differently from the CDX key is still this page's"
        );
    }

    /// A record already large as fetched is fetched with the lock held, so
    /// the workers cannot each download one at once.
    #[test]
    fn a_long_record_is_fetched_with_the_lock_held() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct LockedFetch<'a> {
            lock: &'a Mutex<()>,
            held_during_fetch: &'a AtomicBool,
        }
        impl RangeFetch for LockedFetch<'_> {
            fn total_len(&self) -> u64 {
                u64::MAX
            }
            fn fetch(&self, _: u64, _: u64) -> std::io::Result<Vec<u8>> {
                self.held_during_fetch
                    .store(self.lock.try_lock().is_err(), Ordering::SeqCst);
                Ok(Vec::new())
            }
        }
        let lock = Mutex::new(());
        let held_during_fetch = AtomicBool::new(false);
        let fetch = LockedFetch {
            lock: &lock,
            held_during_fetch: &held_during_fetch,
        };
        let starts = HashMap::from([("w".to_string(), 0)]);
        let c = crate::wacz::CdxjRecord {
            url: "https://ex.test/".into(),
            timestamp: String::new(),
            mime: "text/html".into(),
            status: 200,
            filename: "w".into(),
            offset: 0,
            length: SMALL_RECORD + 1,
        };
        let mut held = None;
        let gate = Gate {
            lock: &lock,
            held: &mut held,
        };
        fetch_raw_records(&fetch, &starts, &c, u64::MAX, gate);
        assert!(held_during_fetch.load(Ordering::SeqCst));
    }

    /// The second pass over the page lists must reach every page the first
    /// listed; if a read falls short the second time, the crawl is refused
    /// rather than written with pages missing their text.
    #[test]
    fn a_second_pass_that_falls_short_is_refused() {
        use std::io::Write;
        let lists = |lines: &[&str]| {
            let mut buf = Vec::new();
            {
                let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
                zw.start_file(
                    "pages/extraPages.jsonl",
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
                for line in lines {
                    writeln!(zw, "{line}").unwrap();
                }
                zw.finish().unwrap();
            }
            buf
        };
        let full = lists(&[
            r#"{"url":"https://ex.test/a","text":"aaa"}"#,
            r#"{"url":"https://ex.test/b","text":"bbb"}"#,
        ]);
        let short = lists(&[r#"{"url":"https://ex.test/a","text":"aaa"}"#]);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let open_lists = || -> Result<_> {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let bytes = if n == 0 { full.clone() } else { short.clone() };
            Ok(zip::ZipArchive::new(std::io::Cursor::new(bytes))?)
        };
        let tmp = tempfile::TempDir::new().unwrap();
        let search = Mutex::new(SearchIndex::open(tmp.path()).unwrap());
        let docs = Docs {
            crawl_id: "c",
            crawl_name: "c",
            collection: "c",
            search: &search,
            read_limit: crate::wacz::DEFAULT_MEMBER_LIMIT,
        };
        let err = index_pages(
            PageSource {
                fetch: NoFetch,
                cdx: &[],
                starts: &HashMap::new(),
                open_lists,
            },
            &Run {
                docs: &docs,
                label: "t",
                concurrency: 1,
                progress: crate::index::no_progress(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("1 of 2"), "{err}");
    }

    /// Extracted WARCs read as one address space: each at its base, and no
    /// record across two files.
    #[test]
    fn concat_fetch_reads_each_file_at_its_base() {
        use std::io::Write;
        let file = |bytes: &[u8]| {
            let mut f = tempfile::NamedTempFile::new().unwrap();
            f.write_all(bytes).unwrap();
            f
        };
        let files = vec![
            ("one".to_string(), file(b"abc")),
            ("two".to_string(), file(b"defgh")),
        ];
        let (fetch, starts) = ConcatFetch::open(&files).unwrap();
        assert_eq!(fetch.total_len(), 8);
        assert_eq!(starts["one"], 0);
        assert_eq!(starts["two"], 3);
        assert_eq!(fetch.fetch(0, 3).unwrap(), b"abc");
        assert_eq!(fetch.fetch(4, 6).unwrap(), b"ef");
        assert!(
            fetch.fetch(2, 5).is_err(),
            "a span across two files is refused"
        );
    }
}
