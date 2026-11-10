//! A search query, as anyone reaching the server can send one.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| indice_lib::fuzz::search(data));
