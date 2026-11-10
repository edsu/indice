//! One record's bytes as a CDX lookup fetches them: a gzip member, which may
//! be a decompression bomb, or a plain record.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| indice_lib::fuzz::warc_slice(data));
