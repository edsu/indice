use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::collections::SeedPage;

/// The most indice inflates of one WACZ member when nothing says otherwise:
/// 1 GiB, set by `index.max_member_mb` in `config.yaml`.
///
/// A WACZ is a zip, and a zip entry can inflate to a thousand times its
/// stored size, so a WACZ of a few megabytes can name gigabytes of
/// `pages.jsonl`, CDX or record. Every member indice reads is read through
/// [`Limited`], so a hostile WACZ can make indice inflate at most this much of
/// any one member rather than whatever it asks for. Using what was inflated
/// takes a small multiple of it (a page-list line is held, then parsed into
/// its text), so the memory a member can cost is a few times this, not more.
/// A genuine crawl's largest member, usually `extraPages.jsonl` with the
/// rendered text of every page, fits well inside.
pub const DEFAULT_MEMBER_LIMIT: u64 = 1024 * 1024 * 1024;

/// A WACZ member that inflates past the limit: the refusal, with what to do
/// about it if the archive is genuine.
#[derive(Debug)]
pub struct MemberTooLarge {
    pub member: String,
    pub limit: u64,
}

impl std::fmt::Display for MemberTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} in this WACZ inflates past {} MiB, the most indice reads of one member. \
             If the archive is genuine, raise index.max_member_mb in config.yaml.",
            self.member,
            self.limit / (1024 * 1024)
        )
    }
}

impl std::error::Error for MemberTooLarge {}

/// A reader that yields at most `limit` bytes of `inner`, and fails with
/// [`MemberTooLarge`] if there are more, rather than stopping quietly: a
/// truncated CDX or page list would index as if it were whole.
///
/// The check is on what is actually read, so it holds whatever size a zip
/// header claims; [`check_declared`] refuses an honest one sooner.
pub(crate) struct Limited<R> {
    inner: R,
    remaining: u64,
    member: String,
    limit: u64,
}

impl<R: Read> Limited<R> {
    pub(crate) fn new(inner: R, limit: u64, member: &str) -> Self {
        Self {
            inner,
            remaining: limit,
            member: member.to_string(),
            limit,
        }
    }
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            // At the limit: one more byte means the member is over it.
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(std::io::Error::other(MemberTooLarge {
                    member: self.member.clone(),
                    limit: self.limit,
                })),
            };
        }
        let want = buf
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Refuse a zip entry whose header already says it inflates past `limit`,
/// before any of it is read.
fn check_declared(entry: &zip::read::ZipFile<'_, impl Read>, limit: u64) -> Result<()> {
    if entry.size() > limit {
        return Err(MemberTooLarge {
            member: entry.name()?.to_string(),
            limit,
        }
        .into());
    }
    Ok(())
}

/// An I/O error as an `anyhow` error, with a [`MemberTooLarge`] inside it
/// lifted to the top, so callers can tell a refusal from a broken file with
/// `downcast_ref`.
pub(crate) fn lift(e: std::io::Error) -> anyhow::Error {
    if e.get_ref()
        .is_some_and(|inner| inner.downcast_ref::<MemberTooLarge>().is_some())
    {
        let inner = e.into_inner().expect("checked above");
        return anyhow::Error::from(*inner.downcast::<MemberTooLarge>().expect("checked above"));
    }
    anyhow::Error::from(e)
}

/// Whether an error is the member limit refusing, which callers that skip a
/// member they cannot read must still pass on.
pub(crate) fn is_too_large(e: &anyhow::Error) -> bool {
    e.downcast_ref::<MemberTooLarge>().is_some()
}

/// Metadata extracted from a WACZ file's `datapackage.json` and `pages/pages.jsonl`.
#[derive(Debug, Default)]
pub struct WaczMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub created: Option<String>,
    /// Last-modified time of the WACZ, if recorded (WACZ 1.1.1 `modified`).
    pub modified: Option<String>,
    /// Tool that produced the WACZ (WACZ 1.1.1 `software`).
    pub software: Option<String>,
    /// The collection's main page URL, if declared (WACZ 1.1.1 `mainPageUrl`).
    pub main_page_url: Option<String>,
    /// Topical keywords (Frictionless Data Package `keywords`).
    pub keywords: Vec<String>,
    /// License labels (Frictionless Data Package `licenses`; title/name/path).
    pub licenses: Vec<String>,
    /// Collecting org/person, if declared (Frictionless `contributors` titles,
    /// else the top-level `organization`).
    pub creator: Option<String>,
    pub seed_pages: Vec<SeedPage>,
}

/// A single record from a WACZ CDX index (`indexes/index.cdx[.gz]`).
#[derive(Debug)]
pub struct CdxjRecord {
    pub url: String,
    pub timestamp: String,
    pub mime: String,
    pub status: u16,
    pub filename: String,
    pub offset: u64,
    pub length: u64,
}

/// Read `datapackage.json` and `pages/pages.jsonl` from a WACZ file and return
/// collected metadata.  Missing or unrecognised fields are silently ignored so
/// that the function works with minimal / non-standard WACZ files.
pub fn read_datapackage(wacz_path: &Path, limit: u64) -> Result<WaczMetadata> {
    let file = std::fs::File::open(wacz_path)
        .with_context(|| format!("opening WACZ {}", wacz_path.display()))?;
    read_datapackage_from(file, limit)
}

/// Read `datapackage.json` + `pages/pages.jsonl` metadata from any `Read + Seek`
/// WACZ — a local file, or an HTTP range reader for streaming indexing.
///
/// Each member is read through [`Limited`] at `limit` bytes; past it this is a
/// [`MemberTooLarge`] error, while a member that is merely malformed is skipped.
pub(crate) fn read_datapackage_from<R: std::io::Read + std::io::Seek>(
    reader: R,
    limit: u64,
) -> Result<WaczMetadata> {
    let mut zip = zip::ZipArchive::new(reader).context("reading WACZ ZIP")?;

    // --- datapackage.json -------------------------------------------------
    // Descriptive fields live at the top level in some WACZs and under a
    // nested `metadata` object in others (e.g. Browsertrix). Read both, with
    // the top level taking precedence.
    #[derive(Deserialize, Default)]
    struct Metadata {
        title: Option<String>,
        description: Option<String>,
        created: Option<String>,
        modified: Option<String>,
        software: Option<String>,
        #[serde(rename = "mainPageUrl")]
        main_page_url: Option<String>,
        /// Epoch-millisecond modification time; a fallback for `created`.
        mtime: Option<i64>,
    }
    /// A Frictionless `licenses` entry — any of the fields may be absent.
    #[derive(Deserialize, Default)]
    struct License {
        name: Option<String>,
        title: Option<String>,
        path: Option<String>,
    }
    /// A Frictionless `contributors` entry (`title` is the display name).
    #[derive(Deserialize, Default)]
    struct Contributor {
        title: Option<String>,
        organization: Option<String>,
    }
    #[derive(Deserialize, Default)]
    struct DataPackage {
        title: Option<String>,
        description: Option<String>,
        created: Option<String>,
        modified: Option<String>,
        software: Option<String>,
        #[serde(rename = "mainPageUrl")]
        main_page_url: Option<String>,
        /// Frictionless Data Package extension fields (WACZ spec-blessed).
        #[serde(default)]
        keywords: Vec<String>,
        #[serde(default)]
        licenses: Vec<License>,
        #[serde(default)]
        contributors: Vec<Contributor>,
        organization: Option<String>,
        #[serde(default)]
        metadata: Option<Metadata>,
    }

    let mut meta = WaczMetadata::default();

    if let Ok(entry) = zip.by_name("datapackage.json") {
        check_declared(&entry, limit)?;
        let mut buf = String::new();
        Limited::new(entry, limit, "datapackage.json")
            .read_to_string(&mut buf)
            .map_err(lift)?;
        if let Ok(dp) = serde_json::from_str::<DataPackage>(&buf) {
            let nested = dp.metadata.unwrap_or_default();
            meta.title = clean(dp.title.or(nested.title));
            meta.description = clean(dp.description.or(nested.description));
            meta.created = dp
                .created
                .or(nested.created)
                .or_else(|| nested.mtime.and_then(millis_to_rfc3339));
            meta.modified = clean(dp.modified.or(nested.modified));
            meta.software = clean(dp.software.or(nested.software));
            meta.main_page_url = clean(dp.main_page_url.or(nested.main_page_url));
            meta.keywords = dp
                .keywords
                .into_iter()
                .filter_map(|k| clean(Some(k)))
                .collect();
            // Prefer a license's title, then its SPDX name, then its path/URL.
            meta.licenses = dp
                .licenses
                .into_iter()
                .filter_map(|l| clean(l.title.or(l.name).or(l.path)))
                .collect();
            // Creator: the top-level `organization`, else the contributors'
            // titles (or their organizations), joined.
            meta.creator = clean(dp.organization).or_else(|| {
                let names: Vec<String> = dp
                    .contributors
                    .into_iter()
                    .filter_map(|c| clean(c.title.or(c.organization)))
                    .collect();
                (!names.is_empty()).then(|| names.join(", "))
            });
        }
    }

    // --- pages/pages.jsonl ------------------------------------------------
    // First line is a header object; subsequent lines are page entries. Read a
    // line at a time, so memory follows the pages kept, not the file's size.
    if let Ok(entry) = zip.by_name("pages/pages.jsonl") {
        check_declared(&entry, limit)?;
        let lines = std::io::BufRead::lines(std::io::BufReader::new(Limited::new(
            entry,
            limit,
            "pages/pages.jsonl",
        )));
        for line in lines.skip(1) {
            let line = line.map_err(lift)?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            #[derive(Deserialize)]
            struct PageEntry {
                url: Option<String>,
                title: Option<String>,
                ts: Option<String>,
            }
            if let Ok(p) = serde_json::from_str::<PageEntry>(line) {
                if let Some(url) = p.url {
                    meta.seed_pages.push(SeedPage {
                        url,
                        title: p.title,
                        ts: p.ts.unwrap_or_default(),
                    });
                }
            }
        }
    }

    Ok(meta)
}

/// One page's extracted text from a WACZ's `pages/*.jsonl` files.
///
/// Browsertrix records the fully rendered (post-JS) page text here - in
/// `pages/pages.jsonl` (seed pages) and `pages/extraPages.jsonl` (pages found
/// while crawling) - as a `text` field. Older crawls store the rendered text
/// *only* here (not as `urn:text:` WARC records), so indexing must read it or
/// JS-rendered content is unsearchable even though it shows up in replay.
#[derive(Debug)]
pub(crate) struct PageText {
    pub url: String,
    pub ts: String,
    pub title: Option<String>,
    pub text: String,
}

/// Read extracted page text from a WACZ's `pages/pages.jsonl` and
/// `pages/extraPages.jsonl`, all of it, in order. See [`for_each_page_text`],
/// which this collects; indexing streams it instead.
#[cfg(test)]
pub(crate) fn read_page_texts<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    limit: u64,
) -> Result<Vec<PageText>> {
    let mut out = Vec::new();
    for_each_page_text(zip, limit, |_, text| {
        out.push(text);
        Ok(())
    })?;
    Ok(out)
}

/// Where a page-list line sits: which file (`0` for `pages.jsonl`, `1` for
/// `extraPages.jsonl`) and which line of it. Two passes over the same WACZ
/// see the same positions, which is how indexing finds a page's line again.
pub(crate) type PageTextPos = (u8, u64);

/// Call `f` with every page's extracted text from `pages/pages.jsonl` and
/// `pages/extraPages.jsonl`, a line at a time, in file order. Each file's
/// header line ({format,id,title}) carries no url/text and is skipped
/// naturally, so no fixed header row is assumed. Entries without a URL or with
/// empty text are dropped; missing files are not an error, and the rest of an
/// unreadable one (not UTF-8, a broken entry) is skipped. A file inflating past
/// `limit` is an error, [`MemberTooLarge`], since skipping it would index the
/// crawl without its text and say nothing.
pub(crate) fn for_each_page_text<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    limit: u64,
    mut f: impl FnMut(PageTextPos, PageText) -> Result<()>,
) -> Result<()> {
    for (file, name) in PAGE_LISTS.into_iter().enumerate() {
        let Ok(entry) = zip.by_name(name) else {
            continue;
        };
        check_declared(&entry, limit)?;
        page_lines(file as u8, Limited::new(entry, limit, name), &mut f)?;
    }
    Ok(())
}

/// The page lists, in the order their positions number them.
const PAGE_LISTS: [&str; 2] = ["pages/pages.jsonl", "pages/extraPages.jsonl"];

/// The page lists copied out of the WACZ to local temp files, each read
/// through [`Limited`] at `limit`. For a remote WACZ, which indexing reads the
/// page lists of twice: the copy turns the second download into a disk read.
pub(crate) fn spool_page_lists<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    limit: u64,
) -> Result<Vec<(u8, tempfile::NamedTempFile)>> {
    let mut out = Vec::new();
    for (file, name) in PAGE_LISTS.into_iter().enumerate() {
        let Ok(entry) = zip.by_name(name) else {
            continue;
        };
        check_declared(&entry, limit)?;
        let mut tmp = tempfile::NamedTempFile::new().context("temp for a page list")?;
        std::io::copy(&mut Limited::new(entry, limit, name), tmp.as_file_mut())
            .map_err(lift)
            .with_context(|| format!("copying {name}"))?;
        out.push((file as u8, tmp));
    }
    Ok(out)
}

/// [`for_each_page_text`] over page lists already copied out by
/// [`spool_page_lists`], with the same positions.
pub(crate) fn for_each_spooled_page_text(
    files: &[(u8, tempfile::NamedTempFile)],
    mut f: impl FnMut(PageTextPos, PageText) -> Result<()>,
) -> Result<()> {
    for (file, tmp) in files {
        page_lines(*file, std::fs::File::open(tmp.path())?, &mut f)?;
    }
    Ok(())
}

/// The page texts in one page list, a line at a time.
fn page_lines(
    file: u8,
    reader: impl std::io::Read,
    f: &mut impl FnMut(PageTextPos, PageText) -> Result<()>,
) -> Result<()> {
    #[derive(Deserialize)]
    struct Entry {
        url: Option<String>,
        title: Option<String>,
        ts: Option<String>,
        text: Option<String>,
    }
    let lines = std::io::BufRead::lines(std::io::BufReader::new(reader));
    for (n, line) in lines.enumerate() {
        let line = match line.map_err(lift) {
            Ok(line) => line,
            Err(e) if is_too_large(&e) => return Err(e),
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(e) = serde_json::from_str::<Entry>(line) else {
            continue;
        };
        let (Some(url), Some(text)) = (e.url, e.text) else {
            continue;
        };
        // Trimmed in place rather than copied, since the line and its text
        // are already two copies of what may be a large page.
        let text = trim_owned(text);
        if text.is_empty() {
            continue;
        }
        f(
            (file, n as u64),
            PageText {
                url,
                ts: e.ts.unwrap_or_default(),
                title: clean(e.title),
                text,
            },
        )?;
    }
    Ok(())
}

/// `s` with surrounding whitespace removed, reusing its allocation.
fn trim_owned(mut s: String) -> String {
    let end = s.trim_end().len();
    s.truncate(end);
    let start = s.len() - s.trim_start().len();
    s.drain(..start);
    s
}

/// Trim a metadata string and drop it if empty.
fn clean(s: Option<String>) -> Option<String> {
    s.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// Convert epoch milliseconds to an RFC 3339 timestamp string.
fn millis_to_rfc3339(ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|dt| dt.to_rfc3339())
}

/// Search a WACZ file's CDX index for records matching `url` (exact URL match).
///
/// Handles both:
/// - CDXJ format: `{surt} {timestamp} {json}`
/// - NDJSON format: `{json}` where the JSON object has a `"url"` field
///
/// The index file may be compressed (`.cdx.gz`) or plain (`.cdx`).
pub fn search_cdx(wacz_path: &Path, url: &str, limit: u64) -> Result<Vec<CdxjRecord>> {
    let file = std::fs::File::open(wacz_path)
        .with_context(|| format!("opening WACZ {}", wacz_path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("reading ZIP {}", wacz_path.display()))?;
    let mut results = Vec::new();
    for_each_cdx_record(&mut zip, limit, |rec| {
        if rec.url == url {
            results.push(rec);
        }
    })?;
    Ok(results)
}

/// Call `f` with every record in the WACZ's CDX, read a line at a time.
///
/// The CDX can be compressed twice over: deflated as a zip entry, and gzipped
/// within it (often as many gzip members, ZipNum-style, so `MultiGzDecoder`
/// reads them all). Both layers are read through [`Limited`] at `limit`, so a
/// CDX that inflates past it at either one is a [`MemberTooLarge`] error, and
/// memory follows the records kept rather than the CDX's size.
fn for_each_cdx_record<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    limit: u64,
    mut f: impl FnMut(CdxjRecord),
) -> Result<()> {
    let Some(cdx_name) = find_cdx_entry(zip)? else {
        return Ok(());
    };
    let entry = zip.by_name(&cdx_name)?;
    check_declared(&entry, limit)?;
    let stored = Limited::new(entry, limit, &cdx_name);
    let text: Box<dyn Read> = if cdx_name.ends_with(".gz") {
        Box::new(Limited::new(
            flate2::read::MultiGzDecoder::new(stored),
            limit,
            &cdx_name,
        ))
    } else {
        Box::new(stored)
    };
    // Split on raw bytes, not UTF-8 lines, so one bad byte in an otherwise
    // good CDX costs its line rather than the rest of the file.
    for line in std::io::BufRead::split(std::io::BufReader::new(text), b'\n') {
        let line = line.map_err(lift)?;
        let line = String::from_utf8_lossy(&line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rec) = parse_cdx_line(line) {
            f(rec);
        }
    }
    Ok(())
}

fn find_cdx_entry<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<Option<String>> {
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        let name = entry.name()?.to_string();
        // The index holds CDXJ data; the extension varies by tool: compressed
        // `.cdx.gz`/`.cdxj.gz` or plain `.cdx`/`.cdxj`. Match any of them.
        if name.starts_with("indexes/")
            && (name.ends_with(".cdx.gz")
                || name.ends_with(".cdxj.gz")
                || name.ends_with(".cdx")
                || name.ends_with(".cdxj"))
        {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// The basename of a slash-separated path (`archive/x.warc.gz` -> `x.warc.gz`).
fn basename(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Whether a ZIP entry name is an embedded WARC (`archive/*.warc` or `.warc.gz`).
fn is_warc_entry(name: &str) -> bool {
    name.starts_with("archive/") && (name.ends_with(".warc.gz") || name.ends_with(".warc"))
}

/// The inner `.wacz` entry names of a **nested multi-WACZ** — a WACZ whose
/// payload is other WACZ files rather than `archive/` WARCs. This is what
/// Browsertrix's combined collection `/download` returns for a collection with
/// more than one crawl. (Verified against a real download: the inner `.wacz`
/// files are top-level, `Stored` entries, and `datapackage.json` carries
/// `profile: "multi-wacz-package"` listing them.)
///
/// Nesting is a Webrecorder/Browsertrix **convention, not part of the WACZ
/// spec**, so we detect it *structurally* — `.wacz` entries present **and no**
/// `archive/` WARCs — rather than trusting the non-standard `profile` string.
/// This is robust to the exact layout (the inner files needn't be under `data/`)
/// and also catches multi-WACZs from other tools. Returns empty for an ordinary
/// (flat) WACZ, so one that merely bundles a `.wacz` alongside its WARCs isn't
/// misread.
pub(crate) fn nested_wacz_locations<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Vec<NestedWacz> {
    let mut waczs = Vec::new();
    let mut has_warc = false;
    for i in 0..zip.len() {
        let Ok(f) = zip.by_index(i) else { continue };
        let Ok(name) = f.name().map(String::from) else {
            continue;
        };
        if is_warc_entry(&name) {
            has_warc = true;
        } else if name.to_ascii_lowercase().ends_with(".wacz") {
            // A Stored inner WACZ is a contiguous, uncompressed byte window of the
            // outer file — record where it starts and how long it is so it can be
            // range-read in place (no extraction). A deflated one (`inline: None`)
            // has to be materialized to read it.
            let inline = if f.compression() == zip::CompressionMethod::Stored {
                f.data_start().map(|start| (start, f.size()))
            } else {
                None
            };
            waczs.push(NestedWacz { name, inline });
        }
    }
    if has_warc {
        Vec::new()
    } else {
        waczs
    }
}

/// An inner `.wacz` of a nested multi-WACZ. `inline` is `Some((data_start, len))`
/// when the entry is Stored and can be read in place as a window of the outer
/// file; `None` when it's compressed and must be extracted first.
pub(crate) struct NestedWacz {
    pub name: String,
    pub inline: Option<(u64, u64)>,
}

/// Read and parse **all** CDX records from a WACZ ZIP (any `Read + Seek`), for
/// CDX-guided/streaming indexing. Unlike [`search_cdx`], no URL filter.
pub(crate) fn cdx_records<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    limit: u64,
) -> Result<Vec<CdxjRecord>> {
    let mut records = Vec::new();
    for_each_cdx_record(zip, limit, |rec| records.push(rec))?;
    Ok(records)
}

/// Error unless every embedded WARC is `Stored` (uncompressed). CDX-guided
/// streaming needs this: a CDX byte offset maps to an absolute WACZ position
/// only when the WARC isn't ZIP-compressed. Browsertrix / py-wacz store WARCs
/// uncompressed (they're already gzipped); some tools deflate them, and those
/// can't be streamed (fall back to scan / `--download`).
pub(crate) fn ensure_warcs_stored<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<()> {
    if warcs_stored(zip)? {
        Ok(())
    } else {
        anyhow::bail!(
            "WACZ stores its WARC entries compressed (deflate); CDX-guided \
             extraction needs uncompressed (stored) WARCs. indice falls back to \
             a full WARC scan for such files automatically."
        )
    }
}

/// Whether every embedded WARC is `Stored` (streamable). `false` if any is
/// compressed — used to decide whether a remote WACZ can be streamed or must be
/// downloaded.
pub(crate) fn warcs_stored<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<bool> {
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if is_warc_entry(&entry.name()?) && entry.compression() != zip::CompressionMethod::Stored {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Map each embedded WARC's basename to the absolute byte offset of its data
/// within the WACZ (`ZipFile::data_start`). Since WARCs are stored uncompressed,
/// a CDX record at `offset` lives at `data_start + offset` in the WACZ.
pub(crate) fn warc_data_starts<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<std::collections::HashMap<String, u64>> {
    let mut map = std::collections::HashMap::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        let name = entry.name()?.to_string();
        // `data_start()` is `Option<u64>` (zip 8+): `Some` once the local header
        // has been read, which `by_index` does. A WARC without a known data start
        // can't be seeked into, so skip it (its CDX records are then not fetched).
        if let (true, Some(start)) = (is_warc_entry(&name), entry.data_start()) {
            map.insert(basename(&name).to_string(), start);
        }
    }
    Ok(map)
}

/// Find the crawl's `warcinfo` by decoding the first gzip member of each
/// embedded WARC (warcinfo is the first record) and parsing it — so streaming
/// indexing gets provenance without reading whole WARCs. Returns the first
/// non-empty warcinfo found.
///
/// Best effort: a first member that inflates past `limit` is skipped like an
/// unreadable one, since warcinfo only adds provenance and a WARC without one
/// may well start with a large record.
pub(crate) fn find_warcinfo_streaming<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    limit: u64,
) -> Result<Option<crate::warc::Warcinfo>> {
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok()?.name().ok().map(String::from))
        .filter(|n| is_warc_entry(n))
        .collect();
    for name in names {
        let mut decoded = Vec::new();
        // read::GzDecoder decodes only the first member, which is enough.
        if Limited::new(
            flate2::read::GzDecoder::new(zip.by_name(&name)?),
            limit,
            &name,
        )
        .read_to_end(&mut decoded)
        .is_err()
            || decoded.is_empty()
        {
            continue;
        }
        for rec in crate::warc::parse_warc_records(&decoded, 0, 0)
            .into_iter()
            .flatten()
        {
            if let Some(info) = crate::warc::Warcinfo::from_record(&rec) {
                if !info.is_empty() {
                    return Ok(Some(info));
                }
            }
        }
    }
    Ok(None)
}

// WACZ CDX JSON quotes some numeric fields as strings (e.g. `"length":"715"`)
// and leaves others as numbers (`"length":715`), depending on the tool that
// wrote it. Deserialize the varying fields as `Value` and coerce below so a
// single quoted number doesn't cause the whole record to be dropped.
#[derive(Deserialize)]
struct CdxJson {
    url: Option<String>,
    ts: Option<String>,
    mime: Option<String>,
    status: Option<serde_json::Value>,
    filename: Option<String>,
    offset: Option<serde_json::Value>,
    length: Option<serde_json::Value>,
}

fn coerce_u64(v: &Option<serde_json::Value>) -> u64 {
    match v {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn parse_cdx_line(line: &str) -> Option<CdxjRecord> {
    let (ts, json_str) = if line.starts_with('{') {
        // NDJSON format: the whole line is the JSON object.
        ("", line)
    } else {
        // CDXJ format: "{surt} {timestamp} {json}"
        let mut parts = line.splitn(3, ' ');
        let _surt = parts.next()?;
        let ts = parts.next()?;
        let json = parts.next()?;
        (ts, json)
    };

    let obj: CdxJson = serde_json::from_str(json_str).ok()?;
    let url = obj.url?;
    let timestamp = if !ts.is_empty() {
        ts.to_string()
    } else {
        obj.ts.unwrap_or_default()
    };
    let status = match &obj.status {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0) as u16,
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    };

    Some(CdxjRecord {
        url,
        timestamp,
        mime: obj.mime.unwrap_or_default(),
        status,
        filename: obj.filename.unwrap_or_default(),
        offset: coerce_u64(&obj.offset),
        length: coerce_u64(&obj.length),
    })
}

/// Iterate over the paths of WARC archives embedded inside a WACZ file.
pub fn iter_warc_paths(wacz_path: &Path) -> Result<impl Iterator<Item = Result<String>>> {
    let file = std::fs::File::open(wacz_path)
        .with_context(|| format!("opening WACZ {}", wacz_path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("reading ZIP in {}", wacz_path.display()))?;

    let mut names: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        let name = entry.name()?.to_string();
        if name.starts_with("archive/") && (name.ends_with(".warc.gz") || name.ends_with(".warc")) {
            names.push(name);
        }
    }

    Ok(names.into_iter().map(Ok))
}

/// Extract a single named WARC entry from a WACZ ZIP into a temp file.
pub fn extract_warc_from_wacz(
    wacz_path: &Path,
    entry_name: &str,
) -> Result<tempfile::NamedTempFile> {
    use std::io::copy;

    let file = std::fs::File::open(wacz_path)
        .with_context(|| format!("opening WACZ {}", wacz_path.display()))?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entry = zip
        .by_name(entry_name)
        .with_context(|| format!("entry {} not found in {}", entry_name, wacz_path.display()))?;

    let suffix = if entry_name.ends_with(".warc.gz") {
        ".warc.gz"
    } else {
        ".warc"
    };
    let mut tmp = tempfile::Builder::new().suffix(suffix).tempfile()?;
    copy(&mut entry, &mut tmp)?;

    Ok(tmp)
}

/// Read a single WARC record located at absolute byte `offset` in a `Read + Seek`
/// WACZ, where the record is one gzip member of `length` bytes (both as given by
/// a CDX entry, translated to an absolute position via the WARC ZIP entry's
/// `data_start`). Gunzips just that slice and parses it, so CDX-guided/streaming
/// indexing can pull one record without reading the rest of the WARC. Returns
/// the record(s) in the member (usually one).
/// Gunzip a single WARC record slice (one gzip member of `length` bytes, located
/// at `offset` — both informational) and parse it. The CDX-guided extractor
/// fetches a record's byte range (a `data_start + offset` slice, possibly
/// concurrently via [`crate::http_range::RangeFetch`]) and hands the bytes here
/// to gunzip + parse without a `Read + Seek`. Returns the record(s) in the member
/// (usually one).
///
/// The slice is as long as the CDX says, but what it inflates to is not, so
/// it is read through [`Limited`] at `limit`.
pub(crate) fn records_from_slice(
    buf: &[u8],
    offset: u64,
    length: u64,
    limit: u64,
) -> Result<Vec<crate::warc::WarcRecord>> {
    use std::io::Read;
    // A record from a plain `.warc` is its own bytes; only a gzip member,
    // which is every record in a `.warc.gz`, needs inflating.
    if !buf.starts_with(&[0x1f, 0x8b]) {
        if buf.len() as u64 > limit {
            return Err(MemberTooLarge {
                member: "a WARC record".to_string(),
                limit,
            }
            .into());
        }
        return Ok(crate::warc::parse_warc_records(buf, offset, length)
            .into_iter()
            .filter_map(Result::ok)
            .collect());
    }
    let mut decompressed = Vec::new();
    Limited::new(flate2::read::GzDecoder::new(buf), limit, "a WARC record")
        .read_to_end(&mut decompressed)
        .map_err(lift)
        .context("decompressing WARC record slice")?;
    Ok(
        crate::warc::parse_warc_records(&decompressed, offset, length)
            .into_iter()
            .filter_map(Result::ok)
            .collect(),
    )
}

/// [`records_from_slice`] for many workers at once: the first `small` bytes of
/// a record inflate freely, and only past them does a worker take `gate`, so
/// any number of ordinary records can be read in parallel while only one at a
/// time can grow toward `limit`. The guard is left in `held`, for the caller to
/// keep for as long as it holds what it made of the record.
pub(crate) fn records_from_slice_gated<'g>(
    buf: &[u8],
    offset: u64,
    length: u64,
    limit: u64,
    small: u64,
    gate: &'g std::sync::Mutex<()>,
    held: &mut Option<std::sync::MutexGuard<'g, ()>>,
) -> Result<Vec<crate::warc::WarcRecord>> {
    use std::io::Read;
    // A plain record is already in hand, as long as its CDX length, which the
    // caller has checked against the limit; only a gzip member inflates.
    if !buf.starts_with(&[0x1f, 0x8b]) {
        if buf.len() as u64 > small && held.is_none() {
            *held = Some(gate.lock().unwrap_or_else(|e| e.into_inner()));
        }
        return records_from_slice(buf, offset, length, limit);
    }
    let mut inflate = Limited::new(flate2::read::GzDecoder::new(buf), limit, "a WARC record");
    let mut decompressed = Vec::new();
    (&mut inflate)
        .take(small)
        .read_to_end(&mut decompressed)
        .map_err(lift)
        .context("decompressing WARC record slice")?;
    if decompressed.len() as u64 == small {
        if held.is_none() {
            *held = Some(gate.lock().unwrap_or_else(|e| e.into_inner()));
        }
        inflate
            .read_to_end(&mut decompressed)
            .map_err(lift)
            .context("decompressing WARC record slice")?;
    }
    Ok(
        crate::warc::parse_warc_records(&decompressed, offset, length)
            .into_iter()
            .filter_map(Result::ok)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The member limit ────────────────────────────────────────────────────

    const MIB: u64 = 1024 * 1024;

    /// A zip in memory holding `(name, bytes, stored)` entries.
    fn zip_of(entries: &[(&str, &[u8], bool)]) -> zip::ZipArchive<std::io::Cursor<Vec<u8>>> {
        use std::io::Write;
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            for (name, bytes, stored) in entries {
                let method = if *stored {
                    zip::CompressionMethod::Stored
                } else {
                    zip::CompressionMethod::Deflated
                };
                let opt = zip::write::SimpleFileOptions::default()
                    .compression_method(method)
                    .large_file(bytes.len() as u64 > u32::MAX as u64);
                zw.start_file(*name, opt).unwrap();
                zw.write_all(bytes).unwrap();
            }
            zw.finish().unwrap();
        }
        zip::ZipArchive::new(std::io::Cursor::new(buf)).unwrap()
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    }

    /// Two MiB of page lines, which compress to almost nothing.
    fn two_mib_of_pages() -> Vec<u8> {
        let line =
            b"{\"url\":\"https://ex.org/p\",\"ts\":\"2026\",\"text\":\"xxxxxxxxxxxxxxxx\"}\n";
        line.repeat((2 * MIB as usize) / line.len() + 1)
    }

    /// The page lists copied out for a remote WACZ read exactly as they do
    /// from the zip: same pages, same positions.
    #[test]
    fn spooled_page_lists_read_as_the_zip_does() {
        let pages = b"{\"format\":\"x\"}\n{\"url\":\"https://ex.org/a\",\"text\":\"  a \"}\n";
        let extra = b"{\"url\":\"https://ex.org/b\",\"title\":\"B\",\"text\":\"b\"}\n";
        let collect = |via_spool: bool| {
            let mut zip = zip_of(&[
                ("pages/pages.jsonl", pages, false),
                ("pages/extraPages.jsonl", extra, false),
            ]);
            let mut out = Vec::new();
            let mut push = |pos, t: PageText| {
                out.push((pos, t.url, t.title, t.text));
                Ok(())
            };
            if via_spool {
                let files = spool_page_lists(&mut zip, MIB).unwrap();
                for_each_spooled_page_text(&files, &mut push).unwrap();
            } else {
                for_each_page_text(&mut zip, MIB, &mut push).unwrap();
            }
            out
        };
        let direct = collect(false);
        assert_eq!(direct.len(), 2);
        assert_eq!(direct[0].3, "a", "text is trimmed");
        assert_eq!(collect(true), direct);
    }

    #[test]
    fn limited_reads_up_to_its_limit_and_refuses_past_it() {
        let mut out = Vec::new();
        Limited::new(&b"0123456789"[..], 10, "m")
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, b"0123456789", "exactly the limit is fine");

        let err = Limited::new(&b"0123456789"[..], 9, "m")
            .read_to_end(&mut Vec::new())
            .map_err(lift)
            .unwrap_err();
        assert!(is_too_large(&err), "{err}");
        assert!(err.to_string().contains("index.max_member_mb"), "{err}");
    }

    /// A deflated pages file that says in its header it inflates past the
    /// limit is refused before any of it is read, by both readers of it.
    #[test]
    fn a_pages_bomb_is_refused() {
        let pages = two_mib_of_pages();
        let mut zip = zip_of(&[("pages/extraPages.jsonl", &pages, false)]);
        let err = read_page_texts(&mut zip, MIB).unwrap_err();
        assert!(is_too_large(&err), "{err}");
        assert!(err.to_string().contains("pages/extraPages.jsonl"), "{err}");

        let zip = zip_of(&[("pages/pages.jsonl", &pages, false)]);
        let err = read_datapackage_from(zip.into_inner(), MIB).unwrap_err();
        assert!(is_too_large(&err), "{err}");

        // Under the limit, the same file reads.
        let mut zip = zip_of(&[("pages/extraPages.jsonl", &pages, false)]);
        assert!(!read_page_texts(&mut zip, 4 * MIB).unwrap().is_empty());
    }

    /// A gzipped CDX inside a stored entry: the zip header knows only the
    /// gzip's size, which passes, so the limit has to hold while inflating.
    #[test]
    fn a_gzipped_cdx_bomb_is_refused_while_it_inflates() {
        let line = "com,example)/ 20200101000000 {\"url\":\"https://example.com/\",\"filename\":\"x.warc.gz\",\"offset\":0,\"length\":10}\n";
        let cdx = gzip(line.repeat((2 * MIB as usize) / line.len() + 1).as_bytes());
        assert!((cdx.len() as u64) < MIB, "the stored entry itself is small");
        let mut zip = zip_of(&[("indexes/index.cdx.gz", &cdx, true)]);
        let err = cdx_records(&mut zip, MIB).unwrap_err();
        assert!(is_too_large(&err), "{err}");

        let mut zip = zip_of(&[("indexes/index.cdx.gz", &cdx, true)]);
        assert!(!cdx_records(&mut zip, 4 * MIB).unwrap().is_empty());
    }

    /// Small records inflate without the shared lock; one that grows past
    /// `small` takes it, and keeps it for the caller.
    #[test]
    fn only_a_large_record_takes_the_shared_lock() {
        let gate = std::sync::Mutex::new(());
        let mut held = None;
        let small = gzip(&[b'x'; 1024]);
        records_from_slice_gated(&small, 0, small.len() as u64, MIB, 4096, &gate, &mut held)
            .unwrap();
        assert!(held.is_none(), "a small record leaves the lock alone");

        let large = gzip(&[b'x'; 8192]);
        records_from_slice_gated(&large, 0, large.len() as u64, MIB, 4096, &gate, &mut held)
            .unwrap();
        assert!(held.is_some(), "a large record holds the lock");
        assert!(gate.try_lock().is_err(), "and nobody else can take it");
        drop(held);
        assert!(gate.try_lock().is_ok());
    }

    #[test]
    fn a_record_that_inflates_past_the_limit_is_refused() {
        let slice = gzip(&vec![b'x'; 2 * MIB as usize]);
        let err = records_from_slice(&slice, 0, slice.len() as u64, MIB).unwrap_err();
        assert!(
            err.chain()
                .any(|c| c.downcast_ref::<MemberTooLarge>().is_some()),
            "{err:#}"
        );
    }

    /// warcinfo only adds provenance, so a first member past the limit is
    /// skipped, not fatal.
    #[test]
    fn an_oversized_first_warc_member_is_skipped_for_warcinfo() {
        let warc = gzip(&vec![b'x'; 2 * MIB as usize]);
        let mut zip = zip_of(&[("archive/data.warc.gz", &warc, true)]);
        assert!(find_warcinfo_streaming(&mut zip, MIB).unwrap().is_none());
    }

    #[test]
    fn cdx_records_reads_all_members_of_a_multimember_gzip() {
        use std::io::Write;
        // Two CDX lines, each in its OWN gzip member (ZipNum-style clustering).
        let line = |u: &str| {
            format!("com,example)/ 20200101000000 {{\"url\":\"{u}\",\"mime\":\"text/html\",\"filename\":\"x.warc.gz\",\"offset\":0,\"length\":10,\"status\":200}}\n")
        };
        let gz = |s: String| {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(s.as_bytes()).unwrap();
            e.finish().unwrap()
        };
        let mut cdx_gz = gz(line("https://example.com/a"));
        cdx_gz.extend_from_slice(&gz(line("https://example.com/b"))); // 2nd member appended

        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            zw.start_file(
                "indexes/index.cdx.gz",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zw.write_all(&cdx_gz).unwrap();
            zw.finish().unwrap();
        }
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(buf)).unwrap();
        let recs = cdx_records(&mut zip, DEFAULT_MEMBER_LIMIT).unwrap();
        let urls: Vec<&str> = recs.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(
            recs.len(),
            2,
            "must read every gzip member of the CDX, not just the first"
        );
        assert!(urls.contains(&"https://example.com/a") && urls.contains(&"https://example.com/b"));
    }

    #[test]
    fn cdx_records_recognizes_a_plain_cdxj_index() {
        use std::io::Write;
        // Some WACZs name the (uncompressed) index `indexes/index.cdxj`.
        let line = "com,example)/ 20200101000000 {\"url\":\"https://example.com/\",\"mime\":\"text/html\",\"filename\":\"x.warc.gz\",\"offset\":0,\"length\":10,\"status\":200}\n";
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            zw.start_file(
                "indexes/index.cdxj",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zw.write_all(line.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(buf)).unwrap();
        let recs = cdx_records(&mut zip, DEFAULT_MEMBER_LIMIT).unwrap();
        assert_eq!(recs.len(), 1, "a plain .cdxj index must be recognized");
        assert_eq!(recs[0].url, "https://example.com/");
    }

    #[test]
    fn read_page_texts_reads_pages_and_extra_pages() {
        use std::io::Write;
        // pages.jsonl (seed pages) + extraPages.jsonl (discovered pages). Header
        // lines, entries without text, and whitespace-only text are all skipped.
        let pages = "{\"format\":\"json-pages-1.0\",\"id\":\"pages\",\"title\":\"All Pages\"}\n\
             {\"id\":\"p1\",\"url\":\"https://ex.com/\",\"title\":\"Home\",\"ts\":\"2022-01-01T00:00:00Z\",\"text\":\"seed body Петиція\"}\n\
             {\"id\":\"p2\",\"url\":\"https://ex.com/no-text\",\"title\":\"NoText\"}\n";
        let extra = "{\"format\":\"json-pages-1.0\",\"id\":\"extraPages\",\"title\":\"Extra\"}\n\
             {\"id\":\"e1\",\"url\":\"https://ex.com/two\",\"ts\":\"2022-01-02T00:00:00Z\",\"text\":\"  discovered text  \"}\n\
             {\"id\":\"e2\",\"url\":\"https://ex.com/blank\",\"text\":\"   \"}\n";
        let mut buf = Vec::new();
        {
            let opt = zip::write::SimpleFileOptions::default();
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            zw.start_file("pages/pages.jsonl", opt).unwrap();
            zw.write_all(pages.as_bytes()).unwrap();
            zw.start_file("pages/extraPages.jsonl", opt).unwrap();
            zw.write_all(extra.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(buf)).unwrap();
        let texts = read_page_texts(&mut zip, DEFAULT_MEMBER_LIMIT).unwrap();
        assert_eq!(
            texts.len(),
            2,
            "only entries with non-empty text: {texts:?}"
        );
        let home = texts.iter().find(|p| p.url == "https://ex.com/").unwrap();
        assert!(home.text.contains("Петиція"), "reads text incl. Cyrillic");
        assert_eq!(home.title.as_deref(), Some("Home"));
        let two = texts
            .iter()
            .find(|p| p.url == "https://ex.com/two")
            .unwrap();
        assert_eq!(two.text, "discovered text", "text is trimmed");
        assert_eq!(two.ts, "2022-01-02T00:00:00Z");
        assert!(two.title.is_none(), "absent title stays None");
    }

    #[test]
    fn records_from_slice_reads_one_gzipped_warc_record() {
        use std::io::Write;
        // Build a WARC response record, gzip it as one member.
        let block = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html>hi</html>";
        let mut warc = format!(
            "WARC/1.0\r\nWARC-Type: response\r\nWARC-Target-URI: https://ex.com/p\r\n\
             Content-Type: application/http; msgtype=response\r\nContent-Length: {}\r\n\r\n",
            block.len()
        )
        .into_bytes();
        warc.extend_from_slice(block);
        warc.extend_from_slice(b"\r\n\r\n");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&warc).unwrap();
        let member = enc.finish().unwrap();

        // The extractor fetches exactly the record's byte slice, then gunzips +
        // parses it (offset/length are informational).
        let len = member.len() as u64;
        let recs = records_from_slice(&member, 64, len, DEFAULT_MEMBER_LIMIT).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].target_uri, "https://ex.com/p");
        assert_eq!(recs[0].http_status, Some(200));
    }

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(FIXTURES).join(name)
    }

    #[test]
    fn list_warc_paths_in_wacz() {
        let paths: Vec<_> = iter_warc_paths(&fixture("simple.wacz"))
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();

        assert!(!paths.is_empty(), "should find at least one WARC entry");
        assert!(
            paths.iter().any(|p| p.contains("archive/")),
            "entries should be under archive/: {paths:?}"
        );
    }

    #[test]
    fn extract_warc_from_wacz_succeeds() {
        let paths: Vec<_> = iter_warc_paths(&fixture("simple.wacz"))
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();

        let first = &paths[0];
        let tmp = extract_warc_from_wacz(&fixture("simple.wacz"), first).unwrap();
        assert!(tmp.path().exists());
        assert!(tmp.path().metadata().unwrap().len() > 0);
    }

    #[test]
    fn read_datapackage_from_simple_wacz() {
        let meta = read_datapackage(&fixture("simple.wacz"), DEFAULT_MEMBER_LIMIT).unwrap();
        // simple.wacz has a pages/pages.jsonl with one page entry
        assert!(
            !meta.seed_pages.is_empty(),
            "should have at least one seed page"
        );
        assert_eq!(meta.seed_pages[0].url, "http://example.com/");
        assert_eq!(meta.seed_pages[0].title.as_deref(), Some("Example"));
    }

    #[test]
    fn read_datapackage_reads_nested_metadata() {
        // github-bitcoin-mining.wacz has no top-level title/created; its title
        // lives under a nested `metadata` object, and the timestamp is mtime
        // (epoch ms). Both should be picked up.
        let meta =
            read_datapackage(&fixture("github-bitcoin-mining.wacz"), DEFAULT_MEMBER_LIMIT).unwrap();
        assert_eq!(meta.title.as_deref(), Some("GitHub Bitcoin Mining"));
        let created = meta.created.expect("created should fall back to mtime");
        assert!(
            created.starts_with("2021-04-17"),
            "created from mtime: {created}"
        );
    }

    #[test]
    fn search_cdx_finds_url_in_ndjson_wacz() {
        let records = search_cdx(
            &fixture("simple.wacz"),
            "http://example.com/",
            DEFAULT_MEMBER_LIMIT,
        )
        .unwrap();
        assert!(!records.is_empty(), "should find CDX entry for example.com");
        assert_eq!(records[0].url, "http://example.com/");
        assert_eq!(records[0].status, 200);
    }

    #[test]
    fn search_cdx_finds_url_in_cdxj_wacz() {
        // github-bitcoin-mining.wacz uses CDXJ format
        let records = search_cdx(
            &fixture("github-bitcoin-mining.wacz"),
            "https://github.com/DocNow/hydrator/pull/78/files",
            DEFAULT_MEMBER_LIMIT,
        )
        .unwrap();
        assert!(!records.is_empty(), "should find CDX entry");
    }

    #[test]
    fn search_cdx_handles_string_typed_numeric_fields() {
        // a.wacz stores offset/length/status as quoted strings in its CDX
        // (e.g. "length":"715"); make sure we still parse those records.
        let records = search_cdx(
            &fixture("a.wacz"),
            "https://storymaps.arcgis.com/stories/278e1b5c18a3474082e583e889705179",
            DEFAULT_MEMBER_LIMIT,
        )
        .unwrap();
        assert!(!records.is_empty(), "should parse string-typed CDX fields");
        assert_eq!(records[0].status, 200);
        assert!(records[0].length > 0, "length should coerce from string");
    }

    #[test]
    fn search_cdx_finds_redirect_record() {
        let records = search_cdx(
            &fixture("a.wacz"),
            "https://arcg.is/1zLCSC4",
            DEFAULT_MEMBER_LIMIT,
        )
        .unwrap();
        let redirect = records.iter().find(|r| r.status == 301);
        assert!(
            redirect.is_some(),
            "arcg.is shortener should be a 301 redirect"
        );
    }

    #[test]
    fn search_cdx_no_match() {
        let records = search_cdx(
            &fixture("simple.wacz"),
            "http://notexist.example/",
            DEFAULT_MEMBER_LIMIT,
        )
        .unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn read_datapackage_reads_software() {
        // a.wacz records the crawler in datapackage.json's `software` field.
        let meta = read_datapackage(&fixture("a.wacz"), DEFAULT_MEMBER_LIMIT).unwrap();
        let software = meta.software.expect("datapackage should carry software");
        assert!(
            software.contains("Browsertrix-Crawler"),
            "unexpected software: {software}"
        );
    }

    #[test]
    fn warcinfo_extracts_software_from_real_wacz() {
        // a.wacz was produced by Browsertrix-Crawler, which writes a warcinfo
        // record with a `software` field.
        let mut zip =
            zip::ZipArchive::new(std::fs::File::open(fixture("a.wacz")).unwrap()).unwrap();
        let info = find_warcinfo_streaming(&mut zip, DEFAULT_MEMBER_LIMIT)
            .unwrap()
            .expect("a.wacz has a warcinfo record");
        let software = info.software.expect("warcinfo should carry software");
        assert!(
            software.contains("Browsertrix-Crawler"),
            "unexpected software: {software}"
        );
    }

    #[test]
    fn warcinfo_is_none_when_absent() {
        // simple.wacz has no warcinfo record.
        let mut zip =
            zip::ZipArchive::new(std::fs::File::open(fixture("simple.wacz")).unwrap()).unwrap();
        assert!(find_warcinfo_streaming(&mut zip, DEFAULT_MEMBER_LIMIT)
            .unwrap()
            .is_none());
    }

    #[test]
    fn read_datapackage_reads_modified_keywords_and_licenses() {
        use std::io::Write;
        // A datapackage.json carrying the Frictionless extension fields the
        // finding-aid work newly reads (keywords/licenses) plus `modified`.
        let dp = r#"{
            "profile": "data-package",
            "title": "Test",
            "modified": "2026-05-01T00:00:00Z",
            "keywords": ["climate", "policy", ""],
            "licenses": [
                {"name": "CC-BY-4.0", "path": "https://creativecommons.org/licenses/by/4.0/"},
                {"title": "Custom terms"}
            ],
            "contributors": [{"title": "Jane Curator"}, {"organization": "Web Archive Co"}]
        }"#;
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            zw.start_file("datapackage.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            zw.write_all(dp.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        let meta = read_datapackage_from(std::io::Cursor::new(buf), DEFAULT_MEMBER_LIMIT).unwrap();
        assert_eq!(meta.modified.as_deref(), Some("2026-05-01T00:00:00Z"));
        // Blank keyword is dropped by `clean`.
        assert_eq!(meta.keywords, vec!["climate", "policy"]);
        // License label prefers title, else name.
        assert_eq!(meta.licenses, vec!["CC-BY-4.0", "Custom terms"]);
        // Creator from contributors' titles (falling back to their organization).
        assert_eq!(
            meta.creator.as_deref(),
            Some("Jane Curator, Web Archive Co")
        );
    }

    #[test]
    fn read_datapackage_creator_prefers_top_level_organization() {
        use std::io::Write;
        let dp = r#"{"profile":"data-package","title":"T","organization":"SUCHO",
            "contributors":[{"title":"Ignored"}]}"#;
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            zw.start_file("datapackage.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            zw.write_all(dp.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        let meta = read_datapackage_from(std::io::Cursor::new(buf), DEFAULT_MEMBER_LIMIT).unwrap();
        assert_eq!(meta.creator.as_deref(), Some("SUCHO"));
    }
}
