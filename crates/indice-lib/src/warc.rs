use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// A parsed WARC record with HTTP response fields extracted.
#[derive(Debug, Clone)]
pub struct WarcRecord {
    pub record_id: String,
    pub concurrent_to: Option<String>,
    pub target_uri: String,
    pub timestamp: String, // 14-digit: 20060102150405
    pub warc_type: String,
    pub http_status: Option<u16>,
    pub content_type: String,
    pub digest: String,
    pub payload: Vec<u8>, // HTTP response body (headers stripped for response records)
    pub http_headers: Vec<(String, String)>, // original HTTP response headers (response records only)
    pub offset: u64, // compressed byte offset in .warc.gz; file offset in .warc
    pub record_length: u64, // compressed member size for .warc.gz
}

/// Provenance fields parsed from a WARC `warcinfo` record's
/// `application/warc-fields` block. A `warcinfo` record describes how the WARCs
/// that follow it were produced, so it is the canonical place to learn which
/// tool made a capture, who ran it, and with what settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Warcinfo {
    /// Crawler software and version (e.g. `Browsertrix-Crawler 1.13.0`).
    pub software: Option<String>,
    /// Contact for the operator who ran the crawl.
    pub operator: Option<String>,
    /// The `User-Agent` the crawler sent (warc-fields `http-header-user-agent`).
    pub user_agent: Option<String>,
    /// The collection/crawl this WARC belongs to (warc-fields `isPartOf`).
    pub is_part_of: Option<String>,
    /// The spec the WARC conforms to (warc-fields `conformsTo`).
    pub conforms_to: Option<String>,
    /// How the crawler handled robots.txt.
    pub robots: Option<String>,
    /// Host the crawl ran on.
    pub hostname: Option<String>,
}

impl Warcinfo {
    /// Parse a `warcinfo` record. Returns `None` for any other record type.
    pub fn from_record(record: &WarcRecord) -> Option<Warcinfo> {
        if !record.warc_type.eq_ignore_ascii_case("warcinfo") {
            return None;
        }
        Some(Warcinfo::from_fields(&parse_warc_fields(&record.payload)))
    }

    /// Build from parsed `application/warc-fields` pairs (keys case-insensitive).
    pub fn from_fields(fields: &[(String, String)]) -> Warcinfo {
        let mut info = Warcinfo::default();
        for (key, value) in fields {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            let slot = match key.to_ascii_lowercase().as_str() {
                "software" => &mut info.software,
                "operator" => &mut info.operator,
                "http-header-user-agent" => &mut info.user_agent,
                "ispartof" => &mut info.is_part_of,
                "conformsto" => &mut info.conforms_to,
                "robots" => &mut info.robots,
                "hostname" => &mut info.hostname,
                _ => continue,
            };
            // First occurrence wins; ignore later duplicates.
            slot.get_or_insert_with(|| value.to_string());
        }
        info
    }

    /// Whether no recognized provenance field was found.
    pub fn is_empty(&self) -> bool {
        *self == Warcinfo::default()
    }
}

/// Parse an `application/warc-fields` block into `(key, value)` pairs. The
/// format is one `Name: value` per line (a small subset of message headers);
/// blank lines and lines without a colon are skipped.
pub fn parse_warc_fields(bytes: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(colon) = line.find(':') {
            let key = line[..colon].trim();
            let value = line[colon + 1..].trim();
            if !key.is_empty() {
                out.push((key.to_string(), value.to_string()));
            }
        }
    }
    out
}

/// Parse all WARC records from a decompressed WARC byte buffer (one gzip
/// member's worth, or any plain-WARC slice). Used by CDX-guided/streaming
/// indexing, which gunzips a single record slice located via the CDX offset and
/// parses it without touching the rest of the WARC. `offset`/`record_length`
/// are informational (carried onto each record for provenance).
pub(crate) fn parse_warc_records(
    data: &[u8],
    offset: u64,
    record_length: u64,
) -> Vec<Result<WarcRecord>> {
    let mut out = Vec::new();
    parse_all_warc_records_from(data, offset, record_length, &mut out);
    out
}

/// Iterate over records in a `.warc` or `.warc.gz` file, one at a time.
///
/// For `.warc.gz`, each gzip member is read individually to track the
/// compressed byte offset of every record. For plain `.warc`, offsets are
/// tracked via an in-band counting wrapper.
///
/// Streaming, so memory holds one record (or one gzip member's records) at a
/// time, never the WARC: a deflated WARC can be gigabytes of images and video.
/// No record is read past `limit` bytes, inflated. A record over it, a gzip
/// member that is a decompression bomb or a plain record whose header claims
/// an absurd length, is yielded as a [`crate::wacz::MemberTooLarge`] error and
/// passed over without being held, and the records after it follow as usual:
/// one oversized record, such as a video, does not end the WARC.
pub fn iter_records(path: &Path, limit: u64) -> Result<impl Iterator<Item = Result<WarcRecord>>> {
    let gzip = is_gzip(path)?;
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    Ok(if gzip {
        Records::Gz {
            reader: Some(reader),
            queue: std::collections::VecDeque::new(),
            limit,
        }
    } else {
        Records::Plain {
            counting: Some(CountingBufReader::new(reader)),
            limit,
        }
    })
}

/// The state of [`iter_records`]: the reader, `None` once it has ended or
/// failed, and for gzip the records of a member not yet handed out.
enum Records {
    Gz {
        reader: Option<BufReader<std::fs::File>>,
        queue: std::collections::VecDeque<Result<WarcRecord>>,
        limit: u64,
    },
    Plain {
        counting: Option<CountingBufReader<std::fs::File>>,
        limit: u64,
    },
}

impl Iterator for Records {
    type Item = Result<WarcRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Records::Gz {
                reader,
                queue,
                limit,
            } => loop {
                if let Some(record) = queue.pop_front() {
                    return Some(record);
                }
                match next_gzip_member(reader.take()?, *limit) {
                    Ok(Some(member)) => {
                        *reader = Some(member.reader);
                        queue.extend(member.records);
                    }
                    Ok(None) => return None,
                    Err(e) => return Some(Err(e)),
                }
            },
            Records::Plain { counting, limit } => loop {
                let reader = counting.as_mut()?;
                let offset = reader.pos();
                match reader.inner.fill_buf() {
                    Ok([]) => {
                        *counting = None;
                        return None;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        *counting = None;
                        return Some(Err(e.into()));
                    }
                }
                match read_one_warc_bytes(reader, *limit) {
                    // Over the limit, and skipped past: report it, and go on.
                    Err(e) if crate::wacz::is_too_large(&e) => return Some(Err(e)),
                    Ok(bytes) => {
                        let record_length = reader.pos() - offset;
                        let cursor = BufReader::new(Cursor::new(bytes));
                        match parse_one_warc_record(cursor, offset, record_length) {
                            Ok(Some(rec)) => return Some(Ok(rec)),
                            Ok(None) => continue,
                            Err(e) => return Some(Err(e)),
                        }
                    }
                    Err(e) => {
                        *counting = None;
                        return Some(Err(e));
                    }
                }
            },
        }
    }
}

/// One gzip member's records, and the reader, positioned at the next member.
struct GzMember {
    reader: BufReader<std::fs::File>,
    records: Vec<Result<WarcRecord>>,
}

/// Read the next gzip member of a `.warc.gz` and parse the records in it,
/// handing the reader back for the next one. `None` at the end of the file,
/// or at a byte that cannot start a gzip member.
fn next_gzip_member(mut reader: BufReader<std::fs::File>, limit: u64) -> Result<Option<GzMember>> {
    use std::io::Seek;
    {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(None);
        }
        // Only bail on a byte that is definitively not the gzip magic 0x1f.
        // fill_buf() may return just 1 byte (0x1f) after an into_inner() +
        // stream_position() pair discards the buffer; in that case buf.get(1)
        // would be None, falsely triggering a two-byte check and stopping
        // records short. Let GzDecoder validate the full header instead.
        if buf[0] != 0x1f {
            tracing::debug!("stopping at non-gzip byte 0x{:02x}", buf[0]);
            return Ok(None);
        }
    }
    // BufReader::stream_position() returns the logical consumed position (OS
    // file pos minus buffered-but-not-consumed bytes), so use it directly.
    let offset = reader.stream_position()?;
    // bufread::GzDecoder takes ownership of the BufReader and returns it via
    // into_inner(), positioned right after the member's compressed footer, so
    // the next member starts cleanly. read::GzDecoder wraps an extra BufReader
    // internally and over-reads, losing the start of the next member.
    let mut gz = flate2::bufread::GzDecoder::new(reader);
    let mut decompressed = Vec::new();
    let read = crate::wacz::Limited::new(&mut gz, limit, "a WARC record")
        .read_to_end(&mut decompressed)
        .map_err(crate::wacz::lift);
    if let Err(e) = read {
        if !crate::wacz::is_too_large(&e) {
            return Err(e.context("decompressing gzip member"));
        }
        // Over the limit: inflate the rest of the member into nothing, which
        // costs time but no memory, so the next member can be read.
        drop(decompressed);
        std::io::copy(&mut gz, &mut std::io::sink()).context("skipping a gzip member")?;
        return Ok(Some(GzMember {
            reader: gz.into_inner(),
            records: vec![Err(e)],
        }));
    }
    let mut reader = gz.into_inner();
    let record_length = reader.stream_position()? - offset;
    let mut records = Vec::new();
    // A single gzip member may contain multiple concatenated WARC records.
    parse_all_warc_records_from(&decompressed, offset, record_length, &mut records);
    Ok(Some(GzMember { reader, records }))
}

/// Detect gzip content by magic bytes rather than file extension.
/// Some WACZ files name their WARC entries `.warc.gz` but store plain WARC data.
fn is_gzip(path: &Path) -> Result<bool> {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut magic = [0u8; 2];
    match file.read_exact(&mut magic) {
        Ok(_) => Ok(magic == [0x1f, 0x8b]),
        Err(_) => Ok(false),
    }
}

// ── gzip (one member per record) ─────────────────────────────────────────────

/// Extract all WARC records from a decompressed byte buffer.
///
/// All records in the same gzip member share the same `offset` and
/// `record_length` (the compressed member bounds).
fn parse_all_warc_records_from(
    data: &[u8],
    offset: u64,
    record_length: u64,
    out: &mut Vec<Result<WarcRecord>>,
) {
    let mut cursor = BufReader::new(Cursor::new(data));
    loop {
        // Peek: are there more bytes?
        if cursor.fill_buf().map(|b| b.is_empty()).unwrap_or(true) {
            break;
        }
        match parse_one_warc_record(&mut cursor, offset, record_length) {
            Ok(Some(rec)) => out.push(Ok(rec)),
            Ok(None) => break,
            Err(e) => {
                out.push(Err(e));
                break;
            }
        }
    }
}

// ── plain (uncompressed) ──────────────────────────────────────────────────────

/// Read exactly one WARC record (header + block + trailing `\r\n\r\n`) from
/// `r`, returning the raw bytes. The block is allocated from the header's
/// `Content-Length`, which the file chooses, so a length past `limit` is
/// refused before anything is allocated.
fn read_one_warc_bytes<R: BufRead>(r: &mut R, limit: u64) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut content_length: Option<usize> = None;
    // The header is read a line at a time, and a line is as long as the file
    // says, so the header gets the limit too: past it the record cannot be
    // stepped over (its end is unknown), and the WARC ends with an error.
    let mut header_budget = limit;
    let mut read_line = |r: &mut R, line: &mut String| -> Result<usize> {
        let n = r
            .by_ref()
            .take(header_budget.saturating_add(1))
            .read_line(line)?;
        if n as u64 > header_budget {
            return Err(anyhow!("a WARC header runs past the {limit}-byte limit"));
        }
        header_budget -= n as u64;
        Ok(n)
    };

    // Read the WARC/1.0 first line.
    let mut first = String::new();
    let n = read_line(r, &mut first)?;
    if n == 0 {
        return Err(anyhow!("unexpected EOF"));
    }
    if !first.trim_end().starts_with("WARC/") {
        return Err(anyhow!(
            "expected WARC/ version line, got: {}",
            first.trim_end()
        ));
    }
    buf.extend_from_slice(first.as_bytes());

    // Read header lines until blank line.
    loop {
        let mut line = String::new();
        let n = read_line(r, &mut line)?;
        if n == 0 {
            return Err(anyhow!("unexpected EOF reading WARC header"));
        }
        buf.extend_from_slice(line.as_bytes());

        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("content-length:") {
            let val = trimmed["content-length:".len()..].trim();
            content_length = val.parse::<usize>().ok();
        }
    }

    let content_length = content_length.unwrap_or(0);
    if content_length as u64 > limit {
        // Skip the block without holding it, so the next record can be read.
        std::io::copy(
            &mut r.by_ref().take(content_length as u64 + 4),
            &mut std::io::sink(),
        )?;
        return Err(crate::wacz::MemberTooLarge {
            member: "a WARC record".to_string(),
            limit,
        }
        .into());
    }
    // Read block + trailing \r\n\r\n.
    let mut block = vec![0u8; content_length + 4];
    r.read_exact(&mut block)
        .with_context(|| "reading WARC block")?;
    buf.extend_from_slice(&block);

    Ok(buf)
}

// ── WARC record parser ────────────────────────────────────────────────────────

fn parse_one_warc_record<R: BufRead>(
    mut r: R,
    offset: u64,
    record_length: u64,
) -> Result<Option<WarcRecord>> {
    // Skip any blank / whitespace-only lines before the WARC version line.
    let first_line = loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        if !line.trim().is_empty() {
            break line;
        }
    };
    if !first_line.trim_end().starts_with("WARC/") {
        return Ok(None);
    }

    let mut warc_type = String::new();
    let mut target_uri = String::new();
    let mut date = String::new();
    let mut record_id = String::new();
    let mut concurrent_to: Option<String> = None;
    let mut digest = String::new();
    let mut content_length: usize = 0;
    let mut content_type_warc = String::new();

    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("warc-type:") {
            warc_type = trimmed["warc-type:".len()..].trim().to_string();
        } else if lower.starts_with("warc-target-uri:") {
            let raw = trimmed["warc-target-uri:".len()..].trim();
            // wget (and some other tools) wrap the target URI in angle brackets
            // (`WARC-Target-URI: <https://…>`), an older WARC convention. Strip a
            // single surrounding pair so the URL — and the SURT/CDX key derived
            // from it — is the bare URI, matching warcio/pywb.
            let raw = raw
                .strip_prefix('<')
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or(raw);
            target_uri = raw.to_string();
        } else if lower.starts_with("warc-date:") {
            date = trimmed["warc-date:".len()..].trim().to_string();
        } else if lower.starts_with("warc-record-id:") {
            record_id = trimmed["warc-record-id:".len()..].trim().to_string();
        } else if lower.starts_with("warc-concurrent-to:") {
            concurrent_to = Some(trimmed["warc-concurrent-to:".len()..].trim().to_string());
        } else if lower.starts_with("warc-payload-digest:") {
            digest = trimmed["warc-payload-digest:".len()..].trim().to_string();
        } else if lower.starts_with("warc-block-digest:") && digest.is_empty() {
            digest = trimmed["warc-block-digest:".len()..].trim().to_string();
        } else if lower.starts_with("content-length:") {
            content_length = trimmed["content-length:".len()..]
                .trim()
                .parse()
                .unwrap_or(0);
        } else if lower.starts_with("content-type:") {
            content_type_warc = trimmed["content-type:".len()..].trim().to_string();
        }
    }

    let timestamp = iso_to_14digit(&date);

    let mut block = vec![0u8; content_length];
    r.read_exact(&mut block).unwrap_or(());

    // Consume the mandatory trailing \r\n\r\n that follows every WARC block.
    let mut trailing = [0u8; 4];
    let _ = r.read_exact(&mut trailing);

    let (http_status, http_headers, content_type, payload) = if warc_type
        .eq_ignore_ascii_case("response")
        && content_type_warc
            .to_ascii_lowercase()
            .contains("application/http")
    {
        parse_http_response(&block)
    } else {
        (None, Vec::new(), content_type_warc, block)
    };

    Ok(Some(WarcRecord {
        record_id,
        concurrent_to,
        target_uri,
        timestamp,
        warc_type,
        http_status,
        content_type,
        digest,
        payload,
        http_headers,
        offset,
        record_length,
    }))
}

/// Convert an ISO 8601 WARC date (`2006-01-02T15:04:05Z`) to a 14-digit
/// CDX timestamp (`20060102150405`).
fn iso_to_14digit(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).take(14).collect()
}

/// Parsed parts of an HTTP/1.x response: `(status, all_headers, content-type, body)`.
type HttpResponseParts = (Option<u16>, Vec<(String, String)>, String, Vec<u8>);

/// Parse raw HTTP/1.x response bytes.
fn parse_http_response(bytes: &[u8]) -> HttpResponseParts {
    let sep_crnl = b"\r\n\r\n";
    let sep_nl = b"\n\n";

    let (header_bytes, body) = if let Some(pos) = find_bytes(bytes, sep_crnl) {
        (&bytes[..pos], bytes[pos + 4..].to_vec())
    } else if let Some(pos) = find_bytes(bytes, sep_nl) {
        (&bytes[..pos], bytes[pos + 2..].to_vec())
    } else {
        (bytes, Vec::new())
    };

    let header_str = String::from_utf8_lossy(header_bytes);
    let mut lines = header_str.lines();

    // Status line: HTTP/1.1 200 OK
    let status = lines.next().and_then(|l| {
        let mut parts = l.splitn(3, ' ');
        parts.next(); // HTTP/1.1
        parts.next()?.parse::<u16>().ok()
    });

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut content_type = String::new();
    for line in lines {
        if let Some(colon) = line.find(':') {
            let name = line[..colon].trim().to_string();
            let value = line[colon + 1..].trim().to_string();
            let lower = name.to_ascii_lowercase();
            if lower == "content-type" {
                content_type = value.clone();
            }
            headers.push((name, value));
        }
    }

    (status, headers, content_type, body)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ── CountingBufReader ─────────────────────────────────────────────────────────

struct CountingBufReader<R: Read> {
    inner: BufReader<R>,
    count: u64,
}

impl<R: Read> CountingBufReader<R> {
    fn new(inner: BufReader<R>) -> Self {
        Self { inner, count: 0 }
    }

    fn pos(&self) -> u64 {
        self.count
    }
}

impl<R: Read> Read for CountingBufReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        Ok(n)
    }
}

impl<R: Read> BufRead for CountingBufReader<R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        self.inner.consume(amt);
        self.count += amt as u64;
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A header line is as long as the file says, so it is bounded too: one
    /// past the limit ends the WARC with an error instead of growing a string
    /// to the size of the file.
    #[test]
    fn a_header_past_the_limit_ends_the_warc() {
        let mut warc = b"WARC/1.0\r\nX-Long: ".to_vec();
        warc.extend(std::iter::repeat_n(b'a', 4096));
        warc.extend_from_slice(b"\r\n\r\n");
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &warc).unwrap();
        let mut records = iter_records(tmp.path(), 1024).unwrap();
        let err = records.next().unwrap().unwrap_err();
        assert!(err.to_string().contains("header runs past"), "{err}");
        assert!(records.next().is_none(), "and the WARC ends there");
    }

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(FIXTURES).join(name)
    }

    #[test]
    fn parse_warc_fields_splits_key_value_lines() {
        let block =
            b"software: Browsertrix-Crawler 1.13.0\nformat: WARC File Format 1.1\n\nrobots: obey\n";
        let fields = parse_warc_fields(block);
        assert_eq!(
            fields[0],
            (
                "software".to_string(),
                "Browsertrix-Crawler 1.13.0".to_string()
            )
        );
        assert!(fields.iter().any(|(k, v)| k == "robots" && v == "obey"));
        // Blank lines are skipped.
        assert!(fields.iter().all(|(k, _)| !k.is_empty()));
    }

    #[test]
    fn warcinfo_from_fields_maps_known_keys() {
        let fields = vec![
            (
                "software".to_string(),
                "Browsertrix-Crawler 1.13.0".to_string(),
            ),
            ("operator".to_string(), "crawls@example.org".to_string()),
            (
                "http-header-user-agent".to_string(),
                "Mozilla/5.0 (compatible)".to_string(),
            ),
            ("isPartOf".to_string(), "my-collection".to_string()),
            (
                "conformsTo".to_string(),
                "https://iipc.github.io/warc-specifications/".to_string(),
            ),
            ("robots".to_string(), "ignore".to_string()),
            ("hostname".to_string(), "crawler-01".to_string()),
            ("format".to_string(), "WARC File Format 1.1".to_string()), // unrecognized -> ignored
        ];
        let info = Warcinfo::from_fields(&fields);
        assert_eq!(info.software.as_deref(), Some("Browsertrix-Crawler 1.13.0"));
        assert_eq!(info.operator.as_deref(), Some("crawls@example.org"));
        assert_eq!(info.user_agent.as_deref(), Some("Mozilla/5.0 (compatible)"));
        assert_eq!(info.is_part_of.as_deref(), Some("my-collection"));
        assert_eq!(
            info.conforms_to.as_deref(),
            Some("https://iipc.github.io/warc-specifications/")
        );
        assert_eq!(info.robots.as_deref(), Some("ignore"));
        assert_eq!(info.hostname.as_deref(), Some("crawler-01"));
        assert!(!info.is_empty());
    }

    #[test]
    fn warcinfo_from_record_only_for_warcinfo_type() {
        // A warcinfo record yields parsed fields.
        let mut rec = WarcRecord {
            record_id: String::new(),
            concurrent_to: None,
            target_uri: String::new(),
            timestamp: String::new(),
            warc_type: "warcinfo".to_string(),
            http_status: None,
            content_type: "application/warc-fields".to_string(),
            digest: String::new(),
            payload: b"software: wget/1.21\n".to_vec(),
            http_headers: Vec::new(),
            offset: 0,
            record_length: 0,
        };
        assert_eq!(
            Warcinfo::from_record(&rec).unwrap().software.as_deref(),
            Some("wget/1.21")
        );

        // Any other record type yields None.
        rec.warc_type = "response".to_string();
        assert!(Warcinfo::from_record(&rec).is_none());
    }

    #[test]
    fn parse_simple_warc_gz() {
        let records: Vec<_> = iter_records(
            &fixture("simple.warc.gz"),
            crate::wacz::DEFAULT_MEMBER_LIMIT,
        )
        .unwrap()
        .collect::<Result<Vec<_>>>()
        .unwrap();

        let resp = records
            .iter()
            .find(|r| r.warc_type.eq_ignore_ascii_case("response"))
            .expect("should have a response record");

        assert_eq!(resp.target_uri, "http://example.com/");
        assert_eq!(resp.http_status, Some(200));
        assert_eq!(resp.timestamp.len(), 14);
    }

    #[test]
    fn strips_angle_brackets_from_target_uri() {
        // wget writes `WARC-Target-URI: <url>` (bracketed); the brackets must be
        // stripped so the URL — and the SURT/CDX key derived from it — is bare.
        let http = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\nhi";
        let mut rec = Vec::new();
        rec.extend_from_slice(b"WARC/1.0\r\n");
        rec.extend_from_slice(b"WARC-Type: response\r\n");
        rec.extend_from_slice(b"WARC-Target-URI: <https://ex.com/x>\r\n");
        rec.extend_from_slice(b"WARC-Date: 2024-01-15T12:00:00Z\r\n");
        rec.extend_from_slice(b"Content-Type: application/http;msgtype=response\r\n");
        rec.extend_from_slice(format!("Content-Length: {}\r\n", http.len()).as_bytes());
        rec.extend_from_slice(b"\r\n");
        rec.extend_from_slice(http);
        rec.extend_from_slice(b"\r\n\r\n");

        let parsed = parse_warc_records(&rec, 0, rec.len() as u64);
        let r = parsed.into_iter().next().unwrap().unwrap();
        assert_eq!(r.target_uri, "https://ex.com/x");
    }

    #[test]
    fn parse_post_warc_gz() {
        let records: Vec<_> =
            iter_records(&fixture("post.warc.gz"), crate::wacz::DEFAULT_MEMBER_LIMIT)
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();

        assert!(records
            .iter()
            .any(|r| r.warc_type.eq_ignore_ascii_case("request")));
        assert!(records
            .iter()
            .any(|r| r.warc_type.eq_ignore_ascii_case("response")));

        let resp = records
            .iter()
            .find(|r| r.warc_type.eq_ignore_ascii_case("response"))
            .unwrap();
        assert!(
            resp.concurrent_to.is_some(),
            "response should have WARC-Concurrent-To"
        );
    }

    #[test]
    fn iso_to_14digit_conversion() {
        assert_eq!(iso_to_14digit("2006-01-02T15:04:05Z"), "20060102150405");
        assert_eq!(iso_to_14digit("2024-12-31T00:00:00Z"), "20241231000000");
    }

    #[test]
    fn all_records_have_record_length() {
        // Verify that record_length is populated (> 0) for every record.
        let records: Vec<_> =
            iter_records(&fixture("post.warc.gz"), crate::wacz::DEFAULT_MEMBER_LIMIT)
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();

        assert!(!records.is_empty());
        for r in &records {
            assert!(r.record_length > 0, "record_length should be > 0");
        }
    }
}
