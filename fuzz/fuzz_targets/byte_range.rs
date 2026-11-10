//! A `Range` header on a WACZ download, against a file size from the input.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| indice_lib::fuzz::byte_range(data));
