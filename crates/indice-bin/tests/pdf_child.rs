//! PDF text extraction runs in a child process with limits (#251): a PDF the
//! child cannot extract within them is indexed by its file name, and the
//! ingest carries on. These run the built binary, since the child is the
//! binary re-running itself; under `cargo test` of the library, extraction
//! stays in process.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The fixture's PDF, whose text says "flux capacitor".
fn real_pdf() -> Vec<u8> {
    let wacz =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../indice-lib/tests/fixtures/pdf-doc.wacz");
    let mut zip = zip::ZipArchive::new(std::fs::File::open(wacz).unwrap()).unwrap();
    let mut warc = Vec::new();
    std::io::Read::read_to_end(&mut zip.by_name("archive/data.warc").unwrap(), &mut warc).unwrap();
    let start = find(&warc, b"%PDF").unwrap();
    let end = rfind(&warc, b"%%EOF").unwrap() + 5;
    warc[start..end].to_vec()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// A WACZ holding each `(url, pdf)` as an `application/pdf` response.
fn wacz_with(dir: &Path, pdfs: &[(&str, Vec<u8>)]) -> PathBuf {
    let mut warc = Vec::new();
    let mut pages = String::from("{\"format\":\"json-pages-1.0\",\"id\":\"pages\"}\n");
    for (i, (url, pdf)) in pdfs.iter().enumerate() {
        let mut http = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: {}\r\n\r\n",
            pdf.len()
        )
        .into_bytes();
        http.extend_from_slice(pdf);
        write!(
            warc,
            "WARC/1.0\r\nWARC-Type: response\r\nWARC-Target-URI: {url}\r\n\
             WARC-Date: 2026-01-01T00:00:00Z\r\n\
             WARC-Record-ID: <urn:uuid:00000000-0000-0000-0000-{i:012}>\r\n\
             Content-Type: application/http;msgtype=response\r\nContent-Length: {}\r\n\r\n",
            http.len()
        )
        .unwrap();
        warc.extend_from_slice(&http);
        warc.extend_from_slice(b"\r\n\r\n");
        pages.push_str(&format!(
            "{{\"id\":\"p{i}\",\"url\":\"{url}\",\"ts\":\"2026-01-01T00:00:00Z\"}}\n"
        ));
    }
    let path = dir.join("pdfs.wacz");
    let mut zw = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    // Deflated, as a few tools write them: with no CDX in the WACZ, that is
    // what sends the ingest to scan the WARC rather than look records up.
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in [
        (
            "datapackage.json",
            br#"{"profile":"data-package","resources":[{"name":"data.warc","path":"archive/data.warc"}],"wacz_version":"1.1.1"}"#.to_vec(),
        ),
        ("pages/pages.jsonl", pages.into_bytes()),
        ("archive/data.warc", warc),
    ] {
        zw.start_file(name, deflated).unwrap();
        zw.write_all(&bytes).unwrap();
    }
    zw.finish().unwrap();
    path
}

/// Index `wacz` into `home` with `config` as its `config.yaml`, returning
/// stderr. The ingest must succeed.
fn index(home: &Path, wacz: &Path, config: &str) -> String {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(home.join("config.yaml"), config).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_indice"))
        .arg("index")
        .arg(wacz)
        .args(["--collection", "pdfs", "--home"])
        .arg(home)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "index failed: {stderr}");
    stderr
}

/// The pages `query` finds (not the crawl or collection it is in).
fn urls_matching(home: &Path, query: &str) -> Vec<String> {
    let idx =
        indice_lib::search::SearchIndex::open_read_only(&home.join("index").join("full_text"))
            .unwrap();
    idx.search(query, 10)
        .unwrap()
        .into_iter()
        .filter(|hit| hit.doc_type == "page")
        .map(|hit| hit.url)
        .collect()
}

/// A normal PDF's text is extracted by the child and searchable, under the
/// default limits (which, on Linux, include the 1 GiB memory limit).
#[test]
fn a_pdf_is_extracted_in_a_child_process() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wacz = wacz_with(tmp.path(), &[("https://ex.test/report.pdf", real_pdf())]);
    let home = tmp.path().join("home");
    let stderr = index(&home, &wacz, "");
    assert_eq!(
        urls_matching(&home, "\"flux capacitor\""),
        ["https://ex.test/report.pdf"],
        "{stderr}"
    );
}

/// One page drawing `segments` line segments: small on disk, but
/// `pdf-extract` parses every operator into memory at once, about 300 bytes
/// each. The shape of the CDC report that peaked at 2.2 GB.
#[cfg(target_os = "linux")]
fn pdf_drawing(segments: usize) -> Vec<u8> {
    let content = "0 0 l\n".repeat(segments);
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        write!(pdf, "{} 0 obj\n{body}\nendobj\n", i + 1).unwrap();
    }
    let xref = pdf.len();
    write!(pdf, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).unwrap();
    for offset in offsets {
        writeln!(pdf, "{offset:010} 00000 n ").unwrap();
    }
    write!(
        pdf,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    )
    .unwrap();
    pdf
}

/// A PDF that needs more memory than `index.pdf_memory_mb` is indexed by its
/// file name, and the next PDF in the same crawl is extracted as usual.
#[cfg(target_os = "linux")]
#[test]
fn a_pdf_past_its_memory_limit_is_indexed_by_name() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wacz = wacz_with(
        tmp.path(),
        &[
            ("https://ex.test/chart-heavy.pdf", pdf_drawing(2_000_000)),
            ("https://ex.test/report.pdf", real_pdf()),
        ],
    );
    let home = tmp.path().join("home");
    let stderr = index(&home, &wacz, "index:\n  pdf_memory_mb: 128\n");
    assert!(
        stderr.contains("128 MiB memory limit") && stderr.contains("indexed by its file name only"),
        "the stop should be logged with the limit: {stderr}"
    );
    assert_eq!(
        urls_matching(&home, "chart"),
        ["https://ex.test/chart-heavy.pdf"],
        "found by its file name"
    );
    assert_eq!(
        urls_matching(&home, "\"flux capacitor\""),
        ["https://ex.test/report.pdf"],
        "and the ingest carried on"
    );
}
