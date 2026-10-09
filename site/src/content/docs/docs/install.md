---
title: Install
description: Install indice from a prebuilt binary, Homebrew, cargo, a container image, or a source clone.
---

indice is a single self-contained binary, so there's nothing else to fetch or configure.

## Prebuilt binary

You can download the archive for your platform from the [latest release](https://github.com/edsu/indice/releases/latest) (macOS arm64/x86_64, Linux x86_64/arm64, Windows x86_64), unpack it, and you have the `indice` binary plus a small sample archive (`apod.wacz`) to try it on; see [Try it in a minute](/docs/quickstart/).

:::caution[macOS Gatekeeper]
An unsigned download is quarantined by Gatekeeper. Clear it once with `xattr -d com.apple.quarantine ./indice` (notarized builds are planned).
:::

## With Homebrew (macOS / Linux)

```sh
brew install edsu/indice/indice
```

Install the latest release binary from the [tap](https://github.com/edsu/homebrew-indice); `brew upgrade indice` picks up new releases. No Gatekeeper prompt, and Homebrew's downloads aren't quarantined.

## With cargo

```sh
cargo install --git https://github.com/edsu/indice --locked indice
```

Builds and installs the `indice` command into `~/.cargo/bin` (needs a [Rust toolchain](https://rustup.rs)).

## Container image

```sh
docker run -p 127.0.0.1:8080:8080 -v indice-data:/data ghcr.io/edsu/indice:latest \
  serve --bind 0.0.0.0:8080 --home /data
```

A multi-arch image (`linux/amd64` and `linux/arm64`) is published to the GitHub Container Registry on every release. It carries no default command, because serving on a public interface needs an authenticating proxy and indice refuses to start without one, so `docker run` with no arguments prints the help instead of guessing.

Publishing that port to anything other than loopback puts an unauthenticated write surface on your network. For a real server, use the [deployment stack](/docs/guides/deploy/), which puts a proxy and an identity provider in front.

## From a clone (for development)

```sh
git clone https://github.com/edsu/indice
cd indice
cargo build
# binary at ./target/debug/indice
```

The bundled ReplayWeb.page assets are committed to the repo, so a fresh clone builds and runs as-is. To upgrade them later, run `./scripts/fetch-replay.sh` and rebuild.
