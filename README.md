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

indice is a single self-contained binary — ReplayWeb.page assets are embedded at
build time, so there's nothing else to fetch or configure.

### Prebuilt binary (fastest — no toolchain)

Download the archive for your platform from the
[latest release](https://github.com/edsu/indice/releases/latest) (macOS
arm64/x86_64, Linux x86_64/arm64, Windows x86_64), unpack it, and you have the
`indice` binary plus a small sample archive (`apod.wacz`) to try it on — see
[Try it](#try-it-in-a-minute).

> **macOS:** an unsigned download is quarantined by Gatekeeper. Clear it once
> with `xattr -d com.apple.quarantine ./indice` (notarized builds are planned).

### With cargo

```sh
cargo install --git https://github.com/edsu/indice --locked indice
```

Builds and installs the `indice` command into `~/.cargo/bin` (needs a
[Rust toolchain](https://rustup.rs)).

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

## Try it in a minute

The prebuilt archive includes `apod.wacz` — a small sample crawl of NASA's
Astronomy Picture of the Day. Index it into a collection, then start the server:

```sh
indice index --collection "APOD" apod.wacz   # build the search index from the sample
indice serve                                 # http://127.0.0.1:8080
```

Open <http://127.0.0.1:8080> and you can full-text search the captured pages,
narrow by the facets, and replay the archived site in your browser. Point
`indice index` at your own `.wacz` files the same way (local paths or
`http(s)://` URLs); `indice serve --manage` adds an in-browser interface for
adding and curating crawls.

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

Re-running `index` **skips** sources already indexed into a collection, so a
large or interrupted ingest is safe to re-run — it resumes where it left off.
Pass `--force` to re-index (refresh) a source that's already there.

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

## Importing from Archive-It

If your crawls live in an [Archive-It](https://archive-it.org/) account (the
Internet Archive's subscription web-archiving service), `indice import
archive-it` pulls them in. Unlike Browsertrix, Archive-It serves **WARC files**
(via WASAPI), not WACZ — so indice downloads each crawl's WARCs, **builds a WACZ
from them** (one WACZ per crawl, the same builder as `wacz build`), and indexes
it. WACZ creation is an implementation detail; you select crawls.

Credentials come from the **environment**, never the command line:

```sh
export ARCHIVEIT_USER='you@example.org'
read -rs ARCHIVEIT_PASSWORD; export ARCHIVEIT_PASSWORD   # prompts, no echo
```

Then import - preview first with `--dry-run`, then pull for real:

```sh
# every active collection in the account, into ~/webarchive
indice import archive-it --home ~/webarchive --dry-run
indice import archive-it --home ~/webarchive

# just one collection (by numeric id) → a matching indice collection
indice import archive-it --collection 18491 --home ~/webarchive

# a single crawl, grouped into a named collection
indice import archive-it --crawl 2626199 --into "Stephen Ratcliffe Papers" --home ~/webarchive
```

Notes:

- **One WACZ per crawl.** WASAPI groups WARC files by crawl (job); indice bundles
  each crawl's WARCs into a single WACZ, preserving per-crawl temporality and
  giving a clean incremental unit. `--dry-run` lists the individual WARC files.
- **Finished, non-deleted crawls only.** A crawl Archive-It marks deleted, or one
  that didn't finish, is skipped (indice checks the Partner API's crawl status).
  `--include-deleted` overrides the deletion skip.
- **Selection.** `--collection <ID>` (the numeric Archive-It collection id) limits
  to one collection; `--crawl <ID>` to a single crawl; `--crawl-time-after` /
  `--crawl-time-before` filter by crawl date. The default is every collection in
  the account (including inactive ones, which still hold importable crawls).
- **Incremental.** Re-running skips crawls already imported (matched by
  host + collection + crawl id), so syncing is cheap; `--force` re-imports anyway.
- **Durable.** Because indice downloads the WARCs and builds a local WACZ, replay
  works offline and needs no Archive-It credentials at replay time. Downloads are
  staged under `<home>` (one crawl at a time, cleaned up as it goes) so a large
  crawl uses your disk, not `/tmp`. `--host <URL>` targets a non-default host.
- **Grouping & metadata.** Without `--into`, each Archive-It collection maps to an
  indice collection of the same name, and its title/description seed the finding
  aid. `--into <NAME>` groups everything into one named collection (and is how to
  reach crawls that aren't in any Archive-It collection). `--limit <N>` caps how
  many crawls per collection.

Both importers are also available in [management mode](#management-mode) as
browse-and-import wizards on the accession desk, using the server's configured
credentials.

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
  files are copied into `<home>/archive/`; a URL is streamed in place. Browsertrix
  and Archive-It are additional source tabs: browse the configured account and
  pick crawls to import, with the same live progress.

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
entirely to your proxy/SSO: anyone it logs in can administer.

The management routes show the workroom chrome + signed-in identity from the
proxy's identity header. The public pages (home, collection, crawl) are ungated,
and browsers won't send the proxy's credentials there — so at login indice sets a
small **signed, display-only session cookie** (HMAC'd with the shared secret) and
reads it on those pages, so a signed-in admin gets the edit-in-place controls
everywhere. The cookie only drives *rendering* — every write is still re-checked
against the proxy's identity header + secret, so a stolen or forged cookie grants
no access. Pages served without an identity show a **Log in** link (it points at
the gated `/manage/login`, so following it trips the proxy's login and returns you
to where you were). A **Log out** link clears the display cookie — but note that
with the Basic-auth stopgap the browser keeps its cached credentials until it's
closed, so logout only hides the chrome; a full sign-out (and single sign-on)
comes with the SSO path.

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

## Deployment

Two options, depending on who is running indice:

- **On your laptop** you can grab a prebuilt binary and run `indice serve`. That's the
  whole story for local use; see [Install](#install) and
  [Try it](#try-it-in-a-minute).
- **As a server** — run the container image behind a TLS-terminating proxy. The
  batteries-included [`compose.yaml`](compose.yaml) does this in one command with
  [Caddy](https://caddyserver.com), which fetches and renews a Let's Encrypt
  certificate for you.

### Container image

A multi-arch image (`linux/amd64` + `linux/arm64`) is published to the GitHub
Container Registry on every release:

```sh
docker run -p 8080:8080 -v indice-data:/data ghcr.io/edsu/indice:latest
```

`/data` is indice's home (`archive/` + `index/`) — mount a volume so it survives
restarts. The image runs the read-only server by default.

### One command with Caddy (recommended)

[`compose.yaml`](compose.yaml) runs indice behind Caddy:

```sh
# local / dev — plain HTTP on :80
docker compose up -d

# production — set your domain and Caddy provisions HTTPS automatically
SITE_ADDRESS=archive.example.org docker compose up -d
```

Caddy passes byte-range requests straight through so ReplayWeb.page's ranged
reads of large WACZs replay correctly through the proxy. Named volumes persist
indice's `/data` and Caddy's certificates. (`compose.yaml` builds the image
from the repo by default; to pull the published image instead, follow the
comment in the file.)

Load archives by indexing into the running container:

```sh
docker compose cp your.wacz indice:/data/your.wacz
docker compose exec indice indice index --collection "Your Collection" /data/your.wacz
```

### Management over the network

The simplest management needs none of this: `indice serve --manage` on loopback
(the [local case](#local-use)) trusts every request — it's just you on your
machine, no login. `docker compose up` alone is **read-only**. To open the
in-browser management surface to authenticated admins *over the network*, add one
of two overlays, both built on the same
[forward-auth](#running-as-a-service-forward-auth) mechanism:

- **Basic auth** (below) — a single admin password, nothing else to run. The
  quickest way to get management over the network.
- **Single sign-on** ([next section](#single-sign-on-oauth2-proxy)) — log in with
  GitHub / Google / OIDC via oauth2-proxy, with a real logout. The upgrade for
  multiple admins or existing SSO.

**Basic auth.** [`compose.manage.yaml`](compose.manage.yaml) is an overlay that
adds the write surface. It keeps the public site read-only and gates `/manage` +
the write APIs behind HTTP Basic auth, forwarding the authenticated user to indice
via forward-auth. Run it alongside the base file:

```sh
docker compose -f compose.yaml -f compose.manage.yaml up -d
```

It needs three values, e.g. in a `.env` file next to `compose.yaml`:

```sh
ADMIN_USER=you
ADMIN_PASSWORD_HASH='$2a$14$...'          # single-quoted — see the note below
INDICE_AUTH_PROXY_SECRET=a-long-random-string
```

Generate the password hash with Caddy:

```sh
docker run --rm caddy:2 caddy hash-password --plaintext 'yourpassword'
```

> **The bcrypt hash contains `$`, which docker compose interpolates.** In a
> `.env` file, wrap it in **single quotes** so it's taken literally — bare and
> double-quoted values get mangled and Caddy then fails to start with a
> `base64-decoding password` error. (If you can't single-quote, double every `$`
> instead: `$` → `$$`.) Confirm the container got a valid 60-character hash with
> `docker compose exec caddy printenv ADMIN_PASSWORD_HASH`.

Basic auth is a simple stopgap for one or a few admins. For real single sign-on,
use the SSO overlay below instead.

### Single sign-on (oauth2-proxy)

[`compose.sso.yaml`](compose.sso.yaml) + [`Caddyfile.sso`](Caddyfile.sso) put
[oauth2-proxy] in front of the management surface, so admins log in with GitHub
(or any OIDC provider) instead of a shared password. indice's forward-auth is
unchanged — oauth2-proxy performs the login and Caddy forwards the identity — and
you get a **real logout** (`/logout` clears indice's display cookie *and*
oauth2-proxy's session).

The example uses **GitHub**, which is the easiest to try: GitHub allows
`http://localhost` callback URLs, so you can test the whole flow locally.

1. Create a **GitHub OAuth App** (Settings → Developer settings → OAuth Apps →
   New) with **Authorization callback URL** `http://localhost/oauth2/callback`
   (use your `https://your-domain/oauth2/callback` for production).
2. Put its credentials in `.env`:
   ```sh
   GITHUB_CLIENT_ID=...
   GITHUB_CLIENT_SECRET=...
   OAUTH2_PROXY_COOKIE_SECRET=      # a 32-char secret from: openssl rand -hex 16
   INDICE_AUTH_PROXY_SECRET=a-long-random-string
   # GITHUB_USER=you            # allow-list a single login (defaults to edsu)
   # OAUTH2_PROXY_COOKIE_SECURE=true   # in production (HTTPS)
   # OAUTH2_PROXY_REDIRECT_URL=https://your-domain/oauth2/callback
   ```
3. Run it alongside the base file:
   ```sh
   docker compose -f compose.yaml -f compose.sso.yaml up -d
   ```

Only the allow-listed GitHub user(s) can reach management; everyone else sees the
read-only site. Clicking **Log in** sends you to GitHub; after authorizing you
land back where you were with the workroom chrome. To widen access beyond one
user, set `OAUTH2_PROXY_GITHUB_ORG` / `OAUTH2_PROXY_GITHUB_TEAM` (or switch
`OAUTH2_PROXY_PROVIDER` to Google/OIDC/etc.) — see the
[oauth2-proxy docs][oauth2-proxy].

## Command line

```
indice index           [--home <DIR>] [--name <NAME>] --collection <NAME> [-f|--from-file <FILE>] [--download] [--force] [--no-optimize] [--concurrency <N>] [-v|--verbose] <PATH|URL>...
indice reindex         [--home <DIR>] [--concurrency <N>] [-v|--verbose]
indice optimize        [--home <DIR>] [--max-segments <N>] [-v|--verbose]
indice stats           [--home <DIR>]
indice config          [--home <DIR>]
indice serve           [--home <DIR>] [--bind <ADDR>] [--manage] [--auth-proxy-header <HEADER> --auth-proxy-secret <SECRET>]
indice collection set  [--home <DIR>] <NAME> [--creator <TEXT>] [--dates <TEXT>] [--rights <TEXT>] [--subject <SUBJECT>]... [--narrative <MD> | --narrative-file <FILE>] [--thumbnail <FILE>] [--description <TEXT>] [--curator <TEXT>]
indice collection list [--home <DIR>]
indice crawl set       [--home <DIR>] <CRAWL_ID> [--image <FILE>] [--note <MD> | --note-file <FILE>]
indice crawl list      [--home <DIR>] [<COLLECTION>]
indice search-url      [--home <DIR>] <URL>
indice verify          [--home <DIR>]
indice import browsertrix [--home <DIR>] [--host <URL>] [--org <SLUG>] [--collection <ID|SLUG>] [--crawl <ID>] [--into <NAME>] [--include-unreviewed] [--min-review <N>] [--limit <N>] [--dry-run] [--stream] [--force] [-v]
indice import archive-it [--home <DIR>] [--host <URL>] [--collection <ID>] [--crawl <ID>] [--into <NAME>] [--crawl-time-after <DATE>] [--crawl-time-before <DATE>] [--limit <N>] [--dry-run] [--include-deleted] [--force] [-v]
indice wacz build      [--home <DIR>] --collection <NAME> [--name <NAME>] [--title <T> | --title-file <FILE>] [--description <D> | --description-file <FILE>] [--creator <TEXT>] [--software <TEXT>] [--main-page-url <URL>] [--keyword <K>]... [--license <L>]... [--yes] [-v] <WARC>...
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
  Each WACZ is committed as it finishes, so a re-run **skips** sources already
  indexed into the collection — an interrupted large ingest resumes where it
  stopped. `--force` re-indexes a source that's already there (to refresh it).
  (A `--download` remote URL is stored under a local path whose id differs from
  the URL's, so it's re-fetched on a re-run rather than skipped.) When a batch
  ingest leaves the index **fragmented** into many segments (which slows every
  query), indice **compacts it automatically** at the end of the run — so you
  don't have to remember to run `optimize`. `--no-optimize` skips that (it just
  prints a reminder to `optimize` later instead); a healthy index, or a single
  add to an already-tidy one, is left alone either way.
- **`collection`** - `collection list` shows collections and their crawl counts;
  `collection set <NAME> …` writes a collection's finding-aid metadata (creator,
  dates, rights, subjects, a Markdown narrative, and an optional `--thumbnail`) to
  a git-committable `collections/<slug>/README.md` you can also hand-edit.
- **`crawl`** - `crawl list [<COLLECTION>]` lists individual crawls with their
  8-char ids (optionally filtered to one collection) — the ids you pass to
  `crawl set`/`crawl delete`. `crawl set <ID> --note` adds a per-crawl Markdown
  note (`collections/<slug>/crawls/<id>.md`); `crawl set <ID> --image` pins a
  crawl thumbnail there too. (WACZ→collection membership is set when indexing,
  via `index --collection <NAME>`.)
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
  older version, indice tells you to run this.) The rebuild is **atomic**: it
  builds a fresh index alongside the live one and swaps it in only once the
  rebuild finishes, so a crash, kill, or full disk mid-rebuild leaves your
  existing index intact — and a running `serve` keeps answering from the old
  index until the swap. (Transient cost: the old and new index coexist on disk
  until then, ~2× the index size.)
- **`optimize`** - compacts the search index by merging its Tantivy *segments*
  down toward `--max-segments` (default 8), **without re-fetching sources** — so
  it's much cheaper than `reindex`. Every search fans out across all segments, so
  an index that has fragmented into hundreds of tiny segments (which happens when
  Tantivy's background merges fail — classically on a full disk) gets slow;
  `optimize` merges them back down. A lower `--max-segments` compacts more but
  needs more free disk during the merge (roughly index size ÷ target). It also
  **reclaims disk from deleted crawls** — a delete only tombstones documents;
  their bytes are freed when the segment is rewritten, which `optimize` now does
  for any segment still carrying deletes (regardless of `--max-segments`) — and
  **sweeps orphaned segment files** left by an interrupted (Ctrl-C'd) run.
  Reports the `before → after` segment count and disk reclaimed. `index` runs
  this automatically when a batch ingest leaves the index fragmented, so you
  mostly only reach for it by hand to reclaim space after deleting crawls.
- **`stats`** - reports the search index's on-disk footprint, broken down by
  Tantivy file type (`.store` doc store, `.pos` positions, `.term`/`.idx`
  inverted index, `.fast` columnar, …), with bytes-per-document and projected
  sizes at 1M / 100M docs. Use it to see the effect of the frugality knobs
  (`config.yaml`) and to size a large ingest before running it.
- **`config`** - prints the resolved operator configuration for the home
  (`<home>/config.yaml`, plus the built-in defaults for anything unset): the
  stored-body cap (`index.stored_body_cap_kb`, `0` = full body) and the Tantivy
  writer heap (`index.writer_heap_mb`). See [Operator configuration](#operator-configuration).
- **`serve`** - opens the index read-only and starts the HTTP server (so you can
  `index` while it runs). Defaults to `127.0.0.1:8080`. `--manage` adds an opt-in
  browser UI + write API for adding archives and curating collections; see
  [Management mode](#management-mode) for local vs. behind-a-proxy (`--auth-proxy-*`)
  use. On startup it warns if the index is fragmented (many segments — e.g. built
  by an older version, or a killed run), pointing you to `optimize`.
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
- **`wacz build`** - the "I have WARCs, not WACZs" on-ramp: packages one or more
  `.warc`/`.warc.gz` files into a WACZ under `<home>/archive/` and indexes it.
  **`--collection <NAME>` is required.** The original WARC bytes are stored
  **verbatim** (uncompressed in the zip) - indice only *packages* your crawl
  data, it never rewrites it - and a CDX index + `datapackage.json` are generated
  so the WACZ both indexes here and is **shaped to replay in ReplayWeb.page**.
  The CDX mirrors [warcio.js](https://github.com/webrecorder/warcio.js)'s indexer
  (verified line-for-line against it) and the packaging mirrors
  [browsertrix-crawler](https://github.com/webrecorder/browsertrix-crawler), so
  the output matches what Webrecorder's own tools produce. Metadata
  (`--title`, `--description`, `--creator`, `--keyword`, `--license`, …) comes
  from flags; on an interactive terminal, missing values are prompted for
  (`--yes` skips prompting for scripts/CI). Each input WARC is sniff-tested first
  (must parse as a WARC with at least one indexable record) so a bad file fails
  fast instead of producing a broken WACZ. This is also the building block for
  importing from services that serve WARCs rather than WACZs (e.g. Archive-It).

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

## Scaling

indice is built to run the same way on a laptop with a handful of WACZs and on a
server holding an institution's collections — one binary, an embedded index, no
Solr/Elasticsearch cluster to operate. Two things make that hold up as a corpus
grows: a **frugal, predictable on-disk footprint** and **query latency that
grows sub-linearly**.

**Footprint.** Only a capped prefix of each page's body is *stored* (for
snippets); the full text is indexed for search but not stored, so the doc store
stays bounded. On a real corpus that works out to **~1.7 KB per document**. Two
commands make the footprint legible:

```sh
indice stats            # size by index file type, bytes/doc, projections to 1M/100M docs
indice stats --fields   # what fills the doc store, by stored field
```

At ~1.7 KB/doc the index projects to roughly **1.6 GB at 1M docs** and **~160 GB
at 100M**. If that's still too much, the `stored_body_cap_kb` knob (below) trades
snippet depth for a smaller store — a laptop keeps generous snippets, an
institution dials it down.

**Query latency.** Search stays interactive as the corpus grows because the
result-grouping and snippet work is bounded (a fixed candidate window), not
proportional to the corpus. Measured on a laptop with the `scale_bench` example
(single binary, one process):

| corpus | full-text search | facet overview | collection page lookup |
|--------|-----------------:|---------------:|-----------------------:|
| 100k docs | ~20 ms | ~2.5 ms | ~1.5 ms |
| 1M docs   | ~40 ms | ~16 ms  | ~1.5 ms |

10× the data costs roughly 2× the search latency — everything stays well under
100 ms at 1M docs. Indexed lookups (opening a collection, resolving a URL for
replay) are corpus-size-independent. Reproduce with:

```sh
cargo run --release --example scale_bench -- 1000000   # docs, [iters], [segments]
```

**Index health.** A search fans out across every segment, so a fragmented index
is a slow one. indice keeps the index compact automatically: `index` and the
management-mode import **auto-compact** when they detect fragmentation, `serve`
warns at startup if the index it opens is fragmented, and `indice optimize`
merges segments on demand — reclaiming disk from deleted crawls *and* sweeping
orphaned segment files left behind by an interrupted (Ctrl-C'd) run, which
otherwise linger. So an interrupted ingest self-heals on the next `index`.

## Operator configuration

An optional `<home>/config.yaml` holds home-level settings. The file is
optional and every field has a default, so you only write the knobs you want to
change; unknown keys are ignored (the file can grow over time). Run `indice
config` to print the resolved values (your file merged over the defaults).

```yaml
index:
  # Bytes of page body text STORED per document for snippets, in KiB. The full
  # body is always indexed, so search recall is unaffected — this only bounds
  # the stored copy used to render snippets. Lower = smaller index. Omit for the
  # 16 KiB default; 0 stores the whole body (no cap).
  stored_body_cap_kb: 16
  # Tantivy indexing buffer, in MiB — the build-time RAM ceiling / throughput
  # knob. Higher = fewer, larger segments (faster bulk ingest, more RAM). Omit
  # for the 50 MiB default; values below Tantivy's ~15 MiB minimum are clamped
  # up (so 0 is *not* unlimited here).
  writer_heap_mb: 50
```

These apply on `index` and `reindex`; use `indice stats` to see their effect on
the footprint. After **upgrading** to a version that changes the index schema
or these defaults, run `indice reindex` to rebuild — that's how existing homes
pick up footprint improvements (a stale index built by an older version is
detected, and indice tells you to reindex).

## Benchmarking

`scripts/bench-ingest.sh` measures indexing a WACZ — wall time, peak RSS, a
per-phase timing breakdown, and the resulting index footprint:

```sh
scripts/bench-ingest.sh path/to/crawl.wacz            # benchmark the current build
scripts/bench-ingest.sh path/to/crawl.wacz <git-ref>  # compare a baseline ref vs. current
```

It builds `--release`, indexes the WACZ into a throwaway home under
`/usr/bin/time`, and reports the phases the indexer times itself:

- **read+extract** — fetch each record and extract text (HTML/PDF)
- **index** — tokenize and add documents to the writer buffer
- **checksum** — whole-file fixity hash
- **commit** — flush the Tantivy segment to disk

The footprint comes from `indice stats` (bytes by index file type +
bytes-per-doc, with projections). Passing a git ref builds that revision
in a temporary worktree and prints a **before/after** — handy for confirming a
change's effect on real data. Peak RSS + wall time use macOS's `/usr/bin/time
-l`; on GNU/Linux use `/usr/bin/time -v` and adjust the field names.

For **query** latency at scale (rather than ingest), the `scale_bench` example
builds a synthetic index of N docs and times the hot query paths (faceted
search, the facet overview, collection page lookups), reporting p50/p95 — see
[Scaling](#scaling):

```sh
cargo run --release --example scale_bench -- 1000000    # N docs, [iters], [target segments]
```

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
