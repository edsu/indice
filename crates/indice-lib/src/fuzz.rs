//! Entry points for the fuzz targets in `fuzz/` (#225), which are a separate
//! crate and so can only call what is `pub`. Each runs a parser that reads
//! untrusted bytes the way indexing does, and throws the result away: what a
//! fuzzer is looking for is a panic, a hang or memory past what [`LIMIT`]
//! allows, not a wrong answer.
//!
//! Compiled into every build rather than behind a feature, so that a change
//! to a parser's signature breaks here, under the normal `cargo build`, and
//! not only when someone next runs the fuzzer.

use std::io::Cursor;

/// The member limit the targets read with, in place of `index.max_member_mb`
/// (1 GiB by default): small, so that a decompression bomb reaches it in
/// milliseconds, and so that a run with `-rss_limit_mb=1024` stopping means
/// memory grew past what the limit is meant to bound.
pub const LIMIT: u64 = 8 * 1024 * 1024;

/// The WARC record and `warcinfo` field parsers, on bytes as a decompressed
/// record slice holds them.
pub fn warc(data: &[u8]) {
    let _ = crate::warc::parse_warc_records(data, 0, data.len() as u64);
    let _ = crate::warc::parse_warc_fields(data);
}

/// One record's bytes as a CDX lookup fetches them, gzip member or plain,
/// inflated to at most [`LIMIT`] bytes.
pub fn warc_slice(data: &[u8]) {
    let _ = crate::wacz::records_from_slice(data, 0, data.len() as u64, LIMIT);
}

/// A WACZ, through everything indexing reads from one before its records:
/// `datapackage.json` and the page lists, nested WACZs, whether the WARCs are
/// stored, where they start, their `warcinfo`, and the CDX index. Each member
/// is read to at most [`LIMIT`] bytes.
pub fn wacz(data: &[u8]) {
    let _ = crate::wacz::read_datapackage_from(Cursor::new(data), LIMIT);
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(data)) else {
        return;
    };
    let _ = crate::wacz::nested_wacz_locations(&mut zip);
    let _ = crate::wacz::ensure_warcs_stored(&mut zip);
    let _ = crate::wacz::warc_data_starts(&mut zip);
    let _ = crate::wacz::find_warcinfo_streaming(&mut zip, LIMIT);
    let _ = crate::wacz::cdx_records(&mut zip, LIMIT);
}
