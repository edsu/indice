---
title: indice
description: A reading room for web archives — full-text search, provenance, and faithful in-browser replay from a single binary.
---

**indice** is a web archive server written in Rust — think of it as a [reading room](https://inkdroid.org/2026/06/03/jan6-doj-archive/) for web archives. Point it at a pile of local or remote [WACZ](https://specs.webrecorder.net/wacz/latest/) files and it gives you:

- **Full-text search with faceted, temporal browsing** — hit-highlighted snippets, then narrow by collection, site, date, type, or language, with a timeline for navigating through time.
- **Provenance up front** — see how each crawl was made (software, operator, dates, seeds, page counts) and verify each WACZ's fixity, instead of taking the archive on faith.
- **In-browser replay** of archived pages via [ReplayWeb.page](https://replayweb.page/) / wabac.js.
- **A management workroom** where authenticated users edit collection metadata and descriptions.

It ships as a single self-contained binary — no Solr, no Elasticsearch, no separate database server. That's a deliberate design goal: indice is built for **small, local, and private** use (a person indexing a handful of their own WACZ files on a laptop, with nothing sent to a hosted service) and uses the same model to **scale up** toward institutional collections. It aims to fit both ends of that range, rather than assuming the infrastructure of a large web archive.

:::note[Replay is Webrecorder's work]
The web-archive replay is entirely [Webrecorder](https://webrecorder.net/)'s work. indice bundles and serves [ReplayWeb.page](https://replayweb.page/) and [wabac.js](https://github.com/webrecorder/wabac.js) — the browser-side engine that does all the actual replay — and adds a thin Rust layer for indexing, search, and serving. Webrecorder did the heavy lifting; please support them.
:::

## Where to start

- **New here?** [Install](/indice/docs/install/) indice, then [try it in a minute](/indice/docs/quickstart/).
- **Bringing archives in?** Import from [Browsertrix](/indice/docs/guides/import-browsertrix/) or [Archive-It](/indice/docs/guides/import-archive-it/).
- **Curating?** [Manage &amp; curate in the workroom](/indice/docs/guides/manage/).
- **Running a server?** [Deploy indice](/indice/docs/guides/deploy/).
- **Digging deeper?** The [command-line reference](/indice/docs/reference/cli/) and [how indice works](/indice/docs/reference/how-it-works/).

## Why "indice"?

An *indice* is a sign that points beyond itself, and the name gathers three senses of the same idea. [Suzanne Briet](https://en.wikipedia.org/wiki/Suzanne_Briet) argued that a wild antelope becomes a *document* once it is captured, catalogued, and set aside as evidence. She defined a document as *"un indice concret ou symbolique, conservé ou enregistré"* (a concrete or symbolic sign, preserved or recorded). [Charles Sanders Peirce](https://plato.stanford.edu/entries/peirce-semiotics/) used *index* for the same family of sign: one bound to its object by a real, existential connection like smoke to fire, or a weathervane to the wind. Squint a little and a web capture is like that too: a trace connected to a moment of the live web. And, of course, indice builds a full-text **index** over the archives it serves, so the simplest meaning applies too.
