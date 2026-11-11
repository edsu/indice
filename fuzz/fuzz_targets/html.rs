//! HTML responses, as indexing and thumbnailing parse them.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| indice_lib::fuzz::html(data));
