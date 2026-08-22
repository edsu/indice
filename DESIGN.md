# indice - Design Document

indice is a minimal, high-performance web archive server written in Rust. It provides full-text search over WACZ collections and serves them for in-browser replay via the ReplayWebPage/wabac.js service worker in **WACZ-direct mode** - the mode where the browser reads the archive directly, without a server-side proxy interpreting individual resource requests.

A guiding design goal is **range**: indice should serve small, local, and private use - an individual indexing a handful of their own WACZ files on a laptop, with nothing sent to a hosted service - and use the same model to scale up toward institutional collections. Peer tools like SHINE and SolrWayback assume the infrastructure of a large web archive (a Solr cluster); indice deliberately does not, so it fits both ends of that range. This is why it ships as one binary with an embedded index, and why the two-level collection model (below) is built to serve both a solo curator and an institution reorganizing TBs of WARC.

Scope:
- Index WACZ files into a local full-text search index
- Serve WACZ files with byte-range support so wabac.js can read them directly
- Implement full-text search with hit-highlighted snippets
- Surface WACZ metadata (title, description, crawl date, seed pages) on the homepage
- Ship as a single self-contained binary (no Solr, no Elasticsearch, no separate database server)

---

## Architecture Overview

```
indice index <files>                  indice serve
       │                                       │
       ▼                                       ▼
  [Indexing pipeline]               [Axum HTTP server]
       │                                       │
       ├── HTML text ──► Tantivy              ├── GET /             → homepage
       ├── WACZ metadata ──► Tantivy          ├── GET /search?q=    → search results
       └── manifest ───► collections.json     ├── GET /api/search   → search JSON
                                              ├── GET /files/{id}   → WACZ byte-range
                                              └── GET /replay/viewer → viewer shell
```

Replay is handled entirely by the wabac.js service worker running in the browser. The service worker reads the WACZ file from `GET /files/{id}` using HTTP byte-range requests, extracts the CDX index from `indexes/index.cdx.gz` inside the ZIP, loads it into browser IndexedDB, and fetches individual WARC records by offset - all without making per-resource requests back to the indice server. indice's job during replay is purely to serve bytes efficiently.

---

## Cargo Workspace Layout

```
indice/
├── Cargo.toml               (workspace root with [workspace.dependencies])
├── crates/
│   ├── indice-lib/        (all logic - importable in tests)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── index.rs     - Indexing pipeline orchestration
│   │       ├── search.rs    - Tantivy schema, indexing, query execution, snippets
│   │       ├── server.rs    - Axum router and all HTTP handlers
│   │       ├── collections.rs - Collection manifest (collections.json)
│   │       ├── warc.rs      - WARC record iteration and HTML extraction
│   │       ├── wacz.rs      - WACZ ZIP handling, datapackage.json, CDX reader
│   │       ├── thumbnail.rs - Representative-image thumbnails (og:image → cached JPEG)
│   │       └── http_range.rs - Read+Seek over HTTP range requests (remote streaming)
│   └── indice-bin/        (thin CLI entry point)
│       └── src/main.rs      - Clap CLI, subcommand dispatch, tokio::main
└── static/replay/           (ReplayWebPage assets - embedded at compile time)
```

---

## CLI Interface

```
indice index          [--home <DIR>] [--name <NAME>] --collection <NAME> [-f|--from-file <FILE>] [--download] [--concurrency <N>] [-v|--verbose] <PATH|URL>...
indice reindex        [--home <DIR>] [--concurrency <N>] [-v|--verbose]
indice optimize       [--home <DIR>] [--max-segments <N>] [-v|--verbose]
indice serve          [--home <DIR>] [--bind <ADDR>]
indice collection set [--home <DIR>] <NAME> [--description <TEXT>] [--curator <TEXT>] [--creator <TEXT>] [--dates <TEXT>] [--rights <TEXT>] [--subject <SUBJECT>]... [--narrative <MD> | --narrative-file <FILE>] [--thumbnail <FILE>]
indice collection list[--home <DIR>]
indice crawl set      [--home <DIR>] <CRAWL_ID> [--image <FILE>] [--note <MD> | --note-file <FILE>]
indice crawl list     [--home <DIR>] [<COLLECTION>]
indice search-url     [--home <DIR>] <URL>
indice verify         [--home <DIR>]
indice import browsertrix [--home <DIR>] [--host <URL>] [--org <SLUG>] [--public] [--collection <ID|SLUG>] [--crawl <ID>] [--into <NAME>] [--include-unreviewed] [--min-review <N>] [--limit <N>] [--concurrency <N>] [--dry-run] [--stream] [--force] [-v]
indice import archive-it [--home <DIR>] [--host <URL>] [--collection <ID>] [--crawl <ID>] [--into <NAME>] [--crawl-time-after <DATE>] [--crawl-time-before <DATE>] [--limit <N>] [--dry-run] [--include-deleted] [--force] [-v]
indice wacz build     [--home <DIR>] --collection <NAME> [--name <NAME>] [--title <T>|--title-file <FILE>] [--description <D>|--description-file <FILE>] [--creator <TEXT>] [--software <TEXT>] [--main-page-url <URL>] [--keyword <K>]... [--license <L>]... [--yes] [-v] <WARC>...
```

Every command takes `--home <DIR>` (default `.`). The home directory holds
`<home>/archive/` (WACZ files) and the derived `<home>/index/` (Tantivy index +
`waczs.json`), alongside the git-committable curatorial `<home>/collections/`
and `<home>/crawls/` finding aids (see *Collection Management*). Keeping them
together makes a home folder portable - move it to another disk or machine and
it still resolves.

- `index`: indexes one or more `.wacz` files or `http(s)://` URLs - at least one argument is required. **CDX-guided extraction is the default** for every WACZ (local or remote), reading only the records the CDX lists; a remote URL is read over HTTP range requests with no download, a local file straight from disk (see *Indexing Pipeline*). It falls back to a full WARC scan only when a WACZ can't be CDX-guided (deflated WARCs, or no readable CDX). `--download` fetches a remote WACZ into `<home>/archive/<collection-slug>/` for a durable local copy instead of streaming it in place. A local WACZ may live anywhere: indice files it into `<home>/archive/<collection-slug>/` — **moving** it if it already sits under `archive/` (reorganizing within its own space), **copying** it otherwise (leaving the original intact) — and stores the source relative to home, so the home directory stays self-contained and the archive is browsable by collection. A directory or a non-`.wacz` file is an error with guidance; index several at once with a shell glob. Extracts searchable page text (HTML, rendered `urn:text` or `pages/*.jsonl` text, PDFs), reads `datapackage.json` + `warcinfo` for provenance, records the SHA-256 of each local WACZ, and updates the manifest. **`--collection <NAME>` is required** — every crawl belongs to a curated collection (created if new); there are no auto singletons. Indexing a glob into one collection (`indice index archive/*.wacz --collection "…"`) makes that a decide-once cost. `--from-file <FILE>` (or `-f -` for stdin) reads a newline-delimited list of files/URLs, ignoring blank lines and `#` comments, and combines with any positional args. Progress is shown as a bar on an interactive terminal; `-v`/`--verbose` replaces it with `DEBUG` logs (see *Indexing Pipeline → Progress reporting*). (A bare `index` with no arguments prints guidance pointing to `index archive/*.wacz` and `reindex`.)
- `reindex`: rebuild the full-text index from the sources already recorded in the manifest, preserving collection membership and metadata. Unlike `index`, this re-indexes every registered source - including remote URLs, which are re-fetched - and recreates the Tantivy index from scratch, so a schema change is picked up. It is *resilient*: a source that can't be indexed - a missing local file, or a remote source still failing after the retry budget - is skipped with a warning rather than aborting the whole rebuild, so one bad source can't torch a long reindex over many. The skipped source's manifest entry is preserved, the mostly-rebuilt index is still committed (usable), and if anything was skipped the command exits non-zero with a summary count - so a partial rebuild is visible to a human *and* to cron/CI, and re-running once the cause is fixed picks the skipped sources back up. Like `index`, it takes `--concurrency <N>` (records fetched at once per source) and shows the same per-WACZ progress bar on an interactive terminal (`-v`/`--verbose` swaps it for `DEBUG` logs) - welcome here since a full reindex re-streams every source. This is the intended way to migrate the index after a schema change (see below).
- `optimize`: compacts the full-text index in place by **merging Tantivy segments** down toward `--max-segments` (default 8), **without re-reading any sources** — far cheaper than `reindex`. Every query (and `facet_overview`, and URL-grouping) fans out across *all* segments, so an index that has fragmented into hundreds/thousands of tiny segments — which happens when Tantivy's background merges fail (classically, on a full disk, since a merge needs transient ~2× space) — makes every search slow. `optimize` merges smallest-first in bounded batches, waiting for each merge before the next, so peak transient disk stays ~one batch rather than a second copy of the whole index; a smaller `--max-segments` compacts more but raises that peak (~index size / target). Needs a writer (takes the write lock) and some free disk; reports `before → after` segment counts. (`SearchIndex::segment_count()` exposes the health signal.) You rarely run it by hand: `index` and the server's bulk import **auto-compact** at the end of a run when they detect the index has fragmented past `FRAGMENTED_SEGMENT_THRESHOLD` (`index::optimize_if_fragmented`), so a big batch leaves a tidy index without the operator remembering — `index --no-optimize` opts out (and instead logs a reminder), and a healthy index or a single incremental add stays under the threshold and is left untouched. `serve` warns at startup if the index it opens is fragmented. (Running out of disk mid-ingest is *not* a correctness problem — Tantivy's commit is atomic, so an uncommitted WACZ simply re-indexes on re-run; the only symptom is a valid-but-fragmented index, which the auto-compaction and warnings now surface. A free-space preflight and segment-creation throttling were considered and deliberately dropped as premature; see `rustyweb-scale-footprint-qw5`.)
- `serve`: opens Tantivy read-only (so `index` can run concurrently), starts Axum. Defaults: `127.0.0.1:8080`.
  - `serve --manage` (opt-in, default **off**) mounts a small **management** write surface on top of the read-only site. Default `serve` mounts none of this and stays strictly read-only. Endpoints + UI:
    - `POST /api/archives` — reuses the exact `index::index_location` path the CLI uses to add a crawl to a collection (a local path is copied into `archive/`; an `http(s)://` URL is streamed in place). Runs on a blocking thread (returns a job id immediately), streams progress over Server-Sent Events at `GET /api/archives/{id}/events`, and on success **hot-reloads** the read-only searcher (held behind an `RwLock<Arc<SearchIndex>>`, swapped for a freshly-opened index) so new results appear without a restart. The server still never holds Tantivy's write lock itself — the ingest opens its own short-lived writer, exactly as the CLI does.
    - `POST /api/archives/upload` — same as above but for a **browser file upload** (multipart/form-data): the `.wacz` bytes are streamed to a temp file, indexed exactly like a local path (copied into `archive/`), then the temp is deleted. The 2 MB default body limit is lifted on this route only. Reuses the same job + SSE + reload machinery as `/api/archives`.
    - `POST /api/collections` — create/edit a collection finding aid (wraps `index::set_collection`); form-encoded, POST-redirect-GET. No searcher reload needed (the homepage re-reads the manifest per request).
    - `POST /api/crawls/{id}/delete` · `POST /api/collections/{id}/delete` — remove content (wraps `index::delete_crawl` / `delete_collection`). A crawl delete drops its documents from Tantivy (delete-by-`crawl_id` term + commit), removes its manifest entry and — for a `File` source, when no other entry references the file — its local WACZ + thumbnail, then hot-reloads the searcher. Order is docs → files → manifest entry so a crash mid-delete is safe to re-run. A collection delete removes the grouping (empty by default; `with_crawls` also deletes every member crawl). Takes the same write lock as an add; a red *danger zone* on the crawl/collection pages gates it behind expand-then-confirm. Also `indice crawl delete <id>` / `collection delete <id|name> [--with-crawls]` on the CLI (with a `--yes`-skippable prompt). Tantivy reclaims disk only on a later segment merge, so the index won't shrink immediately.
    - **Edit-in-place UI.** There is no separate management area: under `--manage` the ordinary pages become an editable *workroom* (a warm clay "red-tape" accent flip marks write mode — chosen to read as an "attention: you can change things" cue while staying clear of the semantic error red). The homepage collection list gains **+ New collection** and per-card **Edit**; each collection page gains **Edit collection** and **+ Add crawls**. Only the two multi-step accessions have dedicated pages — the finding-aid form (`GET /manage/collections/new`, `GET /manage/edit/{id}`) and the add-crawls desk (`GET /manage/add?collection=…`, with source tabs: upload, path-URL, and the Browsertrix and Archive-It browse-and-import wizards). One design system, two accent modes (reading-room blue / workroom clay); buttons use one outline treatment whose color follows the mode; no separate stylesheet.
    - **Auth.** Two modes:
      - **Local** (default `--manage`): every request is trusted — the local operator is the admin, no login. Because it trusts everything, the server *refuses to start* if `--manage` (without an auth proxy) is bound to a non-loopback address; local mode must bind `127.0.0.1`/`::1`.
      - **Forward-auth** (`--auth-proxy-header <HEADER> --auth-proxy-secret <SECRET>`): for running as a service. indice sits behind an authenticating reverse proxy that performs the real login (SSO/OIDC/SAML) and injects the authenticated user in `<HEADER>` (e.g. `X-Forwarded-Email`). Management routes are wrapped in middleware that allows a request only if it carries a non-empty identity in that header **and** the shared `SECRET` in `X-Indice-Auth-Secret` (a static header the proxy adds — *not* the IdP). Requiring the secret is what makes trusting the identity header safe: a client that forges the identity header, or a request that skipped the proxy, lacks the secret and gets 403. indice stores no passwords and speaks to no IdP; "who is an admin" is delegated entirely upstream (anyone the proxy authenticates). The signed-in user is shown in the workroom strip on every management page. The public read-only site is *not* gated (its read handlers only render the workroom controls to an authenticated admin). Deploy note: bind indice to loopback and have the proxy reach it there, and ensure the proxy strips any client-supplied copy of the identity header.
    - **Browsertrix import** (a source tab on the accession desk): browse endpoints (`GET /api/browsertrix/orgs · collections · items`) list what the configured credentials can see; `POST /api/browsertrix/import` streams the selected crawls into a collection via the shared job/SSE/reload machinery (`Source::Browsertrix` + the replay resolver). Credentials **and the target host** stay in the binary — a `BrowsertrixProvider` (the lib/bin boundary, like `SourceResolver`) builds authenticated clients from `BROWSERTRIX_*` / `BROWSERTRIX_HOST`; the host is *never* taken from the request, so a client can't redirect the server's token elsewhere. v1 streams (index-only); durable download is a follow-up.
    - **Archive-It import** (a source tab on the accession desk): the same shape as Browsertrix. Browse endpoints (`GET /api/archiveit/collections · crawls?collection=<id>`) list what the configured credentials can see, marking crawls already imported; `POST /api/archiveit/import` downloads the selected crawls' WARCs, builds one WACZ per crawl, and indexes them via the shared job/SSE/reload machinery — reusing `archiveit::import_crawls`, the same orchestrator the CLI drives. Credentials **and host** stay in the binary behind an `ArchiveItProvider` (`ARCHIVEIT_*` / `ARCHIVEIT_HOST`), never taken from the request. Both providers are bundled into a `server::Providers` value threaded through `serve`/`build_router`, so a new import source adds a field rather than a parameter to every constructor.
- `collection set` / `collection list`: create/update a collection's finding-aid metadata / list collections and their members. `collection set` writes the structured front-matter fields (`--creator`/`--dates`/`--rights`/`--subject`, plus `--description`/`--curator`), the narrative body (`--narrative[-file]`), and an optional `--thumbnail` to a committable `collections/<slug>/README.md` (+ `thumbnail.jpg`); a curator can also just hand-edit those files (see *Collection Management*). (WACZ→collection membership is set at index/import time via `index --collection`.)
- `crawl set`: set curator-controlled crawl properties — `--image` pins a representative thumbnail (`collections/<slug>/crawls/<id>.jpg`), `--note[-file]` writes a committable Markdown note to `collections/<slug>/crawls/<id>.md`.
- `crawl list [<COLLECTION>]`: list crawls (id, page count, capture date, name) grouped by collection, optionally filtered to one (matched by name or slug). Reads the manifest; the ids feed `crawl set`/`crawl delete`. (`collection list` is collection-level only, so this is how you find a crawl's id from the CLI.)
- `search-url`: opens each indexed WACZ, reads its internal `indexes/index.cdx.gz`, and prints all CDX records matching the given URL. Useful for debugging - does not require the CDX to be separately indexed.
- `verify`: re-hashes every WACZ in the manifest and compares against the stored SHA-256, reporting each as `OK`, `MODIFIED`, or `MISSING`. Exits non-zero on any failure so it can run unattended (cron/CI). This is the fixity check for the archive.
- `import <source>`: a group of importers that pull content from external web-archiving services (each source is its own subcommand, since their auth and selection differ; grouped so sibling sources are not new top-level verbs). `import archive-it` pulls from an [Archive-It](https://archive-it.org/) account (credentials from `ARCHIVEIT_USER`/`ARCHIVEIT_PASSWORD` in the environment, never argv). Unlike Browsertrix, Archive-It serves **WARC files**, not WACZ, over two Basic-auth APIs on one host: the **Partner API** (`/api/collection`, `/api/crawl_job` — collections, descriptive metadata, and crawl status/dates) and **WASAPI** (`/wasapi/v1/webdata` — the WARC file records, with download `locations`, checksums, and each file's `crawl` id + `crawl-time`; cursor-paginated via `next`). Selection is **crawl-centric**: WASAPI lists the WARCs and `--collection <ID>` (numeric) / `--crawl <ID>` / `--crawl-time-{after,before}` are filters; files are grouped by crawl (`plan_crawls`), and **one WACZ is built per crawl** (bundling that crawl's WARCs) via the shared `wacz_build::build_wacz`, then indexed as a durable local (File) source under `<home>/archive/<slug>/`. So there is **no new `Source` variant and no resolver** — the download yields a plain local WACZ (Archive-It's WARC URLs are durable, not expiring presigned ones like Browsertrix's). Downloads stage under `<home>/.import-tmp/<crawl>/` (same volume as `archive/`, so the build files in place; cleaned per crawl, even on error). Only **finished, non-deleted** crawls are imported — WASAPI has no status, so the Partner API's `crawl_job` (`status` / `test_crawl_state` / `type`) is the source of truth; `--include-deleted` overrides. **Incremental:** an `ArchiveItRef` provenance on each crawl (`archive_it` in the manifest: host, collection id, crawl id, WARC count, collection title) lets a re-run skip already-imported crawls unless `--force`. Each built WACZ also embeds an **allowlisted** subset of the source crawl+collection records in its `datapackage.json` under an `archiveit` object (allowlist, not denylist, because every collection record carries a per-collection `private_access_token` secret plus operator PII — copying only named descriptive fields means a new upstream field can never leak into a shareable WACZ). Metadata seeds the finding aid (`collection_fields`: the collection title/description, and `topics` humanized into a subject — descriptive `metadata` is empty account-wide in practice, so the **title** is the useful signal, also surfaced in the crawl display). Grouping mirrors Browsertrix: an Archive-It `--collection` maps to an indice collection of the same name, `--into <NAME>` overrides (and reaches crawls in no Archive-It collection); `--dry-run` lists WARC files without downloading. The HTTP client (`archiveit.rs`) is transport-abstracted for testing (like `browsertrix.rs`). `import browsertrix` authenticates to a [Browsertrix](https://browsertrix.com/) instance (credentials from `BROWSERTRIX_USER`/`BROWSERTRIX_PASSWORD` or `BROWSERTRIX_TOKEN` in the environment, never argv), resolves the org, and for each selected archived item **downloads** the WACZ into `<home>/archive/<item-id>/` (a per-item subfolder, so two items can't clash on a shared resource filename) via its presigned `replay.json` URL and indexes it as a durable local (File) source. Downloading (rather than streaming in place) is the default because Browsertrix presigned URLs expire in ~48h, so a naive streamed source would break replay. **`--stream`** opts into an index-only footprint instead: the manifest stores a `Source::Browsertrix { host, org, item, resource }` (stable identity, encoded `browsertrix|…`), not a URL, and a fresh presigned URL is re-resolved on demand — at index/reindex time, and at replay time by the server (`serve_file` 302-redirects to a freshly-resolved URL, cached under its expiry). Resolution is done by a bin-provided `index::SourceResolver` (the library never touches credentials); `serve` builds one from the same `BROWSERTRIX_*` env vars, so streamed crawls replay only when the server has credentials (503 otherwise). Selection: `--collection <ID|SLUG|NAME>` (resolved to the collection UUID the API requires) or `--crawl <ID>`; default is the whole org. **QA filter:** by default only crawls a reviewer has QA'd in Browsertrix (`reviewStatus` set) are imported; `--include-unreviewed` / `--min-review <1-5>` adjust this, and a single named `--crawl` is always included. **Incremental:** provenance recorded on each crawl (`browsertrix` field in the manifest: host, item id, resource hash) lets a re-run skip already-imported items unless `--force`. Importing a `--collection` groups its crawls into a indice collection of the same name (`--into <NAME>` overrides, and groups org-wide/single-crawl imports); `--dry-run` lists without downloading. **`--public`** imports a collection an org has published openly, with **no credentials at all**: it uses Browsertrix's unauthenticated public API (`/api/public/orgs/{slug}/collections`, then each collection's `/api/orgs/{oid}/collections/{id}/public/replay.json`) rather than login → org → items. It needs `--org <slug>` (the public org slug); `--collection` picks one, else all the org's public collections are imported. Public streaming stores a `Source::BrowsertrixPublic { host, org, collection, resource }` (encoded `browsertrix-public|…`), re-resolved via the *collection*-scoped public replay.json — so replay needs no server credentials either. (`--crawl` isn't supported in public mode; the public API is collection-scoped.) The HTTP client (`browsertrix.rs`) is transport-abstracted for testing (mirrors `http_range::RangeFetch`). See *Indexing Pipeline*.
- `wacz build <WARC>...`: the "I have WARCs, not WACZs" on-ramp — packages one or more `.warc`/`.warc.gz` files into a WACZ under `<home>/archive/` and indexes it (**`--collection` required**). It is a **native Rust** builder (no py-wacz/js-wacz/Node runtime — the single-binary goal holds) that deliberately **mirrors Webrecorder's own tools** so the output is exactly what ReplayWeb.page/wabac.js expect: the CDX matches `warcio.js`'s `CDXIndexer` and the packaging matches `browsertrix-crawler`'s `WACZ` class. Design points (see `wacz_build.rs`): (1) **WARC bytes are packaged verbatim** — each input is stored *uncompressed* (`CompressionMethod::Stored`) as `archive/<basename>`, byte-for-byte identical to the source (indice packages the crawl data, it never re-serializes it; digests are preserved), and the CDX offsets are *read* from the originals via `warc.rs` — exactly the read model indice already uses. (2) The **CDX** (`indexes/index.cdxj`, sorted) is generated to match `warcio.js` line-for-line: `<surt> <14-digit-ts> {url,mime,status,digest,length,offset,filename}`, where `surt` is a verbatim port of warcio's `getSurt` (so replay-time URL canonicalization matches), `digest` is the WARC payload digest with its `algo:` prefix stripped, and `request`/`warcinfo` records are excluded. (3) **`datapackage.json`** takes the browsertrix shape (`resources[]` with `sha256:<hex>` hashes, `wacz_version`, `software`, `created`, …) plus the descriptive fields indice reads for finding-aid seeding, with a `datapackage-digest.json`; a minimal `pages/pages.jsonl` carries seed pages. (4) Each input is **sniff-tested** first (must parse as a WARC with ≥1 indexable record) so a bad file fails fast rather than yielding a broken WACZ. Metadata comes from flags, or is prompted for on an interactive terminal (`--yes` skips prompting). The headless library entry point (`wacz_build::build_wacz`) is the reusable building block for the Archive-It importer (which downloads WARCs via WASAPI, not WACZs). **Fidelity is pinned by a dev/CI reference-oracle test** that diffs our CDXJ against `warcio.js`'s own `cdx-index` on the fixture WARCs (gated on a warcio CLI; skipped otherwise so plain `cargo test` needs no Node). Byte-identical-to-Browsertrix output is a non-goal (nondeterministic zip mtimes / `created` / gzip); line-for-line CDX conformance is the guarantee that matters, since replay is driven by the CDX. POST fuzzy-match keying, ZipNum-clustered CDX for large indexes, HTML-`<title>` page titles, and WACZ signing are tracked follow-ups.

---

## Web Server Routes

| Route | Handler |
|---|---|
| `GET /` | Homepage: search box, browse-by-facet entry points, and the collection overview |
| `GET /search?q=...&page=N` | Server-rendered results with a facet sidebar, month timeline, snippets, and pagination |
| `GET /collection/{id}` | Collection detail: metadata, a scoped facet overview, and member crawls |
| `GET /crawl/{id}` | Crawl detail: provenance, file metadata, a scoped facet overview, and seed pages (a crawl is one WACZ) |
| `GET /api/search?q=...` | Full-text search → JSON (results, `total`, `capped`, `facets`) |
| `GET /thumb/{id}` | A crawl's cached representative-image thumbnail (small JPEG); 404 when it has none |
| `GET /files/{id}` | Stream a registered WACZ file with byte-range support |
| `GET /collection/{id}/replay.json` | wabac multi-WACZ manifest for replaying a whole collection |
| `GET /collection/{id}/pages` | Collection page list + exact-URL→WACZ resolution (wabac `pagesQueryUrl`), from the index |
| `GET /assets/*` | Embedded site assets (the shared `app.css` stylesheet) |
| `GET /replay/viewer` | Viewer shell (reads `?source=&url=&ts=&name=&collection=` params) |
| `GET /replay/*` | Embedded ReplayWebPage static assets (JS, CSS, WASM, sw.js) |

---

## Tantivy Schema (Full-text Index)

Two document types share the same index, distinguished by `doc_type`.

| Field | Type | Stored | Indexed | Fast | Notes |
|---|---|---|---|---|---|
| `doc_type` | STRING | ✓ | exact | - | `"page"` or `"collection"` |
| `crawl_id` | STRING | ✓ | exact | - | Per-crawl (per-WACZ) hash, e.g. `e02536ec`; scoped by the `crawl:` filter |
| `crawl_name` | STRING | ✓ | - | - | Human-readable crawl (WACZ) name |
| `collection` | STRING | ✓ | exact | ✓ | Curated collection slug the crawl belongs to, for `collection:` filtering + faceting |
| `url` | STRING | ✓ | exact | - | Page URL (empty for collection docs) |
| `timestamp` | STRING | ✓ | - | - | 14-digit crawl timestamp |
| `title` | TEXT | ✓ | BM25 | - | Page title or collection name |
| `body` | TEXT | - | BM25 | - | Page body text (or collection description + seed URLs); indexed for search but **not stored** — the snippet copy is `body_snip` |
| `body_snip` | text | ✓ | - | - | Capped (~16 KiB) prefix of `body`, stored (not indexed) for snippet highlighting; the cap is the `stored_body_cap_kb` frugality knob |
| `description` | TEXT | ✓ | BM25 | - | Page `<meta description>` / og:description; shown as a snippet fallback |
| `headings` | TEXT | - | BM25 | - | Page `<h1>`/`<h2>` text; boosted at query time |
| `keywords` | TEXT | - | BM25 | - | `<meta name=keywords>`; searchable via default fields |
| `author` | TEXT | - | BM25 | - | `<meta name=author>` / `article:author`; also `author:name` |
| `domain` | STRING | ✓ | exact | ✓ | Exact host of the page URL, for `domain:` filtering + results display |
| `site` | STRING | ✓ | exact | ✓ | Registrable domain (eTLD+1, via the PSL), for the cross-subdomain `site:` filter + the Site facet |
| `url_tokens` | TEXT | - | BM25 | - | URL host + path split into words, so URL words are searchable as ordinary terms |
| `year` | u64 | ✓ | numeric | ✓ | Four-digit crawl year, for `year:2021` / `year:[2020 TO 2023]` + the Year facet |
| `month` | u64 | ✓ | numeric | ✓ | Six-digit crawl month `YYYYMM`, for `month:202103` / ranges + the results timeline |
| `type` | STRING | ✓ | exact | ✓ | Coarse media type (`html` or `pdf`), for `type:pdf` filtering + facet |
| `lang` | STRING | ✓ | exact | ✓ | Primary language subtag (`en`); from `<html lang>`, else detected from body text; `lang:en` filtering + facet |
| `status` | u64 | ✓ | numeric | - | HTTP status code, for `status:200` / `status:[200 TO 299]` |
| `modified` | u64 | ✓ | numeric | - | Year from the HTTP `Last-Modified` header, for `modified:2015` |

The capped body prefix (`body_snip`) and `description` are stored so Tantivy's `SnippetGenerator` can produce hit-highlighted excerpts without re-reading the source files, and so a result can show the description when the query didn't match the body. The full `body` is indexed for search (recall is unaffected by the cap) but **not stored** — only its capped prefix is, which bounds the doc store (see the size/scale model). `headings`, `keywords`, `author`, and `url_tokens` are indexed but not stored. Positions (for phrase queries) are kept on every phrase-useful field — `title`, `body`, `description`, `keywords`, `author` — and dropped only on `headings` (whose text is duplicated into `body`) and `url_tokens` (URL words are never phrase-searched).

The facet dimensions (`collection`, `site`, `type`, `lang`) and the numeric `year`/`month` are **fast** (columnar) fields. Fast storage is what lets them back Tantivy *terms aggregations*, which compute the per-value counts for the facet sidebar and the timeline. The string facet fields use the `raw` tokenizer so each field value is a single term (one bucket), rather than being split into words. Note the **Site facet is the registrable domain (`site`)**, not the raw host — so a whole site groups across subdomains; `domain:` remains available for exact-host filtering. `lang` is taken from the declared `<html lang>` when present, else detected from the body text with `whatlang` (single dominant language, only when confident); the code is normalized to a 639-1 subtag so declared and detected values unify.

### Query behavior

`SearchIndex::search_faceted(query, limit, offset)` runs one query and returns a page of
results, the total, facet counts, and the month timeline together (see *Faceted, temporal
discovery* below). `SearchIndex::search` is a thin wrapper returning just the hits.
`SearchIndex::facet_overview()` runs only the aggregation over a match-all query (the
homepage browse entry points); `facet_overview_scoped(FacetScope)` does the same restricted
to one collection (`collection`) or crawl (`crawl_id`) — this backs the **scoped facet
overview on the collection and crawl detail pages**, where each value (top sites, years,
types, languages) links into a search already scoped to that collection or crawl. The
crawl-scoped links use a `crawl:<id>` filter - a short alias rewritten to the `crawl_id`
field before parsing. Since the id is opaque, the search page resolves it to the crawl's
name for the active-filter chip.

Queries go through Tantivy's `QueryParser`, configured in `search_faceted`:

- **Default fields** are `title`, `headings`, `body`, `description`, and `url_tokens`, so a bare word matches any of them. Other fields are reachable with explicit `field:` syntax (`title:climate`, `domain:example.com`).
- **AND by default** (`set_conjunction_by_default`): `climate policy` requires both terms. Users can still write `OR`, `-`, `+`, `"phrases"`, `(groups)`, and `^boost`.
- **Field boosts** (`set_field_boost`): title matches rank highest, headings next, then body/description/URL.
- **Lenient parsing** (`parse_query_lenient`): a malformed query (stray quote, empty `field:`) yields a best-effort query rather than an error, so the search box never returns a 500 while a user experiments with syntax.

The `<details>` "Search tips" panel on the homepage and results page documents this syntax for end users; its examples must stay in sync with this configuration.

### Schema changes and migration

Tantivy persists the schema inside the index directory (`index/full_text/meta.json`) and reuses it when the index is opened. Changing the schema — adding a searchable field (as `domain`, `year`, `type`, `month` did) or making fields *fast* for faceting — therefore makes an index built by an older binary *stale*. To avoid writing/querying against a mismatched schema, `SearchIndex::open` compares the stored schema to the current one and, if they differ, returns an error telling the user to run `indice reindex` rather than proceeding (which would otherwise panic on a missing field).

`indice reindex` performs the migration: it reads `collections.json`, builds a fresh index (with the current schema) from every registered source (files and remote URLs), and swaps it in for the old one. The manifest and collection names are preserved. The rebuild is **atomic**: the new index is built into a sibling `index/full_text.new` and promoted with rename swaps only once it completes, so a crash/kill/disk-full mid-rebuild leaves the existing `index/full_text` untouched (a running `serve` keeps reading it until the swap), and a crash *between* the two swap renames is recovered on the next `reindex` from the parked `full_text.old`. The cost is transient: old + new coexist on disk (~2×) until the swap. A partial rebuild (some sources skipped, above) is still swapped in — usable and no worse than the old index — while the command exits non-zero.

### Collection documents

One document per WACZ is indexed at `indice index` time. Its `body` concatenates the collection description and seed page titles and URLs from `datapackage.json`. This makes the collection itself searchable: a query for "attar" returns both individual pages from that site and the collection whose metadata mentions it.

### Page documents

One document per HTML response in the WACZ. `body` is extracted from the `<body>` element with `<script>`, `<style>`, and `<noscript>` removed.

### Index footprint & the size/scale model

indice's identity is small→institutional (a laptop's handful of WACZs up to an institution's TBs), so index footprint is a first-class concern. `indice stats` reports the on-disk size broken down by Tantivy segment-file type, the live doc count, bytes-per-doc, and projections to 1M / 100M docs — re-run it after any change to measure the effect (this is the "measure-first" model; deeper tuning is tracked under the *Scale* epic).

The file types and what drives them:

- **`store`** — the stored fields returned for results, dominated by the page **`body`** text kept for snippets. This is the largest, **corpus-linear** cost (≈64% on a measured 21.7k-doc / 118 MB corpus). It is compressed with **zstd** (set via `IndexSettings.docstore_compression` at index-create time; tantivy's default is lz4, which compresses text markedly worse). A `reindex` migrates an older lz4 index to zstd.
- **`pos`** — term positions, needed for phrase queries; dominated by `body` (~20%). Positions are kept on the phrase-useful fields (`title`/`body`/`description`/`keywords`/`author`) and dropped only on `headings` (its text is in `body`) and `url_tokens` (never phrase-searched) — the `.pos` win is small because `body` dominates it, so the short metadata fields keep positions rather than silently breaking phrase recall.
- **`term`/`idx`** — the inverted index; intrinsic to search (~16%).
- **`fast`** — columnar values backing facets/sorting (`year`/`month`, `site`/`domain`/`media_type`/`lang`); small.
- **`fieldnorm`**, **`del`** (deletes), **`meta`** (JSON bookkeeping) — minor.

The dominant costs are attacked directly: zstd doc-store compression (`qw5.2`), capping stored `body` text with a snippet fallback (`qw5.3`), and dropping term positions on `headings`/`url_tokens` (`qw5.4`) — all applied on a fresh index or `reindex`. The stored-body cap is operator-configurable (`qw5.8`, see below), so a laptop can keep generous snippets while an institution dials it down. Remaining, data-driven follow-ups under the epic: query-time faceting/grouping cost at scale (`qw5.5`), index-build RAM/merges (`qw5.6`), and the one-index-vs-sharding question (`qw5.7`). Every such change should be chosen from an `indice stats` before/after, not intuition.

### Operator config (`<home>/config.yaml`)

indice's first optional, hand-editable home-level config — committable alongside the archive, in the same YAML as the finding aids. It is deliberately forgiving: the file is optional, every field defaults, and unknown/missing keys are ignored, so it can grow (site name, branding, CSS override, theming, …) without breaking older or newer homes. `indice config` shows the resolved settings and the file path. Today it carries index tuning:

```yaml
index:
  # Bytes of page body text STORED per document for snippets, in KiB. The full
  # body is always indexed (search recall is unaffected); this only bounds the
  # stored copy used to render snippets. 0 = store the full body; omit for the
  # 16 KiB default. Applied on index/reindex — measure with `indice stats`.
  stored_body_cap_kb: 8
  # Tantivy indexing-buffer budget in MiB — the RAM ceiling / throughput knob for
  # building the index. Higher = faster bulk ingest, more RAM; omit for 50 MiB.
  writer_heap_mb: 256
```

### Ingesting large corpora

`index` accepts many WACZs at once (globs, or `--from-file`), and builds the
index to be practical at TB scale:

- **Incremental & resumable.** Each WACZ is committed and the manifest saved as
  it finishes, so an interrupted run (OOM, crash, Ctrl-C, power) keeps every
  completed crawl. Re-running the same `index` command **skips sources already
  indexed** into the collection and continues from where it stopped; `--force`
  re-indexes them. (Streaming, local-file, and Browsertrix sources resume this
  way. A `--download` remote URL is stored under its local archive path, whose id
  differs from the URL's, so a re-run re-fetches it rather than skipping —
  idempotent, but not skipped.)
- **RAM ceiling.** `index.writer_heap_mb` sets the Tantivy indexing buffer
  (default 50 MiB), the main lever on build-time memory vs. throughput; Tantivy
  splits it across indexing threads and merges segments in the background.
- **Measuring a run.** Time and peak RSS aren't instrumented in-process; measure
  a build with the OS, e.g. `/usr/bin/time -l indice index …` (macOS) or
  `/usr/bin/time -v` (GNU), and track the resulting footprint with `indice stats`.

---

## Snippets and Hit Highlighting

Search results include a `snippet` field generated by Tantivy's `SnippetGenerator`. The generator:

1. Re-tokenizes the stored `body` text
2. Locates the window with the highest density of matched query terms
3. Returns the window as a string with matched terms wrapped in `<b>` tags

The server renders these `<b>` tags in the search results HTML; CSS applies a highlight background color.

---

## Collection Metadata

`datapackage.json` inside each WACZ (WACZ spec §4) is read at index time and stored on the
WACZ's manifest entry.

Fields extracted:
- `title` - WACZ display name (falls back to filename stem)
- `description` - free-text description
- `created` / `modified` - ISO 8601 crawl / packaging dates
- `software` - crawler/packager software (also enriched from the WARC `warcinfo`)
- `keywords` / `licenses` - Frictionless Data Package extension fields (WACZ spec-blessed),
  surfaced as crawl keywords and a license/rights signal
- `warcinfo` `isPartOf` / `hostname` / `conformsTo` - previously parsed but dropped; now stored
- Seed pages - first entries from the `pages` array (url, title, timestamp)
- **Capture-quality histogram** - HTTP status codes tallied from the CDX at index time (every
  capture, including the bodyless 4xx/5xx that never become search documents), stored compactly
  as `status_counts` on the crawl. Surfaces the 404/403/504 "absences" a clean-looking crawl can
  hide (a derived DACS *Appraisal* signal); no per-capture index bloat.

The crawl detail page shows this per crawl; the collection page aggregates it across members.
All fields are conditional, and the crawl-level ones populate on **reindex**.

---

## Collection Management

The manifest splits into a **git-committable curatorial layer** (the finding aids) and a
**derived index** (rebuildable from the WACZs). The line is: *curatorial = committed prose;
derived = rebuildable index.*

```
<home>/
  collections/<slug>/       # curator source of truth — commit this
    README.md               #   the finding aid (dir name identifies the collection)
    thumbnail.jpg           #   optional collection-level representative image
    crawls/<id>.md          #   optional per-crawl curator note
    crawls/<id>.jpg         #   optional curator-pinned crawl thumbnail
  archive/<slug>/…          # local WACZ files, organized by collection (browsable)
  index/                    # derived — add to .gitignore
    waczs.json              #   registration ledger (source + membership) + derived provenance
    full_text/              #   the Tantivy index
    thumbs/                 #   auto-selected representative-image cache
```

Recommended for a curator keeping their home in git: `echo '/index' >> .gitignore` and
`git add collections/`. Everything a curator authors — prose, pinned images — lives under
`collections/<slug>/`; everything the tool derives is rebuildable under `index/`.

**Every crawl belongs to a collection** (there are no auto "singleton" collections): `import`
supplies it; hand-`index` requires `--collection` (see *Two-level collection model*).

### Finding aids (`collections/<slug>/README.md`)

Each collection's descriptive metadata is a Markdown file with YAML **front-matter** (the short
structured fields) and a Markdown **body** (the narrative). The file is the source of truth:
`indice collection set …` writes it, and a curator can equally hand-edit and commit it; the id
is the collection **directory** name.

```markdown
---
name: SUCHO Ukraine
created: 2026-07-01T00:00:00Z
creator: Saving Ukrainian Cultural Heritage Online   # DACS Name of Creator / EAD <origination>
dates: 2022–2023                                      # coverage statement / EAD <unitdate>
rights: See individual sites; archived for research  # access & use / EAD <userestrict>
subjects: [ukraine, cultural heritage]               # access points / EAD <controlaccess>
---
## Scope and Content
Why this was archived… (EAD <scopecontent>)

## Custodial History and Appraisal
How it was acquired, why these seeds, and what is *absent* (EAD <custodhist>/<appraisal>).
```

The fields are framed against **DACS** / **EAD** (see *Discovery, Provenance & Collections*
below) rather than flat Dublin Core: the collection is described richly once (the `archdesc`),
and Scope & Content / Custodial history / Appraisal collapse into the one narrative body (a
finding aid is a prose document with sections, not a set of columns). The body is rendered to a
**safe HTML subset** (`markdown.rs`: raw HTML escaped, link/image schemes restricted to
http/https/mailto, images neutralized to alt text) since curator- and importer-supplied content
is untrusted. Curator/importer edits merge with a *fill gaps, curator wins* policy
(`CollectionFields`): an importer sets a field only when it is still empty, so hand edits survive
re-sync. A legacy `index/collections.json`, or an earlier flat `collections/<slug>.md`, is
migrated to the `collections/<slug>/README.md` form on open/save.

The DACS front-matter fields are **scaffolded as empty blanks** rather than omitted when unset —
a freshly-created finding aid writes `creator: ''`, `dates: ''`, `rights: ''`, `description: ''`,
`subjects: []` — so a curator opening the file sees exactly what to fill in (fill-in-the-blank,
not a guess-the-schema). The blanks are a *display scaffold only*: `parse_finding_aid` maps a
blank back to unset, so ingest still seeds it (fill-gaps) and the collection page's "Still needed"
prompt still fires. `curator` (the instance/repository operator, not a per-collection gap) stays
omitted when unset, and the narrative body is left un-templated so the Scope & Content nudge isn't
fooled by placeholder prose.

Per-crawl notes live in `collections/<slug>/crawls/<id>.md` (plain Markdown), for documenting a
single crawl's context or absences without repeating the collection-level description (DACS
multilevel inheritance). Curator-pinned crawl thumbnails sit alongside as
`collections/<slug>/crawls/<id>.jpg`, and a collection-wide image as
`collections/<slug>/thumbnail.jpg` — all committable; auto-selected thumbnails stay in the
derived `index/thumbs/` cache.

### Derived index (`index/waczs.json`)

`waczs.json` is the authoritative **registration ledger** — the list of sources and each one's
collection membership (recoverable only here, since `Url`/`Browsertrix` crawls have no local
file to scan) — plus the extracted/derived provenance cache. It's machine-owned and never
hand-edited; `reindex` rebuilds the extracted fields from the registered sources.

```jsonc
// waczs.json - one entry per WACZ member
[
  {
    "id": "e02536ec",
    "collection": "demo",                       // -> collections/demo/README.md; authoritative membership
    "source": "archive/attar.wacz",
    "name": "Attar Silas",
    "date_indexed": "2026-07-01T00:00:00Z",
    "file_size": 104857600,
    "sha256": "e3b0c44298fc1c149afbf4c8996fb924...",
    "crawl_date": "2026-02-24T00:00:00Z",
    "software": ["browsertrix-crawler 1.0.0"],
    "seed_pages": [ { "url": "https://www.attarsilas.fr/", "title": "Attar Silas", "ts": "20260224005439" } ]
  }
]
```

- `source`: a local file path (stored relative to `<home>` when under it, e.g. `archive/attar.wacz`; absolute otherwise) or an `http(s)://` URL. Relative paths resolve against `<home>` at serve time, so the whole home folder is portable.
- `id`: first 8 hex chars of SHA-256 of the source string - relative sources give IDs that are stable across moves. Collection ids are slugs of the collection name.
- Re-indexing the same source upserts its WACZ entry. Every crawl belongs to an explicitly named collection (no singletons).
- Collection descriptive metadata lives in `collections/<slug>/README.md` (above), not in the index. An older single-file `collections.json` (flat, per-WACZ with a `source` key), a `collections.json` groups file, or a flat `collections/<slug>.md` is detected and **migrated** on open/save.
- For a **file** source, `GET /files/{id}` streams the registered file with byte-range support; only registered files are served, so arbitrary filesystem access is not possible.
- For a **URL** source, replay points wabac.js directly at the remote URL (the host must provide range + CORS); `GET /files/{id}` just redirects there. indice never proxies remote bytes.

---

## Discovery, Provenance & Collections

Discovery in indice is search-first and faceted, over a two-level collection model, with
provenance surfaced rather than buried. This section explains that design and the reasoning
behind it; the *Planned* subsection at the end lists what is deliberately not built yet.

### Why (grounded in the literature)

- **Needs are mostly navigational and temporal.** Costa & Silva's query-log study of the
  Portuguese Web Archive found web-archive needs are ~53-81% *navigational* (see a page/site
  as it was, or how it changed over time), 14-38% *informational* (find information on a
  topic from the past), 5-16% *transactional*. So **time is a first-class axis**, and both
  known-item lookup (URL + date + versions) and topical full-text search matter.
- **Faceted "slice and dice" scales navigation better than clever ranking.** SHINE (UK Web
  Archive) and SolrWayback (Royal Danish Library), both built on the UK Web Archive's
  warc-indexer, offer facets for content-type, domain, crawl year, links, and public suffix.
  Facets are the established answer to a growing, unwieldy list. indice follows this
  lineage directly — SolrWayback pairs the same faceted full-text search with in-browser
  replay — but swaps their Solr backend for a single embedded Tantivy index, so the same
  faceted search fits a private laptop archive as readily as an institutional one.
- **Provenance is essential and usually buried.** Maemura, Worby, Milligan & Becker, *If
  These Crawls Could Talk* (JASIST 2018): to trust and interpret an archive you must be able
  to evaluate its provenance, scope, and absences (curatorial intent, seeds/scope, crawler
  software/parameters, operator, dates).

### Two-level collection model

indice uses a **two-level model** (see *Collection Management* above for the on-disk form):

- **Collection** - a curated grouping with *curatorial* provenance (the finding aid: scope,
  creator, dates, rights, subjects). The primary unit users browse and facet by; its
  descriptive metadata is the git-committable `collections/<slug>/README.md`.
- **WACZ members** - each carries *technical* provenance (crawler software, operator,
  user-agent, crawl date range, seeds, page counts, fixity). Stored in `waczs.json`, each
  pointing at its collection.

**Every crawl belongs to a collection — no singletons.** Import supplies the collection
automatically (the Browsertrix collection name, else the org name); hand-`index` **requires
`--collection <NAME>`**. That requirement is deliberate friction, not an oversight: indice's
Maemura-grounded thesis (below) is that curatorial context is worth the small cost of pausing to
ask "what is this a part of, and why keep it?" — so the tool asks at ingest, the same way the
empty-state nudge asks on the collection page (index a glob into one collection to decide once).
Membership lives in the manifest (`waczs.json`), not the filesystem, because remote/streamed
crawls have no local file; `archive/<slug>/` is a browsable *placement* convention for the WACZs
indice downloads, not the source of truth. **Nesting** (collections of collections, à la
EAD/DACS fonds→series) is intentionally deferred: indice ships flat, single-level collections,
with the slug/on-disk form kept so an optional `parent` can be added later without a migration.

This model serves both audiences: an **individual** self-hosting WACZs made with wget
or browsertrix-crawler gets context with no hosted-service dependency; an **institution**
(e.g. TBs of WARC behind pywb) can reorganize crawls into navigable, provenance-bearing
collections. It is also the structural fix for the "long list" problem.

**Vocabulary (UI vs. data model).** WACZ is a *packaging format* - a technical container -
which most users don't think in terms of; they think in **crawls**. So the web UI presents
each WACZ member as a "crawl" (the `/crawl/{id}` detail page, the "Crawls" count on
collections). "WACZ" is kept only where the file/format is genuinely what's meant - the
`index`/`reindex` CLI, `/files/{id}` byte-range serving, replay source, and fixity. The
data model (`Wacz`, `waczs.json`, `wacz_by_id`) stays WACZ-named, since there it *is* the
file; the rename is a presentation-layer relabel. (A WACZ is 1:1 with a crawl today; if the
Browsertrix importer later distinguishes crawls from uploads, the label can follow the
item's actual type.)

### Provenance

indice distinguishes **curatorial** provenance (who/why/scope, at the collection level) from
**technical/derived** provenance (how it was captured, at the crawl level), and presents both
**prominently** rather than tucking them away.

**Curatorial** provenance is the finding aid (*Collection Management* above), framed against
**DACS** and its EAD encoding rather than flat Dublin Core — because DACS supplies the *shape*
archivists expect: multilevel description with inheritance (describe the collection richly once;
a crawl adds only a differentiating note), and an emphasis on Creator, Scope & Content, and
access conditions. The collection page reads like a finding aid: an "About this collection"
front-matter (rendered narrative + a curatorial `Creator`/`Dates`/`Rights`/`Subjects` table)
above the facets, set apart from the derived aggregates. When those fields are empty the page
shows a muted nudge naming the DACS single-level *minimum* curatorial elements — Creator, Scope
& Content, Access — so the prompt carries archival authority.

**Ingest pre-seeds the finding aid.** So a collection isn't blank on arrival, both ingest paths
populate `CollectionFields` and apply them *fill-gaps* (`Manifest::seed_fields` — set only
still-empty fields, so a curator's edit or an earlier seed is never overwritten; distinct from
the CLI's authoritative `apply_fields`). Sources: the **WACZ `datapackage.json`** on `index`
(`description`→narrative, `keywords`→subjects, `created`→dates, Frictionless
`contributors`/`organization`→creator, `licenses`→rights) and the **Browsertrix API** on `import`
(collection `description`→narrative, `caption`→abstract, `tags`→subjects, `dateEarliest`/
`dateLatest`→dates, org name+site→creator; `access` is deliberately *not* mapped to rights, since
replay visibility ≠ a reuse license). The model is the union target for Browsertrix and Archive-It
(deferred) so both map cleanly.

**Technical/derived** provenance is extracted from the WACZ/WARC and shown in the crawl page's
provenance panel plus a compact line on collection member listings. Sources used:

- **`datapackage.json`** (WACZ 1.1.1): `title`, `description`, `created`, `modified`, `software`,
  and the Frictionless extension fields `keywords` / `licenses`.
- **WARC `warcinfo` record** (`application/warc-fields`, one per WARC, read by `warc.rs`):
  `software`, `operator`, `http-header-user-agent`, `robots`, `isPartOf`, `hostname`,
  `conformsTo`.
- **Timestamps**: capture date range (earliest/latest) and page counts.
- **Capture quality** (a derived DACS *Appraisal* signal): the HTTP status histogram tallied
  from the CDX (see *Collection Metadata*), surfacing failed/blocked captures. Plus the
  Browsertrix QA `reviewStatus` (1–5, Excellent→Bad) carried onto imported crawls — the machine
  and human sides of "documenting absences".

Fixity is verifiable with `indice verify` (re-hashing each WACZ against the stored
SHA-256). Signature-based authenticity (`datapackage-digest.json`, the WACZ auth spec) is
*Planned* (below).

### Faceted, temporal discovery

The results page is search-first and faceted, implemented in `search_faceted` +
`views.rs`:

- **Facet sidebar** with live counts for collection, year, site (registrable domain),
  content type, and language. Each value is a link that toggles a `field:value` filter on
  the query; applied filters show as removable chips. Counts come from Tantivy *terms
  aggregations* over the fast fields, computed in the **same query pass** as the results, so
  they always reflect the current query. Beyond the faceted fields, results can also be
  filtered by `author:`, `domain:` (exact host), `status:` (HTTP status), and `modified:`
  (Last-Modified year).
- **Month timeline** - a chronological histogram (a terms aggregation on `month`) above the
  results; each bar toggles a `month:` filter.
- **Repeat-capture grouping** - multiple captures of the same URL collapse into one result
  showing "captured N times". Tantivy has no native field collapsing, so grouping is done
  over the top `CANDIDATE_CAP` scored captures per query (`SearchResponse.capped` flags when
  more matched).
- **Pagination** over the grouped results (`?page=N`).
- **Search-first homepage** - a prominent search box, "browse by year"/"top sites" entry
  points (from an archive-wide facet overview), then the collection cards.

A key distinction: **facet and timeline counts count captures and are exact over the whole
match set; the result total counts distinct URLs and is bounded by `CANDIDATE_CAP`.** They
measure different things, so a facet count is generally larger than the number of grouped
results it yields.

### Planned / not yet built

- **Authenticity**: verify `datapackage-digest.json` signatures (WACZ auth spec), surfaced
  alongside fixity. Tracked by `rustyweb-authenticity-671`.
- **Search enrichment**: keywords/author, language-detection fallback, the `site:`
  registrable-domain facet, HTTP `status:`, and `modified:` (Last-Modified year) have
  shipped. Still open under `rustyweb-search-enrichment-6by`: outbound-link fields (deferred
  over index size), plus a `crawler` facet.
- **Browsertrix import**: pull WACZs from a Browsertrix org's public API into `<home>/archive`.
  Tracked by `rustyweb-15z` (includes nested/multi-WACZ indexing).
- **Archive-It import**: pull crawls from an Archive-It account (WARCs via WASAPI, built into
  one WACZ per crawl) — CLI (`import archive-it`) and an accession-desk wizard. Tracked by
  `rustyweb-kx53`.

### References

- Costa & Silva, *Understanding the Information Needs of Web Archive Users*, IWAW 2010.
- Maemura, Worby, Milligan & Becker, *If These Crawls Could Talk: Studying and Documenting
  Web Archives Provenance*, JASIST 2018.
- SHINE (`github.com/ukwa/shine`) and SolrWayback (`github.com/netarchivesuite/solrwayback`),
  both on the UK Web Archive's warc-indexer / webarchive-discovery
  (`github.com/ukwa/webarchive-discovery`).
- WACZ 1.1.1 and the WACZ auth spec; the WARC 1.0 format specification.

---

## Replay Viewer

`GET /replay/viewer` serves a thin HTML shell (`static/replay/viewer.html`) that:

1. Reads `source`, `url`, `ts`, and `name` from the URL query string
2. Renders a banner bar showing the collection name and current page URL
3. Mounts a `<replay-web-page>` component with the given `source` and `url`

```html
<div id="banner">
  <a href="/">indice</a>
  <span id="collection-name"></span>
  <span id="current-url"></span>
</div>
<replay-web-page id="rp"></replay-web-page>
```

The `<replay-web-page>` component fires a `rwp-url-change` event as the user navigates within the archive; the banner listens for this event and updates the displayed URL in real time.

In WACZ-direct mode the component reads the WACZ from `/files/{id}` via byte-range requests, loads the internal CDX into browser IndexedDB, and serves all resources from WARC bytes without making per-resource calls to indice. All URL rewriting, wombat.js injection, fuzzy matching, and redirect handling are performed client-side by wabac.js.

### Collection (multi-WACZ) replay

A whole collection can be replayed at once (the "Replay collection" affordance on the homepage card and the collection page), not just a single crawl. The viewer points wabac.js at `GET /collection/{id}/replay.json` — a wabac multi-WACZ manifest (`{ resources: [{ name, path, hash, crawlId }], metadata }`) listing every member crawl, each `path` reusing the same `/files/{id}` byte-serving as single-WACZ replay (so File/Browsertrix members work identically, remote-URL members point at their URL). A generic whole-collection entry opens on a default landing page (the first member's first seed page); a specific-context entry (a crawl's Replay button, a search result) carries its own `url`/`ts` instead.

To keep the **replay-client footprint flat** as a collection grows (a laptop→institutional concern; see *Scale*), the manifest sets `metadata.pagesQueryUrl` to `GET /collection/{id}/pages`, answered from the Tantivy index (`SearchIndex::collection_pages`). wabac then queries that endpoint for the page-list sidebar and for on-demand URL→WACZ resolution, instead of loading every member's page index into the browser. The response is wabac's shape (`{ total, items: [{ url, ts, title, filename }] }`), where `filename` is the member WACZ id (`== resources[].name`) and `ts` is ISO 8601 (the index stores a 14-digit timestamp). The exact-URL mode is a direct term query on the raw `url` field — no URL-collapsing or candidate cap, so totals are exact and pagination is complete (unlike the ranked, grouped main search).

**Resolution is scoped to the collection** — a deliberate decision (`rustyweb-cross-wacz-replay-dk4`, closed won't-fix). A link from one member crawl to a page archived in *another member of the same collection* resolves and lands on that crawl's capture. This is *within-collection* only: there is no archive-wide or cross-collection resolution, and no explicit "open in another crawl" prompt — within-collection resolution is accepted as-is. Page **sub-resources** (images/CSS/JS) always come from the page's own crawl: wabac's per-URL `pagesQueryUrl` lookup is gated to top-level page navigations (`document`/`iframe` destinations), so a page is never stitched together from resources captured at different times — preserving per-page temporal coherence (the ODU/WS-DL temporal-violation concern).

---

## WACZ CDX Reader (`wacz.rs`)

`indice search-url` reads WACZ CDX files on-the-fly without a separate CDX store. The implementation:

1. Opens the WACZ as a ZIP
2. Reads and decompresses `indexes/index.cdx.gz`
3. Parses each CDXJ line (space-separated SURT + timestamp + JSON fields)
4. Matches lines by URL equality or SURT prefix

This is intentionally lazy - no persistent CDX index is maintained by indice. The WACZ's built-in CDX is authoritative; indice simply reads it when asked.

---

## Indexing Pipeline

```
Input WACZ
  └── Open as ZIP
       ├── Read datapackage.json ──► WaczMetadata (title, description, crawl date, seed pages)
       │    └── Index as collection document in Tantivy
       │    └── Write to collections.json
       ├── Iterate archive/*.warc(.gz) members, collecting per record:
       │    ├── HTML response        ──► title (<title>) + scraped body text
       │    ├── urn:text: resource   ──► fully rendered (post-JS) page text
       │    └── application/pdf resp ──► extracted PDF text (title from filename)
       └── Read pages/pages.jsonl + pages/extraPages.jsonl `text` field
            └── fully rendered page text (where a crawl stores it here
                instead of as urn:text: records) + a fallback title
                 │
                 └── Merge into one document per URL (body prefers rendered
                     text, then PDF text, then scraped HTML; title from HTML)
                     └── Derive domain/year/month/type/lang, then write the page
                         document to Tantivy (see the schema table above)
```

Records are collected across all inner WARCs before merging, because a page's
rendered `urn:text` often lives in a different WARC than its HTML response.
Rendered text is *also* read from `pages/pages.jsonl` and `pages/extraPages.jsonl`
(Browsertrix's `text` field): many crawls - including this era of SUCHO WACZs -
store the fully rendered, post-JS page text only there, not as `urn:text:` WARC
records. Without it, JS-rendered content is visible in replay but unsearchable.
The jsonl text is merged into the same per-URL document (interchangeable with
`urn:text:`), so it enriches the existing HTML-response document rather than
adding a duplicate.
Collapsing to one document per URL deduplicates repeat captures *within a WACZ*;
repeat captures of the same URL *across* WACZs stay as separate documents and are
grouped at query time instead (see *Faceted, temporal discovery*).

Parallelism: Rayon parallel iterator over the WARC member list within each WACZ;
merge and Tantivy writes happen once per WACZ.

### CDX-guided extraction (default) vs. full scan (fallback)

There are two extraction modes, sharing the same per-record transform
(`record_to_raw`) and merge step (`index_merged`) so they produce an identical
index:

- **CDX-guided** (`index_wacz_streaming`, over a pluggable `RangeFetch` byte
  source) - **the default**: read the WACZ's CDX, then fetch *only* the page
  records (HTML/PDF responses and `urn:text:` rendered text) at
  `data_start + offset` for the length the CDX gives. Media (images/JS/CSS/JSON)
  and pseudo-records (pageinfo, thumbnail) are never read. It also reads the
  `pages/*.jsonl` `text` once during setup (cheap, alongside the CDX), folding it
  in as rendered text - so crawls that store rendered text only in the pages files
  are fully searchable even though the CDX never points at it. The byte source is
  pluggable (`http_range.rs`): a local `FileFetch`, or an `HttpFetch` issuing HTTP
  range requests for a remote WACZ - so a remote WACZ is indexed **without
  downloading it**, fetching only the central directory, the CDX, and the page
  records. The same primitive wabac.js uses for replay.
- **Scan** (`index_wacz`) - **the fallback**: decompress and inspect *every*
  WARC record. Used only when a WACZ can't be CDX-guided (see below).

**Why CDX-guided is the default everywhere.** Extraction mode is really about
CDX-guided vs. full-scan, not local vs. remote - the reader abstracts over where
the bytes live. And **replay already resolves records through the CDX** (wabac.js
range-reads against it), so a WACZ with a broken CDX wouldn't replay regardless;
indexing from the same index keeps the two consistent, and there's no additional
trust to lose. The remaining reason to scan is purely mechanical: some WACZs
*can't* be CDX-guided.

Requirements and caveats, grounded in the WACZ spec:

- CDX-guided extraction relies on the spec's SHOULD that `archive/` WARCs are
  **Stored** (uncompressed) in the ZIP, so a CDX byte offset maps to an absolute
  position. A WARC gzip member is one record; the CDX `index.cdx.gz` is often a
  multi-member gzip (ZipNum blocks), so it's read with `MultiGzDecoder`.
- **Automatic fallback to scan.** `local_warcs_streamable` / `remote_warcs_streamable`
  probe the central directory: if the WARCs aren't Stored (a WACZ deflates them,
  violating the SHOULD) or there's no readable CDX, indice scans instead. For a
  remote host without range support, the fallback downloads a temp copy and scans
  it, keeping the URL as the source. No user flag selects the mode; it's decided
  per WACZ.
- `--download` instead fetches the remote WACZ into `<home>/archive` and indexes
  it as a local file (durable copy, whole-file SHA-256, offline replay). The
  downloaded copy is itself CDX-guided when its WARCs are Stored.
- **Fixity of streamed sources**: a streamed remote is never read in full, so it
  has no whole-file SHA-256 (empty in the manifest) and its `file_size` comes
  from the HTTP `Content-Length`. `verify` already skips remote sources, so this
  is consistent; per-resource integrity from `datapackage-digest.json` is future
  work (see *Planned*).

The ZIP/CDX **setup** (central directory, CDX, per-WARC data-starts, warcinfo)
runs serially over a buffered `RangeReader`, which uses a rolling read-ahead
buffer for forward reads plus a one-time cache of the last 1 MiB (the EOCD +
central directory). The ZIP central directory is at the end of the file and is
touched once per entry while local headers are read scattered across the file;
without the tail cache those two regions thrash a single buffer, turning the open
of a multi-GB ZIP64 WACZ into hundreds of range requests.

The per-record **fetch** phase is **concurrent**: the CDX gives each record an
independent `(offset, length)`, so a dedicated pool of `--concurrency` workers
(default 4 for remote — gentle on the host; CPU count for local, and clamped to a
per-host ceiling of 64) each does an independent
`RangeFetch::fetch` + gunzip + extract, driven by an atomic progress counter.
This hides per-record round-trip latency - the big win for remote WACZs, which
would otherwise be one serial HTTP round trip each - and parallelizes HTML/PDF
text extraction (CPU) across cores. Remote is latency-bound so more workers than
cores helps; local fetch is cheap and the work is CPU-bound extraction, so the
core count is the sweet spot.

**Resilient + polite remote fetching.** Every HTTP fetch (the range GETs and the
whole-file downloads) retries transient failures - network errors and HTTP
`429`/`502`/`503`/`504` - with capped exponential backoff + jitter, honoring a
server `Retry-After` (`with_retry` in `http_range.rs`). This makes a long ingest
survive blips, and is deliberately *polite*: when a host pushes back we wait
rather than hammer it. That matters because a single WACZ's `--concurrency`
requests all hit one host, so an aggressive setting against a small (non-object-
store) server could otherwise overload it or get the client IP-blocked. As a
proactive backstop the resolved worker count is clamped to a per-host ceiling
(`MAX_CONCURRENCY` = 64), so even a mis-typed `--concurrency 500` can never put an
unbounded number of range requests in flight against a single host. The agent
is built with `http_status_as_error(false)` so `4xx`/`5xx` come back as
inspectable responses (status + `Retry-After`) rather than opaque errors.

### Nested multi-WACZ

A WACZ can nest: a **multi-WACZ** is a ZIP whose payload is other `.wacz` files
rather than top-level `archive/` WARCs. This is what Browsertrix's combined
collection `/download` returns for a collection with more than one crawl — its
`datapackage.json` sets `profile: "multi-wacz-package"` and the inner `.wacz`
files are top-level `Stored` entries (confirmed against a real download). Nesting
is a Webrecorder/Browsertrix **convention, not part of the WACZ spec**, so
`wacz::nested_wacz_entries` detects it *structurally* — `.wacz` entries present
and *no* `archive/` WARCs — rather than depending on the non-standard `profile`
string (which also makes it work for multi-WACZs from other tools). `index_nested`
(in `index.rs`) then extracts each inner `.wacz` to a temp file and indexes it
through the ordinary per-WACZ path (CDX-guided or scan), **flattening** them into
one manifest entry tagged with the outer crawl's id — its `page_count` /
capture-range / provenance aggregate the inner crawls (approach A). Works for a
local file or a remote URL (the inner WACZs are read out of the outer ZIP, over
range requests when remote). Without this, the flat-WACZ assumption would index a
multi-WACZ as *empty*, silently. **Replay is unaffected** — ReplayWeb.page /
wabac.js already resolves nested WACZs over range reads.

### Representative-image thumbnails

To make the UI visual, each crawl gets a small representative image on its card
and detail pages, chosen in tiers (first hit wins):

0. a **Browsertrix page screenshot** of the main page, if the crawl captured one.
   Browsertrix (with screenshots enabled - common) stores a rendered image of each
   page as a WARC record keyed by a `urn:` URL: `urn:thumbnail:<page>` (a small,
   ready-made JPEG - preferred) or `urn:view:<page>` (the full-page PNG). It's an
   actual picture of the page, so it beats every heuristic below and works even for
   JS-rendered sites; matched on the exact page URL (tolerating a trailing-slash
   difference).
1. else the crawl's **main-page `og:image`** (the site's own social-preview image;
   `twitter:image` next);
2. else the **largest content image the main page embeds** (`<img>`/`srcset`,
   resolved against the page URL);
3. else the **largest captured image on the crawl's own registrable domain**
   (`site_of`), read straight from the CDX.

Tiers 1-3 matter for crawls *without* screenshots: `og:image` is far from
universal - cultural-heritage crawls (SUCHO) and even some magazines omit it - and
tier 3 specifically handles **JS-rendered sites**, whose *saved* HTML has no
`og:image` and no `<img>` at all (images are injected client-side), yet the crawl
still captured them; it ignores third-party/CDN/ad images on other domains. Tiers
2-3 pick by captured byte size within a window (`MIN_IMAGE_BYTES` 5 KB ..
`MAX_IMAGE_BYTES` 3 MB): the floor skips icons/sprites/tracking pixels, the ceiling
avoids fetching + decoding a full-res original for a 400px thumbnail. (Tier 0's
screenshot is purpose-built, so it skips that window.)

After indexing a CDX-streamable WACZ, `thumbnail::generate` (best-effort) checks
for a screenshot first, then reads the main page's HTML for the `og:image` /
embedded-image tiers, range-fetches the chosen image from the CDX, decodes +
downscales it (the `image` crate; longest edge 400px), and writes
`<home>/index/thumbs/<crawl_id>.jpg`. Any failure - no usable image, an image that
isn't captured or won't decode - just means no thumbnail (the UI shows a
placeholder; a curator can pin one).

A curator can **pin a specific image** with `indice crawl set <id> --image
<file>` (any local PNG/JPEG/WebP/GIF): it's downscaled and written to the
committable `collections/<slug>/crawls/<id>.jpg`. Its *presence* is the pin
marker — a pinned image lives with the finding aid and a later (re)index (which
only writes the auto cache under `index/thumbs/`) never overwrites it. A
**collection-wide** image can be pinned with `collection set --thumbnail <file>`
(`collections/<slug>/thumbnail.jpg`), which the homepage card prefers over a
member crawl's thumbnail.

The server serves crawl thumbnails at `GET /thumb/{id}` (preferring the committed
pinned image over the auto cache) and collection thumbnails at
`GET /collection-thumb/{slug}`. The homepage collection card and the
crawl detail page each show one image; the **collection detail page shows a grid
of its member crawls**, each with its own thumbnail, so the page conveys that a
collection spans multiple crawls of multiple sites. When a crawl has no image the
UI falls back to a **CSS-only placeholder** (a gradient tinted by a hash of the
name - no image bytes). Thumbnails are generated at index time, so populating
them needs a (re)index.

### Progress reporting

Indexing reports progress through a small, UI-agnostic `IndexProgress` trait
(`begin` / `phase` / `set_total` / `set_records` / `finish`): the library only
emits counts and phase labels, so it stays free of any UI dependency. The binary
implements the trait with an [indicatif] bar - an indeterminate spinner during
setup (probe / download / reading the CDX), which flips to a determinate bar with
throughput and ETA once the CDX yields the page-record total. A fresh bar is
created per WACZ (and cleared when it finishes), so it's only on screen while a
WACZ is being worked on and never collides with log lines.

Logging vs. the bar (all overridable via `RUST_LOG`): an interactive `index`
hushes `INFO` (the bar carries progress; `WARN`/`ERROR` still print); `-v` /
`--verbose` shows `DEBUG` logs and no bar; a non-TTY (piping / CI) keeps `INFO`
and shows no bar, so logs aren't lost.

[indicatif]: https://docs.rs/indicatif

---

## ReplayWebPage Assets

`static/replay/` holds the [ReplayWeb.page][rwp] JS bundle (`ui.js` + `sw.js`), embedded in the binary at compile time via `rust-embed` and served under `/replay/`. Replay runs in WACZ-direct mode: the `<replay-web-page>` component reads the WACZ over byte-range from our `/files/{id}` endpoint and serves every resource client-side through its service worker (`sw.js`); indice does no server-side rewriting (see *viewer.html* / `server.rs`).

These two files **are committed**, **pinned** to a specific `replaywebpage` npm release (currently **2.4.6**), so builds are reproducible and offline. They are vendored assets, not a Cargo dependency, so **Dependabot does not track them** - upgrading is a deliberate manual step via `scripts/fetch-replay.sh`:

```sh
./scripts/fetch-replay.sh          # re-fetch the pinned VERSION (2.4.6)
./scripts/fetch-replay.sh 2.4.7    # fetch a specific version (one-off)
```

To upgrade: pick a version from <https://www.npmjs.com/package/replaywebpage>, bump `VERSION` in `scripts/fetch-replay.sh`, re-run it (downloads `ui.js`/`sw.js` from the jsDelivr npm CDN), rebuild, **re-test replay in a browser** (`cargo test -p indice-lib --test browser` needs Chrome + chromedriver), then commit the refreshed assets. Do this periodically so replay keeps up with wabac.js fixes.

[rwp]: https://replayweb.page

---

## Key Crates

| Crate | Role |
|---|---|
| `axum` 0.8 | HTTP server |
| `tokio` 1.x | Async runtime |
| `tower-http` 0.7 | Compression, tracing middleware |
| `clap` 4.x | CLI (derive API) |
| `indicatif` 0.17 | Indexing progress bar / spinner |
| `tantivy` 0.26 | Full-text search engine with snippet generation |
| `zip` 2.x | WACZ ZIP reading |
| `url` 2.x | URL parsing |
| `scraper` 0.27 | HTML parsing and text extraction |
| `serde_json` 1.x | JSON APIs and CDXJ parsing |
| `rust-embed` 8.x | Embed ReplayWebPage assets at compile time |
| `rayon` 1.x | Parallel WARC scan + concurrent CDX-guided record fetch |
| `tracing` + `tracing-subscriber` | Structured logging with per-level line coloring |
| `anyhow` 1.x | Error propagation |
| `flate2` 1.x | gzip decompression (WARC, WACZ CDX) |
| `sha2` 0.10 | Collection file hashing |
| `chrono` 0.4 | Date formatting |
