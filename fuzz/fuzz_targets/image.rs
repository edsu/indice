//! Captured images, as thumbnailing decodes and re-encodes them.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| indice_lib::fuzz::image(data));
