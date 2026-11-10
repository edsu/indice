# Fuzzing indice's parsers

These are [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the code that reads untrusted archive bytes. Each target calls an entry point in `indice_lib::fuzz` (`crates/indice-lib/src/fuzz.rs`), which runs a parser the way indexing does and throws the result away. A finding is a panic, an abort, a hang, or memory growing past what the parser's limit should allow.

| Target | What it feeds |
| --- | --- |
| `warc` | Decompressed WARC records, and `warcinfo` fields |
| `warc_slice` | One record's bytes as a CDX lookup fetches them, gzipped or plain |
| `wacz` | Whole WACZ files: `datapackage.json`, nested WACZs, stored WARCs, `warcinfo`, the CDX index |
| `wacz_members` | WACZs built from the member names indice looks for, so the fuzzer spends its time on member contents rather than on ZIP structure |

## Running

cargo-fuzz needs nightly Rust, for the coverage instrumentation and AddressSanitizer flags it passes to the compiler. The fuzz crate is its own workspace, so the main build and CI never compile it.

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
cd fuzz
./seed.sh    # copies the test fixtures into corpus/ as starting inputs
ASAN_OPTIONS=quarantine_size_mb=32 cargo +nightly fuzz run wacz -- \
    -max_total_time=3600 -rss_limit_mb=1024 -malloc_limit_mb=256 \
    -timeout=10 -max_len=1048576
```

What the settings are for:

- `-malloc_limit_mb=256` stops on any single allocation over 256 MB and prints its stack trace. The targets read with an 8 MiB limit (`indice_lib::fuzz::LIMIT`), so an allocation that large is sized from something in the input, not from data that is there.
- `-rss_limit_mb=1024` stops when the process as a whole grows past 1 GiB. libFuzzer checks this itself, so it works on macOS as well as Linux. It reports whichever input was running when the limit tripped, which may not be the one that used the memory: replay the artifact alone before believing it.
- `ASAN_OPTIONS=quarantine_size_mb=32` shrinks the memory AddressSanitizer holds back from reuse to catch use-after-free. Its default quarantine is large enough that a long run reaches the RSS limit on its own.
- `-timeout=10` counts an input that takes more than 10 seconds as a hang.
- `-max_len=1048576` lets inputs grow to 1 MiB, enough for a WACZ with a few members.

## A finding

libFuzzer writes the input to `artifacts/<target>/`. Replay it alone:

```sh
cargo +nightly fuzz run wacz artifacts/wacz/<file> -- -malloc_limit_mb=256
```

A finding someone could use against a running server goes in a private security advisory, not an issue (see `SECURITY.md`). Fix it with a regression test beside the parser, and keep the artifact out of the repository until the fix is released.
