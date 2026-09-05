---
title: Building & testing
description: Build indice from source and run its test suite, including the headless-browser replay test.
---

Build from a clone (see also [Install → From a clone](/indice/docs/install/#from-a-clone-for-development)):

```sh
git clone https://github.com/edsu/indice
cd indice
cargo build --release   # binary at ./target/release/indice
```

## Testing

```sh
cargo test              # unit + integration tests (no browser needed)
```

Most tests run without a browser, including server-side *replay-contract* tests that assert what wabac.js depends on: the WACZ we serve is byte-identical to disk, byte-range requests return the correct slice, the served archive's CDX resolves a page, and the viewer wires up `<replay-web-page>` correctly.

Actual replay rendering can only be checked in a real browser, so there's one `#[ignore]`d end-to-end test that drives headless Chrome via WebDriver and confirms an archived page renders from a WACZ we serve:

```sh
chromedriver --port=9515 &          # WebDriver server; must match your Chrome's major version
cargo test -p indice-lib --test browser -- --ignored
```

- Override the WebDriver endpoint with `WEBDRIVER_URL` (default `http://localhost:9515`).
- `chromedriver`'s major version must match your installed Chrome. If they differ, grab a matching build from [Chrome for Testing](https://googlechromelabs.github.io/chrome-for-testing/).
- On macOS, a Homebrew `chromedriver` is quarantined and gets killed on launch; clear it once with `xattr -d com.apple.quarantine $(which chromedriver)`.
