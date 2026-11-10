//! A decompressed WARC record slice, through the record and field parsers.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| indice_lib::fuzz::warc(data));
