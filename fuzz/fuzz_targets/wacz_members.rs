//! A WACZ built from fuzzed members under the names indexing looks for, so
//! the fuzzer spends its time on `datapackage.json`, the page lists, the CDX
//! index and the WARCs rather than on finding a valid ZIP. A member can be
//! gzipped here, since a fuzzer seldom writes a valid gzip stream itself.
#![no_main]

use std::io::Write;

use arbitrary::Arbitrary;
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

const NAMES: &[&str] = &[
    "datapackage.json",
    "pages/pages.jsonl",
    "pages/extraPages.jsonl",
    "archive/data.warc",
    "archive/data.warc.gz",
    "indexes/index.cdx.gz",
    "indexes/index.cdxj",
    "nested/crawl.wacz",
];

#[derive(Arbitrary, Debug)]
struct Member {
    name: u8,
    /// Deflated in the ZIP, rather than stored as a WACZ's WARCs must be.
    deflate: bool,
    /// Gzipped before it goes in, as `.gz` members are.
    gzip: bool,
    bytes: Vec<u8>,
}

libfuzzer_sys::fuzz_target!(|members: Vec<Member>| {
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for m in &members {
        let method = if m.deflate {
            CompressionMethod::Deflated
        } else {
            CompressionMethod::Stored
        };
        let name = NAMES[m.name as usize % NAMES.len()];
        // The writer refuses a name twice; that input is no use.
        if zw
            .start_file(name, SimpleFileOptions::default().compression_method(method))
            .is_err()
        {
            return;
        }
        let bytes = if m.gzip {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gz.write_all(&m.bytes).unwrap();
            gz.finish().unwrap()
        } else {
            m.bytes.clone()
        };
        zw.write_all(&bytes).unwrap();
    }
    let data = zw.finish().unwrap().into_inner();
    indice_lib::fuzz::wacz(&data);
});
