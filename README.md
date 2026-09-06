# indice

[![CI](https://github.com/edsu/indice/actions/workflows/ci.yml/badge.svg)](https://github.com/edsu/indice/actions/workflows/ci.yml)

**indice** is a web archive server written in Rust — a [reading room] for web
archives. Point it at a pile of local or remote [WACZ] files and it gives you
full-text faceted search, provenance up front, faithful in-browser replay, and a
management workroom — all from a single self-contained binary.

- 🌐 **Website & tour:** <https://indice.page>
- 📖 **Documentation:** <https://indice.page/docs/>

> **Nota bene:** *indice is alpha software and has been written extensively with
> the support of Claude Code. Like any piece of software it may contain bugs, and
> the developer's understanding of how it operates at a low level may be limited.
> See [DESIGN.md](DESIGN.md) for the overall design principles. Technical reviews
> of the code and design are always welcome!*

> **The web archive replay is entirely [Webrecorder]'s work.** indice bundles and
> serves [ReplayWeb.page] and [wabac.js] — the browser-side engine that does all
> the actual replay — and adds a thin Rust layer for indexing, search, and
> serving. Webrecorder did the heavy lifting; please support them.

## What you get

- **Full-text search with faceted, temporal browsing** — hit-highlighted
  snippets, then narrow by collection, site, date, type, or language, with a
  timeline for navigating through time.
- **Provenance up front** — see how each crawl was made (software, operator,
  dates, seeds, page counts) and verify each WACZ's fixity, instead of taking the
  archive on faith.
- **In-browser replay** of the archived pages via [ReplayWeb.page] / wabac.js.
- **A management workroom** where authenticated users add archives and edit
  collection metadata and descriptions.

It ships as a single self-contained binary — no Solr, no Elasticsearch, no
separate database server. That's a deliberate design goal: indice is built for
**small, local, and private** use (a person indexing a handful of their own WACZ
files on a laptop, nothing sent to a hosted service) and uses the same model to
**scale up** toward institutional collections.

## Install

Prebuilt binaries for macOS, Linux, and Windows are on the
[latest release](https://github.com/edsu/indice/releases/latest). Or:

```sh
brew install edsu/indice/indice                                     # Homebrew (macOS / Linux)
cargo install --git https://github.com/edsu/indice --locked indice  # cargo
```

Docker, building from source, and the macOS Gatekeeper note are covered in the
[install docs](https://indice.page/docs/install/).

## Try it in a minute

The prebuilt archive includes `apod.wacz` — a small sample crawl of NASA's
Astronomy Picture of the Day. Index it into a collection, then start the server:

```sh
indice index --collection "APOD" apod.wacz   # build the search index from the sample
indice serve                                 # http://127.0.0.1:8080
```

Open <http://127.0.0.1:8080> to full-text search the captured pages, narrow by
the facets, and replay the archived site in your browser. Point `indice index` at
your own `.wacz` files the same way (local paths or `http(s)://` URLs);
`indice serve --manage` adds an in-browser interface for adding and curating crawls.

## Documentation

The full manual lives at **<https://indice.page/docs/>**:

- [Install](https://indice.page/docs/install/) ·
  [Quick start](https://indice.page/docs/quickstart/) ·
  [Searching](https://indice.page/docs/guides/searching/)
- Importing from
  [Browsertrix](https://indice.page/docs/guides/import-browsertrix/) and
  [Archive-It](https://indice.page/docs/guides/import-archive-it/)
- [Manage & curate](https://indice.page/docs/guides/manage/) ·
  [Deploy & run](https://indice.page/docs/guides/deploy/) ·
  [Scale up](https://indice.page/docs/guides/scale/)
- Reference:
  [Command line](https://indice.page/docs/reference/cli/) ·
  [Operator configuration](https://indice.page/docs/reference/configuration/) ·
  [How indice works](https://indice.page/docs/reference/how-it-works/)

Architecture and design rationale: [DESIGN.md](DESIGN.md).

## Credits

indice stands almost entirely on the shoulders of [Webrecorder]. The hard part —
faithfully replaying an archived page in the browser — is done by their
[ReplayWeb.page] and [wabac.js] (which bundles wombat.js), both of which indice
ships and serves unmodified. It also builds on the open [WACZ] format and the
broader web-archiving community. If indice is useful to you, please support
Webrecorder's work.

## License

indice is licensed under the **GNU Affero General Public License v3.0 or later**
(AGPL-3.0-or-later) — the same license as the ReplayWeb.page and wabac.js
components it bundles. See [LICENSE](LICENSE) for the full text and
[NOTICE](NOTICE) for third-party attributions and bundled-asset details.

[WACZ]: https://specs.webrecorder.net/wacz/latest/
[Webrecorder]: https://webrecorder.net/
[ReplayWeb.page]: https://replayweb.page/
[wabac.js]: https://github.com/webrecorder/wabac.js
[reading room]: https://inkdroid.org/2026/06/03/jan6-doj-archive/
