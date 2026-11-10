//! Raw bytes as a WACZ: mostly finds what the ZIP layer makes of a damaged
//! archive. `wacz_members` reaches the members' contents.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| indice_lib::fuzz::wacz(data));
