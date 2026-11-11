# Fuzzing indice's parsers

These are [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the code that reads untrusted archive bytes. Each target calls an entry point in `indice_lib::fuzz` (`crates/indice-lib/src/fuzz.rs`), which runs a parser the way indexing does and throws the result away. A finding is a panic, an abort, a hang, or memory growing past what the parser's limit should allow.

| Target | What it feeds |
| --- | --- |
| `warc` | Decompressed WARC records, and `warcinfo` fields |
| `warc_slice` | One record's bytes as a CDX lookup fetches them, gzipped or plain |
| `wacz` | Whole WACZ files: `datapackage.json`, the page lists, nested WACZs, stored WARCs, `warcinfo`, the CDX index |
| `wacz_members` | WACZs built from the member names indice looks for, so the fuzzer spends its time on member contents rather than on ZIP structure |
| `search` | Search queries up to the server's 2 KiB limit, against a small index built once per run |
| `byte_range` | `Range` headers on a WACZ download, checking that what is served lies inside the file |
| `markdown` | Curator Markdown, checking that the HTML has no tag, attribute or link scheme that could run script (`indice_lib::fuzz::unsafe_html`) |
| `html` | HTML responses, as indexing reads them for text and thumbnailing reads them for images |
| `image` | Captured images, as thumbnailing decodes, downscales and re-encodes them |

`search`, `byte_range` and `markdown` take input anyone who reaches a server can send, or, for Markdown, that every visitor's browser renders. The `search` index has two pages, so the target finds panics and parser hangs, not queries that are slow on a large index. Run it with `cargo +nightly fuzz run -O --sanitizer none search`. `-O` builds as indice ships, without debug assertions: Tantivy 0.27's phrase scorer has a `debug_assert!` that a negated phrase repeating a term, such as `-"sea sea"`, trips within minutes, though a release build answers that query correctly. With AddressSanitizer on, the process grows past `-rss_limit_mb` in under an hour from the sanitizer's own bookkeeping; without it, memory grew from 98 to 104 MB over an hour and 4.7 million inputs. The search path is safe Rust, which is what AddressSanitizer would add the least to.

## Running

cargo-fuzz needs nightly Rust, for the coverage instrumentation and AddressSanitizer flags it passes to the compiler. The fuzz crate is its own workspace, so the main build never compiles it. Its `Cargo.lock` is not committed: copy the main one in before building, so that what is fuzzed is the dependency versions indice ships, and copy it again after the main one changes.

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
cd fuzz
cp ../Cargo.lock Cargo.lock
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
