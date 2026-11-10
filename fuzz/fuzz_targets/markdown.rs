//! Curator Markdown, checked for anything that could run script.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| indice_lib::fuzz::markdown(data));
