---
title: How indice works
description: WACZ-direct replay, how indexing reads a WACZ, and the discovery/provenance research behind the reading-room model.
---

indice runs [ReplayWeb.page](https://replayweb.page/) in **WACZ-direct mode**. Rather than reimplementing web replay on the server (URL rewriting, redirect handling, fuzzy matching, serving individual archived resources), indice hands the whole job to the well-tested [wabac.js](https://github.com/webrecorder/wabac.js) service worker running in the browser:

```text
 indice index <files>                 indice serve
        │                                      │
        ▼                                      ▼
  [ Indexing ]                        [ Axum HTTP server ]
        │                                      │
        ├── page HTML ──► Tantivy      GET /             homepage + collections
        ├── WACZ metadata ─► Tantivy   GET /search?q=    search results + snippets
        └── datapackage ─► collections GET /api/search   search results as JSON
                             .json      GET /files/{id}   the WACZ, with byte-range
                                        GET /replay/…     ReplayWeb.page assets + viewer
                                        GET /collection/{id}/replay.json  collection replay manifest
                                        GET /collection/{id}/pages        page list + URL resolution
```

When you open a page for replay, the browser fetches the WACZ directly from `GET /files/{id}` using HTTP range requests, reads the CDX index embedded inside the WACZ, and serves every resource from the WARC records — all client-side. indice's job during replay is simply to serve bytes efficiently. Everything else (search, metadata, the collection homepage) is what indice is actually good at.

You can also **replay a whole collection** at once (from the homepage card or the collection page): `GET /collection/{id}/replay.json` hands wabac.js a multi-WACZ manifest listing every member crawl, so a link from one crawl to a page archived in another crawl *of the same collection* resolves on demand. That resolution is answered from the search index via `GET /collection/{id}/pages` — which also feeds the viewer's page-list sidebar — so the browser never has to load every member's index, and it scales from a handful of WACZs to institutional collections. Resolution is scoped to the collection, and page sub-resources always come from the page's own crawl, preserving per-page temporal coherence.

See [DESIGN.md](https://github.com/edsu/indice/blob/main/DESIGN.md) for the full architecture.

## How indexing reads a WACZ

By default indice reads a WACZ through its internal **CDX index**, fetching only the records that become pages (HTML, PDFs, and Browsertrix's rendered `urn:text`) and skipping images, video, JS, and CSS. It also reads the fully rendered page text from `pages/pages.jsonl` and `pages/extraPages.jsonl` — many crawls store the post-JS text only there, so this keeps JS-rendered content searchable, not just visible in replay. This works the same way for local and remote WACZs — the only difference is *how* the bytes are read: a remote WACZ over HTTP range requests (no download), a local WACZ straight from the file.

It falls back to a **full scan** of every WARC record only when a WACZ can't be read via its CDX — its WARCs are stored compressed (the WACZ spec says the `archive/` WARCs *should* be stored uncompressed so they can be read by offset; a few tools don't), or it has no readable CDX. For a remote WACZ whose host doesn't support range requests, the fallback downloads a temporary copy and scans it.

indice trusts the CDX because **replay already does**: the in-browser player resolves each record through the CDX, so a WACZ with a broken CDX wouldn't replay anyway. Indexing from the same index keeps the two consistent.

## Discovery and provenance

The "reading room" idea is that you should be able to *find* things in an archive and *understand* what you're looking at — not just replay a URL you already know. Two findings from web-archiving research shape indice (both expanded, with citations, in [DESIGN.md](https://github.com/edsu/indice/blob/main/DESIGN.md)):

- **Web-archive use is mostly navigational and temporal** — seeing a page or site as it was, or how it changed over time (Costa &amp; Silva's query-log study of the Portuguese Web Archive). So time is a first-class axis, and facets beat one long scrolling list as an archive grows. indice has a faceted results page (collection, site, date, type, language), a month timeline, and grouping of repeat captures of the same URL — the faceted, full-text "slice and dice" browsing that [SHINE](https://github.com/ukwa/shine) (UK Web Archive) and [SolrWayback](https://github.com/netarchivesuite/solrwayback) (Royal Danish Library) established over the [warc-indexer](https://github.com/ukwa/webarchive-discovery). indice owes both a clear debt; it just trades their Solr backend for a single embedded Tantivy index — so the same faceted search runs with no cluster to operate, fitting a private laptop archive as readily as an institutional one.
- **Provenance is part of the record** — to trust and interpret an archive you need to know how it was made: the crawler software, operator, dates, and seeds (Maemura et al., *If These Crawls Could Talk*). indice reads this from the WACZ and WARC and surfaces it on each collection and WACZ — and lets you verify each file's fixity — rather than burying it.
