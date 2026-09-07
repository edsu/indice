use super::*;
// Phase internals these tests exercise directly (the pipeline's own surface
// comes in via `use super::*`).
use super::acquire::{file_display_name, local_warcs_streamable};
use super::pages::{index_nested_from, index_wacz, index_wacz_streaming, last_modified_year};
use crate::index::testsupport::*;
use tempfile::TempDir;

/// Index a fixture WACZ either by scanning (default) or CDX-guided streaming,
/// returning the page count for a parity comparison. (Per-record extraction
/// correctness is covered by `wacz::record_at` tests + the offset proof.)
fn indexed_page_count(fixture_name: &str, stream: bool) -> u64 {
    use crate::search::SearchIndex;
    let f = fixture(fixture_name);
    let tmp = TempDir::new().unwrap();
    let search = Mutex::new(SearchIndex::open(tmp.path()).unwrap());
    let stats = if stream {
        let fetch = crate::http_range::FileFetch::open(&f).unwrap();
        index_wacz_streaming(
            fetch,
            "cid",
            "cname",
            "coll",
            &search,
            fixture_name,
            4,
            None,
        )
        .unwrap()
    } else {
        index_wacz(&f, "cid", "cname", "coll", &search).unwrap()
    };
    stats.pages
}
#[test]
fn streaming_matches_scan_on_a_stored_wacz() {
    // a.wacz stores its WARCs uncompressed, so streaming can seek into them.
    let scan = indexed_page_count("a.wacz", false);
    let stream = indexed_page_count("a.wacz", true);
    assert!(scan > 0, "fixture should index some pages");
    assert_eq!(
        scan, stream,
        "CDX-guided streaming must index the same page count as scanning"
    );
}
#[test]
fn local_warcs_streamable_gates_the_default_extraction() {
    // The auto-decision for a local file: CDX-guided when WARCs are Stored,
    // else a full scan. a.wacz is Stored; simple.wacz deflates its WARCs.
    assert!(local_warcs_streamable(&fixture("a.wacz")).unwrap());
    assert!(!local_warcs_streamable(&fixture("simple.wacz")).unwrap());
}
#[test]
fn streaming_refuses_a_deflated_wacz() {
    use crate::search::SearchIndex;
    // simple.wacz deflates its WARC entries, which streaming can't seek into.
    let f = fixture("simple.wacz");
    let tmp = TempDir::new().unwrap();
    let search = Mutex::new(SearchIndex::open(tmp.path()).unwrap());
    let fetch = crate::http_range::FileFetch::open(&f).unwrap();
    let err = index_wacz_streaming(
        fetch,
        "cid",
        "cname",
        "coll",
        &search,
        "simple.wacz",
        4,
        None,
    )
    .unwrap_err()
    .to_string()
    .to_lowercase();
    assert!(
        err.contains("stored") || err.contains("compress"),
        "unexpected error: {err}"
    );
}
#[test]
fn last_modified_year_parses_http_date() {
    let headers = vec![
        ("Content-Type".to_string(), "text/html".to_string()),
        (
            "Last-Modified".to_string(),
            "Wed, 21 Oct 2015 07:28:00 GMT".to_string(),
        ),
    ];
    assert_eq!(last_modified_year(&headers), Some(2015));
    // Header name match is case-insensitive.
    let headers = vec![(
        "last-modified".to_string(),
        "Mon, 01 Jan 2001 00:00:00 GMT".to_string(),
    )];
    assert_eq!(last_modified_year(&headers), Some(2001));
    // Absent or unparseable -> None.
    assert_eq!(last_modified_year(&[]), None);
    assert_eq!(
        last_modified_year(&[("Last-Modified".to_string(), "not a date".to_string())]),
        None
    );
}
#[test]
fn index_path_wacz_writes_manifest() {
    let tmp = TempDir::new().unwrap();
    index_fixture("simple.wacz", tmp.path(), Some("my-collection"));

    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(manifest.waczs.len(), 1);
    let col = &manifest.waczs[0];
    assert_eq!(col.name, "my-collection");
    assert!(!col.sha256.is_empty());
    assert!(col.file_size > 0);
}
#[test]
fn index_records_capture_status_histogram() {
    // a.wacz is a real Browsertrix WACZ whose CDX carries HTTP statuses; the
    // capture-quality tally should populate from it (task .9).
    let tmp = TempDir::new().unwrap();
    index_fixture("a.wacz", tmp.path(), None);
    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    let counts = &manifest.waczs[0].status_counts;
    assert!(!counts.is_empty(), "CDX statuses should be tallied");
    assert!(
        counts.keys().any(|c| (200..300).contains(c)),
        "a normal crawl is mostly 2xx; got {counts:?}"
    );
}
#[test]
fn index_path_name_defaults_to_stem() {
    // simple.wacz has no title in its datapackage, so the name falls back
    // to the filename stem.
    let tmp = TempDir::new().unwrap();
    index_fixture("simple.wacz", tmp.path(), None);

    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(manifest.waczs[0].name, "simple");
}
#[test]
fn indexed_local_wacz_is_filed_under_its_collection_relative_to_home() {
    let tmp = TempDir::new().unwrap();
    index_fixture("simple.wacz", tmp.path(), None);
    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    // `index` files the WACZ into archive/<collection-slug>/ and stores the
    // source relative to home (so the home dir stays portable).
    assert_eq!(
        manifest.waczs[0].source,
        Source::File(PathBuf::from("archive/test/simple.wacz")),
    );
    assert!(
        tmp.path().join("archive/test/simple.wacz").is_file(),
        "the WACZ was moved into its collection folder"
    );
}
#[test]
fn provenance_is_recorded_on_the_manifest() {
    // a.wacz carries crawler software (datapackage + warcinfo) and real
    // captures, so the manifest entry should record provenance.
    let tmp = TempDir::new().unwrap();
    index_fixture("a.wacz", tmp.path(), None);

    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    let col = &manifest.waczs[0];
    assert!(
        col.software
            .iter()
            .any(|s| s.contains("Browsertrix-Crawler")),
        "unexpected software: {:?}",
        col.software
    );
    assert!(col.page_count.is_some(), "page_count should be recorded");
}
/// Build a nested multi-WACZ that wraps the `a.wacz` fixture, mirroring a
/// real Browsertrix combined download: no top-level archive/ WARCs, the inner
/// .wacz a top-level *Stored* entry, and a multi-wacz-package datapackage.
fn nested_multi_wacz() -> Vec<u8> {
    use std::io::Write;
    let inner = std::fs::read(fixture("a.wacz")).unwrap();
    let inner_name = "20250101000000-abc-0.wacz";
    let mut outer = Vec::new();
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let opt = zip::write::SimpleFileOptions::default();
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut outer));
    zw.start_file(inner_name, stored).unwrap();
    zw.write_all(&inner).unwrap();
    zw.start_file("datapackage.json", opt).unwrap();
    let dp = format!(
        r#"{{"profile":"multi-wacz-package","resources":[{{"name":"{inner_name}","path":"{inner_name}"}}]}}"#
    );
    zw.write_all(dp.as_bytes()).unwrap();
    zw.finish().unwrap(); // consumes zw, releasing the borrow of `outer`
    outer
}
/// In-memory [`RangeFetch`], a stand-in for a remote `HttpFetch`.
#[derive(Clone)]
struct MemFetch(std::sync::Arc<Vec<u8>>);
impl crate::http_range::RangeFetch for MemFetch {
    fn total_len(&self) -> u64 {
        self.0.len() as u64
    }
    fn fetch(&self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        Ok(self.0[start as usize..end as usize].to_vec())
    }
}
#[test]
fn nested_multi_wacz_is_indexed() {
    let outer = nested_multi_wacz();
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let path = archive.join("nested.wacz");
    std::fs::write(&path, &outer).unwrap();
    index_path(&path, tmp.path(), None, "test").unwrap();

    // One manifest entry (approach A: flatten), with the inner crawl's pages
    // and provenance surfaced on it.
    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(manifest.waczs.len(), 1, "one manifest entry per outer file");
    let w = &manifest.waczs[0];
    assert!(
        w.page_count.unwrap_or(0) > 0,
        "the nested WACZ's inner pages should be indexed"
    );
    assert!(
        w.software.iter().any(|s| s.contains("Browsertrix-Crawler")),
        "the inner crawl's software should surface on the outer entry: {:?}",
        w.software
    );
    assert_eq!(
        w.nested_waczs,
        Some(1),
        "the entry should record how many inner WACZs it bundles"
    );
}
#[test]
fn nested_multi_wacz_streams_over_a_range_fetch() {
    // Drive index_nested through an in-memory RangeFetch (a stand-in for a
    // remote HttpFetch) to prove the Stored inner WACZ is read in place via
    // SubRangeFetch — no extraction, no full download.
    let outer = MemFetch(std::sync::Arc::new(nested_multi_wacz()));
    let tmp = TempDir::new().unwrap();
    let search = Mutex::new(SearchIndex::open(&tmp.path().join("ft")).unwrap());

    let stats = index_nested_from(outer, "cid", "Nested", "coll", &search, 2, None)
        .unwrap()
        .expect("should detect and index the nested WACZ");
    assert!(
        stats.pages > 0,
        "inner pages should be indexed by streaming in place"
    );
}
#[test]
fn browsertrix_source_without_resolver_errors_clearly() {
    // A Browsertrix source can't be indexed without a resolver to turn its
    // stable identity into a fresh presigned URL — the error should say so.
    let tmp = TempDir::new().unwrap();
    let loc = "browsertrix|https://app.browsertrix.com|o1|item-1|x-0.wacz";
    let err = index_location(loc, tmp.path(), None, "test", false, false, None, None)
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("BROWSERTRIX") || err.to_lowercase().contains("credential"),
        "{err}"
    );
}
#[test]
fn index_into_named_collection_groups_the_wacz() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let dest = archive.join("simple.wacz");
    std::fs::copy(fixture("simple.wacz"), &dest).unwrap();

    index_location(
        &dest.to_string_lossy(),
        tmp.path(),
        None,
        "My Project",
        false,
        false,
        None,
        None,
    )
    .unwrap();

    let m = crate::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    assert!(
        m.collections
            .iter()
            .any(|c| c.id == "my-project" && c.name == "My Project"),
        "collection should be created: {:?}",
        m.collections.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
    assert_eq!(
        m.waczs[0].collection, "my-project",
        "WACZ should reference the collection"
    );
}
#[test]
fn index_copies_external_wacz_into_the_collection_archive() {
    // A WACZ from anywhere is brought into archive/<slug>/ — copied when it's
    // outside the home, leaving the curator's original intact.
    let home = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let stray = elsewhere.path().join("simple.wacz");
    std::fs::copy(fixture("simple.wacz"), &stray).unwrap();

    index_path(&stray, home.path(), None, "My Coll").unwrap();

    assert!(
        stray.is_file(),
        "the external original is left in place (copied, not moved)"
    );
    assert!(
        home.path().join("archive/my-coll/simple.wacz").is_file(),
        "the WACZ is copied into archive/<slug>/"
    );
    let m = Manifest::open(&home.path().join("index")).unwrap();
    assert_eq!(
        m.waczs[0].source,
        Source::File(PathBuf::from("archive/my-coll/simple.wacz"))
    );
}
#[test]
fn index_seeds_collection_finding_aid_from_the_wacz() {
    // Indexing a WACZ pre-seeds its collection's finding aid (fill-gaps) from
    // the datapackage. a.wacz declares created 2026-…, so `dates` is seeded.
    let tmp = TempDir::new().unwrap();
    index_fixture("a.wacz", tmp.path(), None); // collection "test"
    let m = Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(
        m.collection_by_id("test").unwrap().dates.as_deref(),
        Some("2026"),
        "collection dates seeded from the WACZ datapackage `created` year"
    );
}
#[test]
fn index_does_not_clobber_two_different_wacz_with_the_same_basename() {
    // The `index a/report.wacz b/report.wacz --collection X` workflow: two
    // DISTINCT files sharing a basename must stay two crawls, not silently
    // collapse into one (regression guard).
    let home = TempDir::new().unwrap();
    let d1 = TempDir::new().unwrap();
    let d2 = TempDir::new().unwrap();
    std::fs::copy(fixture("a.wacz"), d1.path().join("report.wacz")).unwrap();
    std::fs::copy(fixture("simple.wacz"), d2.path().join("report.wacz")).unwrap();

    index_path(&d1.path().join("report.wacz"), home.path(), None, "Reports").unwrap();
    index_path(&d2.path().join("report.wacz"), home.path(), None, "Reports").unwrap();

    let m = Manifest::open(&home.path().join("index")).unwrap();
    assert_eq!(
        m.waczs.len(),
        2,
        "two distinct WACZs must remain two crawls"
    );
    // The second was disambiguated rather than overwriting the first.
    assert!(home.path().join("archive/reports/report.wacz").is_file());
    assert!(home.path().join("archive/reports/report-2.wacz").is_file());

    // Re-indexing the same external file is idempotent (byte-identical → reused).
    index_path(&d1.path().join("report.wacz"), home.path(), None, "Reports").unwrap();
    let m = Manifest::open(&home.path().join("index")).unwrap();
    assert_eq!(
        m.waczs.len(),
        2,
        "re-indexing an identical file must not duplicate"
    );
}
#[test]
fn index_refuses_to_recollect_a_registered_crawl() {
    // Indexing a WACZ that's already filed in one collection into a different
    // one is refused (moving it would change its id and orphan its assets).
    let home = TempDir::new().unwrap();
    index_fixture("simple.wacz", home.path(), None); // collection "test"
    let filed = home.path().join("archive/test/simple.wacz");
    assert!(filed.is_file());

    let err = index_path(&filed, home.path(), None, "Other")
        .expect_err("re-collecting a filed crawl should be refused");
    assert!(
        format!("{err:#}").contains("already in collection"),
        "error should explain the crawl is already collected: {err:#}"
    );
}
#[test]
fn index_rejects_a_directory() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&archive).unwrap();

    let err = index_path(&archive, tmp.path(), None, "test")
        .expect_err("indexing a directory should fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("directory"),
        "error should say it is a directory: {msg}"
    );
}
#[test]
fn index_name_comes_from_datapackage_title() {
    // pdf-doc.wacz has "title": "PDF Test Collection" in its datapackage,
    // which should name the collection when --name is not given.
    let tmp = TempDir::new().unwrap();
    index_fixture("pdf-doc.wacz", tmp.path(), None);

    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(manifest.waczs[0].name, "PDF Test Collection");
}
#[test]
fn explicit_name_overrides_datapackage_title() {
    // --name wins even when the WACZ has a title.
    let tmp = TempDir::new().unwrap();
    index_fixture("pdf-doc.wacz", tmp.path(), Some("Custom Name"));

    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    assert_eq!(manifest.waczs[0].name, "Custom Name");
}
#[test]
fn pages_jsonl_text_is_indexed_when_absent_from_html() {
    use std::io::Write;
    // A crawl whose rendered text lives ONLY in pages.jsonl (older
    // Browsertrix/SUCHO WACZs write it there, not as urn:text: records): the
    // HTML body lacks the term, but the pages.jsonl `text` field has it. It
    // must still be searchable, via the default CDX-guided path.
    let term = "zqxsentinel"; // unique token, present only in pages.jsonl text
    let url = "https://ex.com/";

    // One WARC response record whose HTML does NOT contain the term.
    let http = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
             <html><head><title>Home Page</title></head><body>nothing useful</body></html>";
    let mut warc = format!(
        "WARC/1.0\r\nWARC-Type: response\r\nWARC-Target-URI: {url}\r\n\
             WARC-Date: 2022-01-01T00:00:00Z\r\n\
             Content-Type: application/http; msgtype=response\r\nContent-Length: {}\r\n\r\n",
        http.len()
    )
    .into_bytes();
    warc.extend_from_slice(http);
    warc.extend_from_slice(b"\r\n\r\n");
    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    };
    let warc_gz = gz(&warc); // single gzip member => offset 0 in data.warc.gz

    // CDX pointing at that record (whole member).
    let cdx_line = format!(
        "com,ex)/ 20220101000000 {{\"url\":\"{url}\",\"mime\":\"text/html\",\
             \"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":0,\"length\":{}}}\n",
        warc_gz.len()
    );
    let cdx_gz = gz(cdx_line.as_bytes());

    // pages.jsonl: header + a page whose `text` carries the term (+Cyrillic).
    let pages = format!(
        "{{\"format\":\"json-pages-1.0\",\"id\":\"pages\",\"title\":\"All Pages\"}}\n\
             {{\"id\":\"p1\",\"url\":\"{url}\",\"title\":\"Home Page\",\
             \"ts\":\"2022-01-01T00:00:00Z\",\"text\":\"Петиція {term}\"}}\n"
    );

    // Assemble the WACZ. The WARC must be Stored so it takes the CDX-guided path.
    let mut wacz = Vec::new();
    {
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let opt = zip::write::SimpleFileOptions::default();
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut wacz));
        zw.start_file("archive/data.warc.gz", stored).unwrap();
        zw.write_all(&warc_gz).unwrap();
        zw.start_file("indexes/index.cdx.gz", opt).unwrap();
        zw.write_all(&cdx_gz).unwrap();
        zw.start_file("pages/pages.jsonl", opt).unwrap();
        zw.write_all(pages.as_bytes()).unwrap();
        zw.finish().unwrap();
    }

    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let path = archive.join("crawl.wacz");
    std::fs::write(&path, &wacz).unwrap();
    assert!(
        local_warcs_streamable(&path).unwrap(),
        "stored WARC should take the CDX-guided path"
    );
    index_path(&path, tmp.path(), None, "test").unwrap();

    let idx =
        crate::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    assert!(
        idx.search(term, 10)
            .unwrap()
            .iter()
            .any(|r| r.doc_type == "page" && r.url == url),
        "text from pages.jsonl must be searchable (term absent from the HTML)"
    );
    assert!(
        !idx.search("Петиція", 10).unwrap().is_empty(),
        "Cyrillic rendered text from pages.jsonl must be searchable"
    );
    assert!(
        !idx.search("Home Page", 10).unwrap().is_empty(),
        "the HTML <title> is still indexed"
    );
}
#[test]
fn og_image_thumbnail_is_cached() {
    use std::io::Write;
    // End-to-end: a crawl whose main page declares an og:image pointing at a
    // captured PNG should get a downscaled JPEG thumbnail cached under
    // <home>/index/thumbs.
    let page_url = "https://ex.com/";
    let img_url = "https://ex.com/preview.png";

    // The captured og:image (a small PNG).
    let mut png_buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        24,
        16,
        image::Rgb([210, 90, 70]),
    ))
    .write_to(&mut png_buf, image::ImageFormat::Png)
    .unwrap();
    let png = png_buf.into_inner();

    let html = format!(
        "<html><head><title>Home</title>\
             <meta property=\"og:image\" content=\"{img_url}\"></head><body>hi</body></html>"
    );

    // One WARC response record, gzipped as a single member.
    let gz_record = |url: &str, ctype: &str, body: &[u8]| {
        let mut http = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\n\r\n").into_bytes();
        http.extend_from_slice(body);
        let mut warc = format!(
            "WARC/1.0\r\nWARC-Type: response\r\nWARC-Target-URI: {url}\r\n\
                 WARC-Date: 2022-01-01T00:00:00Z\r\n\
                 Content-Type: application/http; msgtype=response\r\nContent-Length: {}\r\n\r\n",
            http.len()
        )
        .into_bytes();
        warc.extend_from_slice(&http);
        warc.extend_from_slice(b"\r\n\r\n");
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&warc).unwrap();
        e.finish().unwrap()
    };
    let html_member = gz_record(page_url, "text/html", html.as_bytes());
    let png_member = gz_record(img_url, "image/png", &png);
    let mut warc_gz = html_member.clone();
    warc_gz.extend_from_slice(&png_member);

    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    };
    // CDX: HTML at offset 0, the PNG right after it.
    let cdx = format!(
        "com,ex)/ 20220101000000 {{\"url\":\"{page_url}\",\"mime\":\"text/html\",\
             \"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":0,\"length\":{}}}\n\
             com,ex)/preview.png 20220101000000 {{\"url\":\"{img_url}\",\"mime\":\"image/png\",\
             \"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":{},\"length\":{}}}\n",
        html_member.len(),
        html_member.len(),
        png_member.len()
    );
    let cdx_gz = gz(cdx.as_bytes());
    let datapackage = format!("{{\"mainPageUrl\":\"{page_url}\"}}");

    let mut wacz = Vec::new();
    {
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let opt = zip::write::SimpleFileOptions::default();
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut wacz));
        zw.start_file("archive/data.warc.gz", stored).unwrap();
        zw.write_all(&warc_gz).unwrap();
        zw.start_file("indexes/index.cdx.gz", opt).unwrap();
        zw.write_all(&cdx_gz).unwrap();
        zw.start_file("datapackage.json", opt).unwrap();
        zw.write_all(datapackage.as_bytes()).unwrap();
        zw.finish().unwrap();
    }

    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let path = archive.join("crawl.wacz");
    std::fs::write(&path, &wacz).unwrap();
    index_path(&path, tmp.path(), None, "test").unwrap();

    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    let id = &manifest.waczs[0].id;
    let thumb = tmp
        .path()
        .join("index")
        .join("thumbs")
        .join(format!("{id}.jpg"));
    assert!(
        thumb.exists(),
        "a crawl with an og:image should get a cached thumbnail"
    );
    let decoded = image::load_from_memory(&std::fs::read(&thumb).unwrap()).unwrap();
    assert!(
        decoded.width() <= 400 && decoded.height() <= 400,
        "thumbnail should be downscaled"
    );
}
#[test]
fn browsertrix_screenshot_is_preferred_over_og_image() {
    use std::io::Write;
    // A crawl that has BOTH an og:image and a Browsertrix screenshot
    // (urn:thumbnail:<page>) should thumbnail from the *screenshot* — it's an
    // actual picture of the page. We tell them apart by aspect ratio:
    // thumbnail() scales to fit 400px preserving aspect, so the 40x30 (4:3)
    // screenshot yields 400x300, whereas the 24x16 (3:2) og:image would yield
    // 400x266.
    let page_url = "https://ex.com/";
    let og_url = "https://ex.com/preview.png";

    let png = |w: u32, h: u32| {
        let mut b = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([9, 9, 9])))
            .write_to(&mut b, image::ImageFormat::Png)
            .unwrap();
        b.into_inner()
    };
    let jpeg = |w: u32, h: u32| {
        let mut b = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([9, 9, 9])))
            .write_to(&mut b, image::ImageFormat::Jpeg)
            .unwrap();
        b.into_inner()
    };
    let og_png = png(24, 16);
    let shot = jpeg(40, 30);

    let html = format!(
        "<html><head><meta property=\"og:image\" content=\"{og_url}\"></head>\
             <body>hi</body></html>"
    );

    let gz_record = |url: &str, ctype: &str, body: &[u8]| {
        let mut http = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\n\r\n").into_bytes();
        http.extend_from_slice(body);
        let mut warc = format!(
            "WARC/1.0\r\nWARC-Type: response\r\nWARC-Target-URI: {url}\r\n\
                 WARC-Date: 2022-01-01T00:00:00Z\r\n\
                 Content-Type: application/http; msgtype=response\r\nContent-Length: {}\r\n\r\n",
            http.len()
        )
        .into_bytes();
        warc.extend_from_slice(&http);
        warc.extend_from_slice(b"\r\n\r\n");
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&warc).unwrap();
        e.finish().unwrap()
    };
    let html_m = gz_record(page_url, "text/html", html.as_bytes());
    let og_m = gz_record(og_url, "image/png", &og_png);
    let shot_m = gz_record(&format!("urn:thumbnail:{page_url}"), "image/jpeg", &shot);
    let mut warc_gz = html_m.clone();
    warc_gz.extend_from_slice(&og_m);
    warc_gz.extend_from_slice(&shot_m);

    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    };
    let cdx = format!(
        "com,ex)/ 20220101000000 {{\"url\":\"{page_url}\",\"mime\":\"text/html\",\
             \"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":0,\"length\":{}}}\n\
             com,ex)/preview.png 20220101000000 {{\"url\":\"{og_url}\",\"mime\":\"image/png\",\
             \"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":{},\"length\":{}}}\n\
             urn:thumbnail:{page_url} 20220101000000 {{\"url\":\"urn:thumbnail:{page_url}\",\
             \"mime\":\"image/jpeg\",\"status\":\"200\",\"filename\":\"data.warc.gz\",\
             \"offset\":{},\"length\":{}}}\n",
        html_m.len(),
        html_m.len(),
        og_m.len(),
        html_m.len() + og_m.len(),
        shot_m.len(),
    );
    let cdx_gz = gz(cdx.as_bytes());
    let datapackage = format!("{{\"mainPageUrl\":\"{page_url}\"}}");

    let mut wacz = Vec::new();
    {
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let opt = zip::write::SimpleFileOptions::default();
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut wacz));
        zw.start_file("archive/data.warc.gz", stored).unwrap();
        zw.write_all(&warc_gz).unwrap();
        zw.start_file("indexes/index.cdx.gz", opt).unwrap();
        zw.write_all(&cdx_gz).unwrap();
        zw.start_file("datapackage.json", opt).unwrap();
        zw.write_all(datapackage.as_bytes()).unwrap();
        zw.finish().unwrap();
    }

    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let path = archive.join("crawl.wacz");
    std::fs::write(&path, &wacz).unwrap();
    index_path(&path, tmp.path(), None, "test").unwrap();

    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    let id = &manifest.waczs[0].id;
    let thumb = tmp
        .path()
        .join("index")
        .join("thumbs")
        .join(format!("{id}.jpg"));
    assert!(thumb.exists(), "a screenshot should produce a thumbnail");
    let decoded = image::load_from_memory(&std::fs::read(&thumb).unwrap()).unwrap();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (400, 300),
        "the thumbnail should come from the 4:3 screenshot, not the 3:2 og:image"
    );
}
#[test]
fn thumbnail_falls_back_to_largest_page_image() {
    use std::io::Write;
    // The main page has NO og:image but embeds two images (a tiny icon and a
    // larger hero). The thumbnail should fall back to the largest captured
    // content image, skipping the sub-threshold icon.
    let page_url = "https://ex.com/";

    // A "noisy" hero PNG (poor compression → well over the 5 KB floor) and a
    // tiny icon (under it, so it's skipped).
    let png = |w: u32, h: u32, noisy: bool| {
        let mut im = image::RgbImage::new(w, h);
        // Pseudo-random (incompressible) pixels so the PNG stays large; a solid
        // fill for the tiny icon so it compresses well below the floor.
        let mut st: u32 = 0x1234_5678;
        for (_, _, p) in im.enumerate_pixels_mut() {
            if noisy {
                st = st.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let b = st.to_le_bytes();
                *p = image::Rgb([b[0], b[1], b[2]]);
            } else {
                *p = image::Rgb([200, 200, 200]);
            }
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(im)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    };
    let hero = png(160, 160, true);
    let icon = png(8, 8, false);
    assert!(
        hero.len() >= 5000 && icon.len() < 5000,
        "test image sizes bracket the floor"
    );

    let html = "<html><head><title>Home</title></head><body>\
             <img src=\"icon.png\"><img src=\"hero.png\"></body></html>";

    let gz_record = |url: &str, ctype: &str, body: &[u8]| {
        let mut http = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\n\r\n").into_bytes();
        http.extend_from_slice(body);
        let mut warc = format!(
            "WARC/1.0\r\nWARC-Type: response\r\nWARC-Target-URI: {url}\r\n\
                 WARC-Date: 2023-01-01T00:00:00Z\r\n\
                 Content-Type: application/http; msgtype=response\r\nContent-Length: {}\r\n\r\n",
            http.len()
        )
        .into_bytes();
        warc.extend_from_slice(&http);
        warc.extend_from_slice(b"\r\n\r\n");
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&warc).unwrap();
        e.finish().unwrap()
    };
    let m_html = gz_record(page_url, "text/html", html.as_bytes());
    let m_icon = gz_record("https://ex.com/icon.png", "image/png", &icon);
    let m_hero = gz_record("https://ex.com/hero.png", "image/png", &hero);
    let mut warc_gz = m_html.clone();
    warc_gz.extend_from_slice(&m_icon);
    warc_gz.extend_from_slice(&m_hero);

    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    };
    let (o_html, o_icon) = (0, m_html.len());
    let o_hero = m_html.len() + m_icon.len();
    let cdx = format!(
            "com,ex)/ 20230101000000 {{\"url\":\"{page_url}\",\"mime\":\"text/html\",\"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":{o_html},\"length\":{}}}\n\
             com,ex)/icon.png 20230101000000 {{\"url\":\"https://ex.com/icon.png\",\"mime\":\"image/png\",\"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":{o_icon},\"length\":{}}}\n\
             com,ex)/hero.png 20230101000000 {{\"url\":\"https://ex.com/hero.png\",\"mime\":\"image/png\",\"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":{o_hero},\"length\":{}}}\n",
            m_html.len(), m_icon.len(), m_hero.len()
        );
    let cdx_gz = gz(cdx.as_bytes());
    let datapackage = format!("{{\"mainPageUrl\":\"{page_url}\"}}");

    let mut wacz = Vec::new();
    {
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let opt = zip::write::SimpleFileOptions::default();
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut wacz));
        zw.start_file("archive/data.warc.gz", stored).unwrap();
        zw.write_all(&warc_gz).unwrap();
        zw.start_file("indexes/index.cdx.gz", opt).unwrap();
        zw.write_all(&cdx_gz).unwrap();
        zw.start_file("datapackage.json", opt).unwrap();
        zw.write_all(datapackage.as_bytes()).unwrap();
        zw.finish().unwrap();
    }

    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let path = archive.join("crawl.wacz");
    std::fs::write(&path, &wacz).unwrap();
    index_path(&path, tmp.path(), None, "test").unwrap();

    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    let id = &manifest.waczs[0].id;
    let thumb = tmp
        .path()
        .join("index")
        .join("thumbs")
        .join(format!("{id}.jpg"));
    assert!(
        thumb.exists(),
        "with no og:image, a thumbnail should be generated from the largest embedded image"
    );
}
#[test]
fn thumbnail_falls_back_to_largest_on_site_captured_image() {
    use std::io::Write;
    // A JS-rendered site: the saved HTML has NO og:image and NO <img>, but the
    // crawl captured images. The thumbnail should come from the largest
    // in-window raster image ON THE CRAWL'S OWN DOMAIN — a bigger off-domain
    // (CDN/ad) image must be ignored.
    let page_url = "https://ex.com/";

    // Distinct aspect ratios so we can tell which image was chosen: the on-site
    // one is landscape, the (larger, must-ignore) off-site one is portrait.
    let png = |w: u32, h: u32| {
        let mut im = image::RgbImage::new(w, h);
        let mut st: u32 = 0x9e37_79b9;
        for (_, _, p) in im.enumerate_pixels_mut() {
            st = st.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let b = st.to_le_bytes();
            *p = image::Rgb([b[0], b[1], b[2]]);
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(im)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    };
    let onsite = png(240, 120); // landscape (w > h)
    let offsite = png(160, 320); // portrait, larger byte size
    assert!(offsite.len() > onsite.len() && onsite.len() >= 5000);

    let html = "<html><head><title>Home</title></head><body>no images here</body></html>";

    let gz_record = |url: &str, ctype: &str, body: &[u8]| {
        let mut http = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\n\r\n").into_bytes();
        http.extend_from_slice(body);
        let mut warc = format!(
            "WARC/1.0\r\nWARC-Type: response\r\nWARC-Target-URI: {url}\r\n\
                 WARC-Date: 2023-01-01T00:00:00Z\r\n\
                 Content-Type: application/http; msgtype=response\r\nContent-Length: {}\r\n\r\n",
            http.len()
        )
        .into_bytes();
        warc.extend_from_slice(&http);
        warc.extend_from_slice(b"\r\n\r\n");
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&warc).unwrap();
        e.finish().unwrap()
    };
    let m_html = gz_record(page_url, "text/html", html.as_bytes());
    let m_on = gz_record("https://ex.com/photo.jpg", "image/jpeg", &onsite);
    let m_off = gz_record("https://cdn.other.com/ad.jpg", "image/jpeg", &offsite);
    let mut warc_gz = m_html.clone();
    warc_gz.extend_from_slice(&m_on);
    warc_gz.extend_from_slice(&m_off);

    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    };
    let o_on = m_html.len();
    let o_off = m_html.len() + m_on.len();
    let cdx = format!(
            "com,ex)/ 20230101000000 {{\"url\":\"{page_url}\",\"mime\":\"text/html\",\"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":0,\"length\":{}}}\n\
             com,ex)/photo.jpg 20230101000000 {{\"url\":\"https://ex.com/photo.jpg\",\"mime\":\"image/jpeg\",\"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":{o_on},\"length\":{}}}\n\
             com,other,cdn)/ad.jpg 20230101000000 {{\"url\":\"https://cdn.other.com/ad.jpg\",\"mime\":\"image/jpeg\",\"status\":\"200\",\"filename\":\"data.warc.gz\",\"offset\":{o_off},\"length\":{}}}\n",
            m_html.len(), m_on.len(), m_off.len()
        );
    let cdx_gz = gz(cdx.as_bytes());
    let datapackage = format!("{{\"mainPageUrl\":\"{page_url}\"}}");

    let mut wacz = Vec::new();
    {
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let opt = zip::write::SimpleFileOptions::default();
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut wacz));
        zw.start_file("archive/data.warc.gz", stored).unwrap();
        zw.write_all(&warc_gz).unwrap();
        zw.start_file("indexes/index.cdx.gz", opt).unwrap();
        zw.write_all(&cdx_gz).unwrap();
        zw.start_file("datapackage.json", opt).unwrap();
        zw.write_all(datapackage.as_bytes()).unwrap();
        zw.finish().unwrap();
    }

    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let path = archive.join("crawl.wacz");
    std::fs::write(&path, &wacz).unwrap();
    index_path(&path, tmp.path(), None, "test").unwrap();

    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    let id = &manifest.waczs[0].id;
    let thumb = tmp
        .path()
        .join("index")
        .join("thumbs")
        .join(format!("{id}.jpg"));
    assert!(
        thumb.exists(),
        "a JS-rendered crawl should still get a thumbnail from a captured on-site image"
    );
    let decoded = image::load_from_memory(&std::fs::read(&thumb).unwrap()).unwrap();
    assert!(
        decoded.width() > decoded.height(),
        "the on-site landscape image should be chosen, not the larger off-site portrait one"
    );
}
#[test]
fn pdf_pages_are_filterable_by_type() {
    // End-to-end: a PDF response in the WACZ should be tagged type:pdf so
    // it can be filtered from the search box.
    let tmp = TempDir::new().unwrap();
    index_fixture("pdf-doc.wacz", tmp.path(), None);

    let idx =
        crate::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    let results = idx.search("type:pdf", 10).unwrap();
    assert!(
        results.iter().any(|r| r.doc_type == "page"),
        "PDF page should be reachable via type:pdf"
    );
}
#[test]
fn index_wacz_html_is_searchable() {
    let tmp = TempDir::new().unwrap();
    index_fixture("simple.wacz", tmp.path(), None);

    let idx =
        crate::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    let results = idx.search("example", 10).unwrap();
    assert!(!results.is_empty(), "should find HTML content from WACZ");
    assert_eq!(results[0].crawl_name, "simple");
}
#[test]
fn index_wacz_stores_seed_pages_in_manifest() {
    let tmp = TempDir::new().unwrap();
    index_fixture("simple.wacz", tmp.path(), None);

    let manifest = Manifest::open(&tmp.path().join("index")).unwrap();
    let col = &manifest.waczs[0];
    assert!(
        !col.seed_pages.is_empty(),
        "simple.wacz has pages in pages.jsonl"
    );
    assert_eq!(col.seed_pages[0].url, "http://example.com/");
}
#[test]
fn index_wacz_collection_is_searchable() {
    let tmp = TempDir::new().unwrap();
    index_fixture("simple.wacz", tmp.path(), None);

    let idx =
        crate::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    // The seed page URL "http://example.com/" is part of the collection body.
    let results = idx.search("example.com", 10).unwrap();
    assert!(
        results.iter().any(|r| r.doc_type == "collection"),
        "collection document should be searchable"
    );
}
#[test]
fn reindexing_does_not_duplicate_documents() {
    let tmp = TempDir::new().unwrap();
    index_fixture("simple.wacz", tmp.path(), None);
    index_fixture("simple.wacz", tmp.path(), None);

    let idx =
        crate::search::SearchIndex::open(tmp.path().join("index").join("full_text").as_path())
            .unwrap();
    let results = idx.search("example", 50).unwrap();
    let pages = results.iter().filter(|r| r.doc_type == "page").count();
    assert_eq!(pages, 1, "re-indexing should upsert, not duplicate pages");
}
#[test]
fn mime_display_name_strips_extension() {
    let p = Path::new("/data/my-archive.wacz");
    assert_eq!(file_display_name(p), "my-archive");
    let p2 = Path::new("/data/my.warc.gz");
    assert_eq!(file_display_name(p2), "my");
}
