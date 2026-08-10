# indice

[![CI](https://github.com/edsu/indice/actions/workflows/ci.yml/badge.svg)](https://github.com/edsu/indice/actions/workflows/ci.yml)

**Note bene**: *indice is alpha software and has been written extensively
with the support of Claude Code. Like any piece of software it may contain
bugs. The developer's understanding of how it operates at a low level may be
limited. See the [DESIGN.md](DESIGN.md) document for the overall design
principles. Technical reviews of the code and design are always welcome!*

---

**indice** is a web archive server written in Rust. Think of it as a [reading
room] for web archives. Point it at a pile of local or remote [WACZ] files and
it gives you:

- **Full-text search with faceted, temporal browsing**: hit-highlighted
  snippets, then narrow by collection, site, date, type, or language, with a
  timeline for navigating through time
- **Provenance up front**: see how each crawl was made (software, operator,
  dates, seeds, page counts) and verify each WACZ's fixity, instead of taking
  the archive on faith
- **In-browser replay** of the archived pages via [ReplayWeb.page] / wabac.js
- **Backroom interface** that allows authenticated users to edit collection
  metadata and descriptions.

It ships as a single self-contained binary - no Solr, no Elasticsearch, no
separate database server. That's a deliberate design goal: indice is built for
**small, local, and private** use, for example a person indexing a handful of their own
WACZ files on a laptop, with nothing sent to a hosted service. It also uses the
same model to **scale up** toward institutional collections. It aims to fit both
ends of that range, rather than assuming the infrastructure of a large web
archive.

> **The web archive replay is entirely [Webrecorder]'s work.** indice bundles
> and serves [ReplayWeb.page] and [wabac.js] - the browser-side engine that does
> all the actual replay - and adds a thin Rust layer for indexing, search, and
> serving. Webrecorder did the heavy lifting; please support them. See
> [Credits](#credits).

## Why "indice"?

An *indice* is a sign that points beyond itself, and the name gathers three
senses of the same idea. [Suzanne Briet] argued that a wild antelope becomes
a *document* once it is captured, catalogued, and set aside as evidence. She
defined a document as *"un indice concret ou symbolique, conservé ou
enregistré"* (a concrete or symbolic sign, preserved or recorded). [Charles
Sanders Peirce] used *index* for the same family of sign: one bound to its
object by a real, existential connection like smoke to fire, or a weathervane
to the wind. Squint a little bit and a web capture is like that too: a trace
connected to a moment of the live web. And, of course, indice builds
a full-text **index** over the archives it serves, so the simplest meaning
applies too.

## Discovery and provenance

The "reading room" idea is that you should be able to *find* things in an
archive and *understand* what you're looking at - not just replay a URL you
already know. Two findings from web-archiving research shape indice (both
expanded, with citations, in [DESIGN.md](DESIGN.md)):

- **Web-archive use is mostly navigational and temporal** - seeing a page or
  site as it was, or how it changed over time (Costa & Silva's query-log study
  of the Portuguese Web Archive). So time is a first-class axis, and facets beat
  one long scrolling list as an archive grows. indice has a faceted results
  page (collection, site, date, type, language), a month timeline, and grouping
  of repeat captures of the same URL - the faceted, full-text "slice and dice"
  browsing that [SHINE] (UK Web Archive) and [SolrWayback] (Royal Danish Library)
  established over the [warc-indexer]. indice owes both a clear debt; it just
  trades their Solr backend for a single embedded Tantivy index - so the same
  faceted search runs with no cluster to operate, fitting a private laptop
  archive as readily as an institutional one.
- **Provenance is part of the record** - to trust and interpret an archive you
  need to know how it was made: the crawler software, operator, dates, and seeds
  (Maemura et al., *If These Crawls Could Talk*). indice reads this from the
  WACZ and WARC and surfaces it on each collection and WACZ - and lets you
  verify each file's fixity - rather than burying it.

## Install

indice is a single self-contained binary. You need a
[Rust toolchain](https://rustup.rs) (Rust 2021 / a recent stable compiler).

### With cargo (recommended)

```sh
cargo install --git https://github.com/edsu/indice --locked indice
```

This builds and installs the `indice` command into `~/.cargo/bin`. The
ReplayWeb.page assets are embedded at build time, so there is nothing else to
fetch or configure.

### From a clone (for development)

```sh
git clone https://github.com/edsu/indice
cd indice
cargo build --release
# binary at ./target/release/indice
```

The bundled ReplayWeb.page assets are committed to the repo, so a fresh clone
builds and runs as-is. To upgrade them later, run `./scripts/fetch-replay.sh`
and rebuild.

## How it works

indice runs [ReplayWeb.page] in **WACZ-direct mode**. Rather than
reimplementing web replay on the server (URL rewriting, redirect handling,
fuzzy matching, serving individual archived resources), indice hands the whole
job to the well-tested [wabac.js] service worker running in the browser:

```
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

When you open a page for replay, the browser fetches the WACZ directly from
`GET /files/{id}` using HTTP range requests, reads the CDX index embedded inside
the WACZ, and serves every resource from the WARC records - all client-side.
indice's job during replay is simply to serve bytes efficiently. Everything
else (search, metadata, the collection homepage) is what indice is actually
good at.

You can also **replay a whole collection** at once (from the homepage card or the
collection page): `GET /collection/{id}/replay.json` hands wabac.js a multi-WACZ
manifest listing every member crawl, so a link from one crawl to a page archived
in another crawl *of the same collection* resolves on demand. That resolution is
answered from the search index via `GET /collection/{id}/pages` - which also feeds
the viewer's page-list sidebar - so the browser never has to load every member's
index, and it scales from a handful of WACZs to institutional collections.
Resolution is scoped to the collection, and page sub-resources always come from
the page's own crawl, preserving per-page temporal coherence.

See [DESIGN.md](DESIGN.md) for the full architecture.

## Quick start

indice keeps everything under a **home directory** (default: the current
directory):

```
<home>/
├── archive/<slug>/     your WACZ files, organized by collection
├── collections/<slug>/ finding aids you author + commit (README.md, thumbnails, notes)
└── index/              search index + derived metadata (rebuildable; git-ignore it)
```

The `collections/` folder is the part worth keeping in version control - the prose
and images a curator writes. `index/` is derived from the WACZs and rebuilt by
`indice reindex`, so a home in git typically `.gitignore`s `/index`.

Index one or more WACZ files into a collection, then serve:

```sh
indice index my-archive.wacz --collection "My Web Archive"   # files it into archive/my-web-archive/
indice serve                                                 # http://127.0.0.1:8080
```

Every crawl belongs to a **collection**, so `index` requires `--collection <NAME>`
(created if new). This is a deliberate nudge to say what a crawl is a part of and
why you're keeping it — the curatorial context indice is built to surface.
Describe a collection further (creator, dates, rights, a scope note) with
[`indice collection set`](#command-line), which writes a git-committable finding
aid at `collections/<slug>/README.md`.

A local WACZ can live anywhere - `indice index path/to/foo.wacz --collection
"Bar"` files it into `<home>/archive/bar/` for you (**moving** it if it's already
under `archive/`, **copying** it otherwise, so your original is left intact). The
source is stored relative to home, so you can move or copy the whole `<home>`
directory to another disk or machine and it still works. Point at a different home
with `--home <DIR>` (every command takes it).

`index` takes one or more archived WACZ files or `http(s)` URLs, so you can also
index a single file or a remote WACZ:

```sh
indice index archive/my-archive.wacz --collection "My Web Archive"
indice index https://example.org/site.wacz --collection "My Web Archive"
```

To rebuild the index later from what you've already indexed, use
[`indice reindex`](#command-line) instead of re-listing everything.

Open <http://127.0.0.1:8080/>, search, and click a result to replay it.

(If you built from a clone instead of installing, use `./target/release/indice`
in place of `indice`.)

Re-indexing the same WACZ is an upsert - safe to re-run any time to add or
refresh collections.

### Remote WACZ files

A WACZ can also live at an `http(s)` URL. For example, this one is hosted on S3:

```sh
indice index https://edsu-webarchives.s3.amazonaws.com/docnow.wacz --collection "DocNow"
indice serve
```

By default indice **streams** a remote WACZ. It never downloads the whole file.
Using the WACZ's internal CDX index, it reads (via HTTP range requests) only
the pieces it needs: the ZIP central directory, the CDX, and the HTML/PDF page
records. It skips images, video, JS, and CSS entirely. On a media-heavy archive
the indexable text is a tiny fraction of the WACZ: a 323 MB WACZ can be indexed
in a few seconds. The URL is recorded as the source, and at replay time the
browser reads the remote WACZ directly (also via range requests).

For this to work the remote host must serve the WACZ with **HTTP range support
and CORS** allowing indice's origin. The S3 bucket above is configured that
way (`Accept-Ranges: bytes` and `Access-Control-Allow-Origin: *`), which is why
S3 and other object stores work with no special support - expose the object as a
range and CORS-capable HTTPS URL (public or presigned) and index it.

If you'd rather keep a **local copy**, add `--download`:

```sh
indice index --download https://edsu-webarchives.s3.amazonaws.com/docnow.wacz --collection "DocNow"
```

This fetches the WACZ into `<home>/archive`, indexes it as a local file, and
records a whole-file SHA-256 - a durable copy you can replay offline and check
with `indice verify`. indice also falls back to downloading automatically if
a remote host doesn't support range requests, or if the WACZ stores its WARCs
compressed (the WACZ spec says the `archive/` WARCs *should* be stored
uncompressed so they can be read by range; a few tools don't).

Streaming a large remote WACZ makes one HTTP range request per page record. Those
requests are latency-bound and independent, so indice fetches them concurrently
(4 at a time by default — gentle on arbitrary hosts; raise it, e.g.
`--concurrency 16`, for object stores like S3). Fetches
retry transient failures (rate limits and `5xx`) with backoff, honoring
`Retry-After`, so a long ingest survives blips and stays gentle on the host - be
mindful that a high `--concurrency` all hits a single host, so dial it down for
small servers (it's fine for object stores like S3). As a backstop the worker
count is capped at 64 per host, so a mis-typed value can't flood a single server. `index` shows a progress
bar - a spinner while it reads the CDX, then a bar with the throughput and an ETA
once it knows how many records there are - so you can see it working. Add
`-v`/`--verbose` for detailed logs instead of the bar; when output isn't a
terminal (piping to a file or CI) it prints plain log lines and no bar.

### How indexing reads a WACZ

By default indice reads a WACZ through its internal **CDX index**, fetching only
the records that become pages (HTML, PDFs, and Browsertrix's rendered `urn:text`)
and skipping images, video, JS, and CSS. It also reads the fully rendered page
text from `pages/pages.jsonl` and `pages/extraPages.jsonl` — many crawls store the
post-JS text only there, so this keeps JS-rendered content searchable, not just
visible in replay. This works the same way for local and
remote WACZs - the only difference is *how* the bytes are read: a remote WACZ over
HTTP range requests (no download), a local WACZ straight from the file.

It falls back to a **full scan** of every WARC record only when a WACZ can't be
read via its CDX - its WARCs are stored compressed (the WACZ spec says the
`archive/` WARCs *should* be stored uncompressed so they can be read by offset; a
few tools don't), or it has no readable CDX. For a remote WACZ whose host doesn't
support range requests, the fallback downloads a temporary copy and scans it.

indice trusts the CDX because **replay already does**: the in-browser player
resolves each record through the CDX, so a WACZ with a broken CDX wouldn't replay
anyway. Indexing from the same index keeps the two consistent.

## Searching

The search box matches page titles, headings, body text, descriptions,
keywords, author, and words from the page URL. A few things worth knowing
(there's also a "Search tips" panel in the app itself):

- **All words must match.** `climate policy` finds pages containing both words.
  Use `OR` for either (`climate OR weather`) and `-` to exclude (`climate -policy`).
- **Quotes** search an exact phrase: `"climate policy"`.
- **Field search**: `title:climate` and `author:hopper` match those fields;
  `site:example.com` matches a whole site across subdomains while
  `domain:www.example.com` is an exact host; `year:2021` (or `year:[2020 TO 2023]`),
  `month:202103`, and `modified:2015` (Last-Modified year) filter by date;
  `type:pdf`, `lang:en`, `status:200`, and `collection:demo` filter by media
  type, language, HTTP status, and collection.
- **Grouping and boosting**: `(climate OR weather) risk`, and `climate^2 change`
  ranks "climate" matches higher.

Title matches rank above body matches, and searches are case-insensitive.

The results page is faceted: a sidebar shows counts by collection, year, site,
type, and language, and clicking one refines the search (applied filters appear
as removable chips). A month timeline sits above the results — click a bar to
filter to that month. Repeat captures of the same URL collapse into a single
result marked "captured N times", and results are paginated. The homepage also
offers "browse by year" and "top sites" entry points into search.

Crawls carry a representative image, cached as a small thumbnail at index time.
It's taken from the crawl's home-page `og:image`; failing that, the largest
content image the page embeds; and failing *that* — for JS-rendered sites whose
saved HTML lists no images — the largest captured image on the crawl's own
domain (skipping icons/sprites and full-res originals).
Homepage collection cards and the crawl detail page show one; the collection
detail page shows a grid of its member crawls, each with its own image —
conveying that a collection spans multiple crawls of multiple sites. Crawls
without an image fall back to a CSS placeholder. A curator can pin a specific
image with `indice crawl set <crawl-id> --image <file>` (kept across reindexing).

## Importing from Browsertrix

If your WACZs live in a [Browsertrix](https://browsertrix.com/) account
(Webrecorder's hosted crawler), `indice import browsertrix` downloads them into
`<home>/archive` and indexes them as durable local sources.

Credentials come from the **environment**, never the command line, so they don't
show up in the process list:

```sh
export BROWSERTRIX_USER='you@example.org'
read -rs BROWSERTRIX_PASSWORD; export BROWSERTRIX_PASSWORD   # prompts, no echo
# or, instead of user/password:  export BROWSERTRIX_TOKEN='<a JWT>'
```

Then import - preview first with `--dry-run`, then pull for real:

```sh
# everything you've QA'd in your (only) org, into ~/webarchive
indice import browsertrix --home ~/webarchive --dry-run
indice import browsertrix --home ~/webarchive

# just one collection (by id, slug, or name) → a matching indice collection
indice import browsertrix --collection us-govarchive --home ~/webarchive

# a single crawl
indice import browsertrix --crawl <item-id> --home ~/webarchive
```

Notes:

- **QA'd crawls only, by default.** Browsertrix lets a reviewer rate a crawl
  (`reviewStatus`); indice imports only reviewed crawls so you publish vetted
  content. Add `--include-unreviewed` to import everything, or `--min-review <1-5>`
  for a rating threshold. A single named `--crawl` is always imported. When crawls
  are skipped for this reason, indice says so.
- **Selection.** `--collection <ID|SLUG|NAME>` limits to one Browsertrix
  collection; `--crawl <ID>` to a single archived item; neither imports the whole
  org. `--org <SLUG>` picks the org when your account has more than one.
- **Incremental.** Re-running skips crawls already imported (matched by content
  hash), so syncing an account is cheap; `--force` re-imports anyway.
- **Durable by default.** WACZs are downloaded into `<home>/archive/<item-id>/`
  (a subfolder per Browsertrix item, so items can't clash on a shared filename),
  because Browsertrix's presigned URLs expire after ~48h - a downloaded copy keeps
  replay working long-term. `--host <URL>` targets a self-hosted Browsertrix
  (default is `https://app.browsertrix.com`).
- **`--stream` (index-only footprint).** Instead of downloading, index the WACZ
  in place from Browsertrix and store only its stable identity, not a copy. Since
  presigned URLs expire, indice re-resolves a fresh one on demand — so **`serve`
  needs the same `BROWSERTRIX_*` credentials** to replay these crawls (they show a
  503 otherwise). Good for a self-hoster who wants search over their own crawls
  without keeping the bytes; download (the default) is better for a durable,
  offline, or shared library.
- **Grouping.** Importing a `--collection` groups its crawls into a indice
  collection of the same name. `--into <NAME>` overrides that name (and is the way
  to group an org-wide or single-`--crawl` import, which otherwise land as
  individual collections). `--limit <N>` caps how many are imported; `--dry-run`
  lists them without downloading.

## Management mode

By default `indice serve` is **read-only** — it never writes, so you curate from
the command line (`index`, `collection set`, `import browsertrix`). Passing
`--manage` turns the ordinary site into an editable **workroom**: the same pages
gain curation controls (a warm clay "red-tape" accent marks write mode), so you can add
archives and curate collections in place, no command line needed:

```bash
indice serve --manage        # http://127.0.0.1:8080
```

With `--manage` on:

- **The homepage** — its collection list gains a **+ New collection** button, and
  each card an **Edit** affordance. An empty instance greets you with "add your
  first archive."
- **Each collection page** — gains **Edit collection** (the finding-aid form:
  description, creator, dates, rights, subjects, narrative) and **+ Add crawls**.
- **Add crawls** (the accession desk) — upload a `.wacz` from your computer, or
  point indice at a local path or an `http(s)://` URL. Indexing runs in the
  background with live progress; when it finishes the crawl is searchable
  immediately (the server hot-reloads its reader — no restart). Uploaded/local
  files are copied into `<home>/archive/`; a URL is streamed in place. (Importing
  from Browsertrix / Archive-It will slot in here as additional sources.)

The default `serve` (without `--manage`) mounts none of this, so a public,
read-only deployment can never mutate the archive.

### Local use

`indice serve --manage` bound to `127.0.0.1` (the default) trusts every request:
you're the only one who can reach it, so you're the admin and there's no login.
Because it trusts everything, indice **refuses to start** if `--manage` is bound
to a non-loopback address without an auth proxy configured (below) — otherwise
you'd expose an unauthenticated write surface to the network.

### Running as a service (forward-auth)

To offer management to real users over the network, run indice behind an
**authenticating reverse proxy** — nginx, Caddy, [oauth2-proxy], Authelia,
Cloudflare Access, Tailscale, an institutional SSO gateway, and so on. The proxy
performs the login and forwards the authenticated user to indice in a header;
indice trusts that header only when the request also carries a shared secret:

```bash
indice serve --manage \
  --bind 127.0.0.1:8080 \
  --auth-proxy-header X-Forwarded-Email \
  --auth-proxy-secret "$INDICE_AUTH_PROXY_SECRET"   # or set that env var
```

- **`--auth-proxy-header`** is the header your proxy injects with the
  authenticated identity (e.g. `X-Forwarded-Email` for oauth2-proxy,
  `Remote-Email` for Authelia).
- **`--auth-proxy-secret`** (or the `INDICE_AUTH_PROXY_SECRET` env var) is a random
  secret your **proxy** must send in the `X-Indice-Auth-Secret` header. It is a
  static header you set in the proxy config — *not* something your identity
  provider sends. Requiring it is what makes trusting the identity header safe: a
  client that forges `X-Forwarded-Email`, or any request that didn't come through
  the proxy, lacks the secret and gets a `403`.

Every management request must carry both the identity header and the secret;
anything else is rejected. The public read-only site (search, browse, replay) is
**not** gated — only the management routes are. "Who is an admin" is delegated
entirely to your proxy/SSO: anyone it logs in can administer, and the signed-in
identity is shown in the workroom strip at the top of every management page.

**Deploy checklist**

- Bind indice to loopback and have the proxy connect to it there, so nothing but
  the proxy can reach the port.
- Configure the proxy to **strip any client-supplied** identity header on inbound
  requests before setting its own, so a client can't smuggle one in. (The shared
  secret is your backstop if this is ever missed.)
- Set the static `X-Indice-Auth-Secret` header in the proxy, and terminate TLS
  there.

Illustrative Caddy config (adapt directives to your proxy/version):

```caddy
example.org {
    # 1. require an SSO login (oauth2-proxy talks to your IdP)
    forward_auth 127.0.0.1:4180 {
        uri /oauth2/auth
        copy_headers X-Forwarded-Email          # the authenticated identity
    }
    # 2. proxy to indice, adding the shared secret
    reverse_proxy 127.0.0.1:8080 {
        header_up X-Indice-Auth-Secret {env.INDICE_AUTH_PROXY_SECRET}
    }
}
```

[oauth2-proxy]: https://oauth2-proxy.github.io/oauth2-proxy/

## Command line

```
indice index           [--home <DIR>] [--name <NAME>] --collection <NAME> [-f|--from-file <FILE>] [--download] [--concurrency <N>] [-v|--verbose] <PATH|URL>...
indice reindex         [--home <DIR>] [--concurrency <N>] [-v|--verbose]
indice optimize        [--home <DIR>] [--max-segments <N>] [-v|--verbose]
indice serve           [--home <DIR>] [--bind <ADDR>] [--manage] [--auth-proxy-header <HEADER> --auth-proxy-secret <SECRET>]
indice collection set  [--home <DIR>] <NAME> [--creator <TEXT>] [--dates <TEXT>] [--rights <TEXT>] [--subject <SUBJECT>]... [--narrative <MD> | --narrative-file <FILE>] [--thumbnail <FILE>] [--description <TEXT>] [--curator <TEXT>]
indice collection list [--home <DIR>]
indice crawl set       [--home <DIR>] <CRAWL_ID> [--image <FILE>] [--note <MD> | --note-file <FILE>]
indice search-url      [--home <DIR>] <URL>
indice verify          [--home <DIR>]
indice import browsertrix [--home <DIR>] [--host <URL>] [--org <SLUG>] [--collection <ID|SLUG>] [--crawl <ID>] [--into <NAME>] [--include-unreviewed] [--min-review <N>] [--limit <N>] [--dry-run] [--stream] [--force] [-v]
```

Every command takes `--home <DIR>` (default `.`); `archive/` and `index/` are
derived siblings under it.

- **`index`** - indexes one or more archived WACZ files or `http(s)://` URLs (at
  least one). By default indice reads a WACZ through its internal **CDX index**,
  extracting only the page records (and falling back to a full WARC scan only when
  a WACZ can't be read that way - see [How indexing reads a
  WACZ](#how-indexing-reads-a-wacz)). A remote URL is **streamed** over HTTP range
  requests, no download (see [Remote WACZ files](#remote-wacz-files)). A local
  WACZ may live anywhere - indice files it into `<home>/archive/<slug>/`
  (moving it if already under `archive/`, else copying it), and a directory or
  non-`.wacz` path is an error. Index several with a shell glob. Extracts
  searchable text from each page (HTML, Browsertrix's rendered `urn:text` records
  or `pages/*.jsonl` text, and PDFs), reads `datapackage.json` for collection
  metadata, and records
  everything in the manifest under `<home>/index/`, including the SHA-256 of each
  local WACZ. Local WACZ paths are stored relative to home so the folder is
  portable. The WACZ name comes from `--name` if given, otherwise the WACZ's
  `datapackage.json` title, otherwise the filename. **`--collection <NAME>` is
  required** — every crawl belongs to a curated collection (created if new); there
  are no auto singletons. `--download` fetches a remote WACZ into
  `<home>/archive/<collection-slug>/` for a
  durable local copy instead of streaming it in place. To index many at once, pass
  a newline-delimited list of files/URLs with `--from-file <FILE>` (or `-f -` to
  read from stdin); blank lines and `#` comments are ignored, and it combines with
  any positional args. `--concurrency <N>` sets how many records are fetched at
  once during CDX-guided (streaming) indexing (default: 4 for remote URLs — gentle
  on the host, raise for object stores like S3; CPU count for local files; capped
  at 64 per host). Indexing shows a progress bar on an interactive terminal;
  `-v`/`--verbose` replaces it with debug logs. A **multi-WACZ** (a WACZ that
  bundles other WACZs, e.g. a Browsertrix combined-collection download) is
  detected automatically and its inner crawls indexed too, into one entry.
- **`collection`** - `collection list` shows collections and their members;
  `collection set <NAME> …` writes a collection's finding-aid metadata (creator,
  dates, rights, subjects, a Markdown narrative, and an optional `--thumbnail`) to
  a git-committable `collections/<slug>/README.md` you can also hand-edit.
  `crawl set <ID> --note` adds a per-crawl Markdown note
  (`collections/<slug>/crawls/<id>.md`); `crawl set <ID> --image` pins a crawl
  thumbnail there too. (WACZ→collection membership is set when indexing, via
  `index --collection <NAME>`.)
- **`reindex`** - rebuild the search index from the WACZs already in the
  manifest, preserving collection membership and metadata. Re-fetches remote URL
  sources and recreates the index from scratch, so it's the way to migrate after
  an upgrade changes the index schema. It's resilient: a source that can't be
  indexed (a missing local file, or a remote source still failing after retries)
  is skipped with a warning rather than aborting the rebuild; the mostly-rebuilt
  index is still usable, and if anything was skipped the command exits non-zero
  with a summary count so you (or cron/CI) know to re-run it once fixed. Takes
  `--concurrency <N>` and shows the same progress bar as `index` (a full reindex
  re-streams every source, so it can take a while); `-v`/`--verbose` swaps the bar
  for debug logs. (If you try to `index` or `serve` against an index built by an
  older version, indice tells you to run this.)
- **`optimize`** - compacts the search index by merging its Tantivy *segments*
  down toward `--max-segments` (default 8), **without re-fetching sources** — so
  it's much cheaper than `reindex`. Every search fans out across all segments, so
  an index that has fragmented into hundreds of tiny segments (which happens when
  Tantivy's background merges fail — classically on a full disk) gets slow;
  `optimize` merges them back down. A lower `--max-segments` compacts more but
  needs more free disk during the merge (roughly index size ÷ target). Reports the
  `before → after` segment count.
- **`serve`** - opens the index read-only and starts the HTTP server (so you can
  `index` while it runs). Defaults to `127.0.0.1:8080`. `--manage` adds an opt-in
  browser UI + write API for adding archives and curating collections; see
  [Management mode](#management-mode) for local vs. behind-a-proxy (`--auth-proxy-*`)
  use.
- **`search-url`** - a debugging aid: reads the CDX index *inside* each WACZ and
  prints the records matching a URL. No separate CDX store is maintained; the
  WACZ's own index is authoritative.
- **`verify`** - re-hashes every registered WACZ and compares against the
  SHA-256 recorded at index time, reporting each as `OK`, `MODIFIED`, or
  `MISSING`. Exits non-zero if any collection fails, so it works in a cron job
  or CI. This is indice's fixity check - a small guard against the archive
  quietly bit-rotting or being tampered with.
- **`import browsertrix`** - imports WACZ files from a [Browsertrix](https://browsertrix.com/)
  instance (Webrecorder's hosted crawler) - the "index your own crawls" path.
  See [Importing from Browsertrix](#importing-from-browsertrix).

## Testing

```sh
cargo test              # unit + integration tests (no browser needed)
```

Most tests run without a browser, including server-side *replay-contract* tests
that assert what wabac.js depends on: the WACZ we serve is byte-identical to
disk, byte-range requests return the correct slice, the served archive's CDX
resolves a page, and the viewer wires up `<replay-web-page>` correctly.

Actual replay rendering can only be checked in a real browser, so there's one
`#[ignore]`d end-to-end test that drives headless Chrome via WebDriver and
confirms an archived page renders from a WACZ we serve:

```sh
chromedriver --port=9515 &          # WebDriver server; must match your Chrome's major version
cargo test -p indice-lib --test browser -- --ignored
```

- Override the WebDriver endpoint with `WEBDRIVER_URL` (default
  `http://localhost:9515`).
- `chromedriver`'s major version must match your installed Chrome. If they
  differ, grab a matching build from
  [Chrome for Testing](https://googlechromelabs.github.io/chrome-for-testing/).
- On macOS, a Homebrew `chromedriver` is quarantined and gets killed on launch;
  clear it once with `xattr -d com.apple.quarantine $(which chromedriver)`.

## Credits

indice stands almost entirely on the shoulders of [Webrecorder]. The hard
part - faithfully replaying an archived page in the browser - is done by their
[ReplayWeb.page] and [wabac.js] (which bundles wombat.js), both of which indice
ships and serves unmodified. It also builds on the open [WACZ] format and the
broader web-archiving community. If indice is useful to you, please support
Webrecorder's work.

## License

indice is licensed under the **GNU Affero General Public License v3.0 or
later** (AGPL-3.0-or-later) - the same license as the ReplayWeb.page and
wabac.js components it bundles. See [LICENSE](LICENSE) for the full text and
[NOTICE](NOTICE) for third-party attributions and bundled-asset details.

[WACZ]: https://specs.webrecorder.net/wacz/latest/
[Webrecorder]: https://webrecorder.net/
[ReplayWeb.page]: https://replayweb.page/
[Paris Web]: https://www.paris-web.fr/
[Olivier Thereaux]: https://github.com/olivierthereaux
[rustyweb-orig]: https://github.com/olivierthereaux/rustyweb
[Karl Dubost]: https://www.la-grange.net/karl/
[1000ans]: https://www.24joursdeweb.fr/2012/un-site-web-de-1000-ans/
[wabac.js]: https://github.com/webrecorder/wabac.js
[reading room]: https://inkdroid.org/2026/06/03/jan6-doj-archive/
[SHINE]: https://github.com/ukwa/shine
[SolrWayback]: https://github.com/netarchivesuite/solrwayback
[warc-indexer]: https://github.com/ukwa/webarchive-discovery
[Suzanne Briet]: https://en.wikipedia.org/wiki/Suzanne_Briet
[Charles Sanders Peirce]: https://plato.stanford.edu/entries/peirce-semiotics/
