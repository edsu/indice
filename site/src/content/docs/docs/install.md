---
title: Install
description: Install indice from a prebuilt binary, Homebrew, cargo, or a source clone.
---

indice is a single self-contained binary — ReplayWeb.page assets are embedded at build time, so there's nothing else to fetch or configure.

## Prebuilt binary (fastest — no toolchain)

Download the archive for your platform from the [latest release](https://github.com/edsu/indice/releases/latest) (macOS arm64/x86_64, Linux x86_64/arm64, Windows x86_64), unpack it, and you have the `indice` binary plus a small sample archive (`apod.wacz`) to try it on — see [Try it in a minute](/indice/docs/quickstart/).

:::caution[macOS Gatekeeper]
An unsigned download is quarantined by Gatekeeper. Clear it once with `xattr -d com.apple.quarantine ./indice` (notarized builds are planned).
:::

## With Homebrew (macOS / Linux)

```sh
brew install edsu/indice/indice
```

Installs the latest release binary from the [tap](https://github.com/edsu/homebrew-indice); `brew upgrade indice` picks up new releases. No Gatekeeper prompt — Homebrew's downloads aren't quarantined.

## With cargo

```sh
cargo install --git https://github.com/edsu/indice --locked indice
```

Builds and installs the `indice` command into `~/.cargo/bin` (needs a [Rust toolchain](https://rustup.rs)).

## From a clone (for development)

```sh
git clone https://github.com/edsu/indice
cd indice
cargo build --release
# binary at ./target/release/indice
```

The bundled ReplayWeb.page assets are committed to the repo, so a fresh clone builds and runs as-is. To upgrade them later, run `./scripts/fetch-replay.sh` and rebuild.
