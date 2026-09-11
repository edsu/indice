---
title: Command-line reference
description: Every indice subcommand, its options, and what it does.
---

Every command takes `--home <DIR>` (default: the current directory), which is the
[home directory](/docs/reference/home-directory/) holding `archive/`, `collections/`, and `index/`.
It's omitted from the synopses below to keep them readable. Each command's own `--help` is the
authoritative summary; this page gives the detail.

| Command | Does |
|---|---|
| [`index`](#indice-index) | Index WACZ files or URLs into a collection |
| [`serve`](#indice-serve) | Start the reading-room web server |
| [`reindex`](#indice-reindex) | Rebuild the search index from what's already indexed |
| [`optimize`](#indice-optimize) | Compact the search index without re-fetching sources |
| [`verify`](#indice-verify) | Re-hash indexed WACZs to check fixity |
| [`stats`](#indice-stats) | Report the index's on-disk footprint |
| [`config`](#indice-config) | Show the resolved operator configuration |
| [`health`](#indice-health) | Probe a running server's `/health` |
| [`search-url`](#indice-search-url) | Print CDX records matching a URL |
| [`collection`](#indice-collection) | Create, describe, list, and delete collections |
| [`crawl`](#indice-crawl) | Annotate, list, and delete individual crawls |
| [`import`](#indice-import) | Import from Browsertrix or Archive-It |
| [`wacz build`](#indice-wacz-build) | Package WARC files into a WACZ and index it |

## `indice index`

Index one or more WACZ files or `http(s)://` URLs into a collection.

```text
indice index [OPTIONS] --collection <NAME> <PATH|URL>...
```

| Option | Does |
|---|---|
| `--collection <NAME>` | **Required.** The collection these WACZs belong to (created if new) |
| `--name <NAME>` | Display name for the WACZ (default: its `datapackage.json` title, else the filename) |
| `-f`, `--from-file <FILE>` | Read more paths/URLs from a text file, one per line (`-` for stdin) |
| `--download` | Fetch a remote WACZ into `<home>/archive/` and index it as a local file instead of streaming it in place. No effect on local sources |
| `--force` | Re-index sources already indexed in this collection (default: skip them) |
| `--concurrency <N>` | Records fetched at once during CDX-guided indexing (default: 4 remote, CPU count local; capped at 64 per host) |
| `--no-optimize` | Skip the automatic post-ingest compaction |
| `-v`, `--verbose` | Debug logs instead of the progress bar |

**`--collection` is required** because every crawl belongs to a curated collection. There are no
auto-created singletons. This is deliberate, to encourage curators to say what a crawl is part of
and why it's being kept.

**Where files land.** A local WACZ may live anywhere; indice files it into
`<home>/archive/<collection-slug>/`, **moving** it if it's already under `archive/` and **copying**
it otherwise, so your original is left intact. Paths are stored relative to home, so the whole home
directory stays portable. A directory or a non-`.wacz` path is an error. Index several with a shell
glob, or with `--from-file` (blank lines and `#` comments are ignored, and it combines with
positional arguments).

**How it reads a WACZ.** By default indice goes through the WACZ's internal **CDX index**, fetching
only records that become pages (HTML, PDFs, and Browsertrix's rendered `urn:text` records), plus
the fully rendered page text in `pages/*.jsonl`. It falls back to a full WARC scan only when a WACZ
can't be read that way; see [How indexing reads a WACZ](/docs/reference/how-it-works/#how-indexing-reads-a-wacz)
and the [WACZ guide](/docs/guides/wacz/). A remote URL is **streamed** over HTTP range requests with
no download at all (see [Remote WACZ files](/docs/quickstart/#remote-wacz-files)). It also reads
`datapackage.json` for collection metadata and records the SHA-256 of each local WACZ for later
`verify`.

**Resumable.** Each WACZ is committed as it finishes, so re-running **skips** sources already
indexed into the collection and an interrupted large ingest picks up where it stopped. `--force`
re-indexes (refreshes) one that's already there. Note that a `--download`ed remote URL is stored
under a local path whose id differs from the URL's, so it is re-fetched on a re-run rather than
skipped.

**Multi-WACZ.** A WACZ that bundles other WACZs, e.g. a Browsertrix combined-collection
download, is detected automatically, and its inner crawls are indexed too, as one entry.

**Automatic compaction.** When a batch ingest leaves the index fragmented into many segments (which
slows every query), indice compacts it at the end of the run so you don't have to remember
[`optimize`](#indice-optimize). `--no-optimize` skips that and prints a reminder instead. A healthy
index, or a single add to an already-tidy one, is left alone either way.

## `indice serve`

Open the index read-only and start the HTTP server. You can `index` while it runs.

```text
indice serve [OPTIONS]
```

| Option | Does |
|---|---|
| `-b`, `--bind <ADDR>` | Address to listen on (default `127.0.0.1:8080`) |
| `--manage` | Enable [management mode](/docs/guides/manage/): mount the opt-in write endpoints and workroom UI |
| `--auth-proxy-header <HEADER>` | Read the authenticated user from this header (e.g. `X-Forwarded-Email`) behind a trusted proxy. Requires `--auth-proxy-secret` |
| `--auth-proxy-secret <SECRET>` | Shared secret the proxy must send in `X-Indice-Auth-Secret`. Also `INDICE_AUTH_PROXY_SECRET` |
| `--site-url <URL>` | This site's public URL, for the cross-site (CSRF) check on writes. Also `INDICE_SITE_URL` |

The public server is **read-only** by default. `--manage` adds the browser write surface; without an
auth proxy it trusts every request, so it must bind to a loopback address. The `--auth-proxy-*` pair
is what lets `--manage` bind to a non-loopback address for a real deployment. The secret's presence
is what makes trusting the identity header safe. See [Manage &amp; curate](/docs/guides/manage/) and
[Deploy](/docs/guides/deploy/).

`--site-url` is only needed behind a proxy that rewrites `Host` without setting `X-Forwarded-Host`
(nginx's default); Caddy and a direct bind are detected automatically.

On startup `serve` warns if the index is fragmented, meaning many segments, e.g. built by an older
version or left by a killed run. It points you at `optimize`.

## `indice reindex`

Rebuild the search index from the WACZs already in the manifest, preserving collection membership
and metadata.

```text
indice reindex [OPTIONS]
```

| Option | Does |
|---|---|
| `--concurrency <N>` | Records fetched at once while re-streaming each source (default: 4 remote, CPU count local; capped at 64 per host) |
| `-v`, `--verbose` | Debug logs instead of the progress bar |

This re-fetches remote URL sources and recreates the index from scratch, so it's the way to migrate
after an upgrade changes the index schema. If you try to `index` or `serve` against an index built by
an older version, indice tells you to run this.

The rebuild is **atomic**: it builds a fresh index alongside the live one and swaps it in only once
finished. A crash, kill, or full disk mid-rebuild leaves your existing index intact, and a running
`serve` keeps answering from the old index until the swap. The transient cost is that both indexes
coexist on disk until then, roughly 2× the index size.

It's also resilient: a source that can't be indexed (a missing local file, or a remote source still
failing after retries) is skipped with a warning rather than aborting the rebuild. The mostly-rebuilt
index is still usable, and if anything was skipped the command exits non-zero with a summary count,
so you, or cron, or CI, know to re-run it once the source is fixed.

## `indice optimize`

Compact the search index by merging its Tantivy *segments*, **without re-fetching sources**, which is much
cheaper than `reindex`.

```text
indice optimize [OPTIONS]
```

| Option | Does |
|---|---|
| `--max-segments <N>` | Target segment count to compact down to, ≥1 (default 8) |
| `-v`, `--verbose` | Debug logs instead of the progress spinner |

Every search fans out across all segments, so an index that has fragmented into hundreds of tiny
segments gets slow. (Classically this happens when Tantivy's background merges fail on a full disk.)
A lower `--max-segments` compacts more but needs more free disk during the merge, roughly index size
÷ the target.

`optimize` also **reclaims disk from deleted crawls**. A delete only tombstones documents, and
their bytes are freed when the segment is rewritten, which happens for any segment still carrying
deletes regardless of `--max-segments`. It also **sweeps orphaned segment files** left by an
interrupted (Ctrl-C'd) run. It reports the `before → after` segment count and the disk reclaimed.

Since `index` runs this automatically after a fragmenting ingest, you mostly reach for it by hand to
reclaim space after deleting crawls.

## `indice verify`

Re-hash every registered WACZ and compare against the SHA-256 recorded at index time.

```text
indice verify
```

Reports each as `OK`, `MODIFIED`, or `MISSING`, and exits non-zero if any fail, so it works in a
cron job or CI. This is indice's fixity check, a small guard against the archive quietly bit-rotting
or being tampered with.

## `indice stats`

Report the search index's on-disk footprint.

```text
indice stats [--fields]
```

| Option | Does |
|---|---|
| `--fields` | Also break the doc store down by stored field, to see which fields' text dominates |

The breakdown is by Tantivy file type (`.store` doc store, `.pos` positions, `.term`/`.idx` inverted
index, `.fast` columnar, …), with bytes-per-document and projected sizes at 1M and 100M docs. Use it
to see the effect of the frugality knobs in `config.yaml` and to size a large ingest before running
it. The `--fields` breakdown is sampled and measured uncompressed, while the on-disk store is
compressed, so treat it as a ratio rather than an absolute.

## `indice config`

Print the resolved operator configuration for the home, and the path to `config.yaml`.

```text
indice config
```

Shows `<home>/config.yaml` merged over the built-in defaults: the stored-body cap
(`index.stored_body_cap_kb`, `0` = full body) and the Tantivy writer heap (`index.writer_heap_mb`).
Edit that file to change settings; changes apply on the next `index` or `reindex`. See
[Operator configuration](/docs/reference/configuration/).

## `indice health`

Probe a running server's `/health` endpoint and exit 0 if healthy, non-zero otherwise.

```text
indice health [--url <URL>]
```

| Option | Does |
|---|---|
| `--url <URL>` | Health endpoint to probe (default `http://127.0.0.1:8080/health`) |

Self-contained, with no `curl` needed, so a distroless container's `HEALTHCHECK` or a Compose
healthcheck can call the binary itself.

## `indice search-url`

A debugging aid: read the CDX index *inside* each WACZ and print the records matching a URL.

```text
indice search-url <URL>
```

The URL is matched exactly against archived URLs. indice maintains no separate CDX store; each
WACZ's own index is authoritative, so this shows you what replay will see.

## `indice collection`

Collections are curated groups of WACZs, each with a git-committable finding aid at
`collections/<slug>/README.md`: YAML front-matter for the structured fields, Markdown body for the
narrative. You can hand-edit that file, or edit it in the [workroom](/docs/guides/manage/).

### `collection set`

Create or update a collection's finding-aid metadata (created if it doesn't exist).

```text
indice collection set [OPTIONS] <NAME>
```

| Option | Does |
|---|---|
| `--description <TEXT>` | Short abstract / caption (EAD `<abstract>`) |
| `--creator <TEXT>` | Collecting org or person responsible for the records (DACS Name of Creator, EAD `<origination>`) |
| `--curator <TEXT>` | Repository / owner running this indice instance (EAD `<repository>`), distinct from `--creator` |
| `--dates <TEXT>` | Curatorial coverage-date statement (EAD `<unitdate>`), distinct from the auto-derived capture range |
| `--rights <TEXT>` | Conditions governing access and use, or a license (EAD `<userestrict>`) |
| `--subject <SUBJECT>` | A topical subject / access point (repeat for several) |
| `--narrative <MD>` | The Scope &amp; Content / provenance narrative, as inline Markdown |
| `--narrative-file <FILE>` | Read that narrative from a file (`-` for stdin) |
| `--thumbnail <FILE>` | Pin a representative image for the collection (PNG/JPEG/WebP/GIF), committed under the collection |

`<NAME>` is the collection name; its id is a slug of it.

### `collection list`

```text
indice collection list
```

Lists collections and their WACZ counts.

### `collection delete`

```text
indice collection delete [OPTIONS] <ID>
```

| Option | Does |
|---|---|
| `--with-crawls` | Also delete every member crawl, not just the grouping |
| `--yes` | Skip the confirmation prompt |

`<ID>` is the collection id/slug or name. An empty collection is removed outright; a non-empty one is
refused unless `--with-crawls`, which also deletes every member crawl, files and index documents
both. **Irreversible.**

## `indice crawl`

A crawl is one indexed WACZ. `crawl list` gives you the 8-char ids the other two take. Note that a
crawl's collection membership is set at index time, via `index --collection <NAME>`, not here.

### `crawl list`

```text
indice crawl list [<COLLECTION>]
```

Lists crawls with their ids, optionally filtered to one collection (by name or slug).

### `crawl set`

Set curator-controlled properties of a crawl.

```text
indice crawl set [OPTIONS] <ID>
```

| Option | Does |
|---|---|
| `--note <MD>` | A curator note (Markdown) for this crawl, e.g. to document absences or context |
| `--note-file <FILE>` | Read that note from a file (`-` for stdin) |
| `--image <FILE>` | Pin a representative image (PNG/JPEG/WebP/GIF), overriding the auto-selected thumbnail |

The note is written to a committable `collections/<slug>/crawls/<id>.md`, and a pinned image is kept
across reindexing.

### `crawl delete`

```text
indice crawl delete [--yes] <ID>
```

Deletes the crawl's search-index documents, its manifest entry, its local WACZ (for a downloaded or
local-file source, if no longer referenced), and its thumbnail. The index reclaims the disk only on a
later segment merge, so run [`optimize`](#indice-optimize) to force it. **Irreversible**; `--yes` skips
the confirmation prompt.

## `indice import`

Import content from an external web-archiving service. Credentials for both importers come from the
**environment**, never the command line, so they don't appear in the process list.

### `import browsertrix`

Import WACZ files from a [Browsertrix](https://browsertrix.com/) instance, Webrecorder's hosted
crawler. Authenticates with `BROWSERTRIX_USER` + `BROWSERTRIX_PASSWORD`, or a `BROWSERTRIX_TOKEN`.

```text
indice import browsertrix [OPTIONS]
```

| Option | Does |
|---|---|
| `--host <URL>` | Browsertrix host, for a self-hosted instance (default `https://app.browsertrix.com`) |
| `--org <SLUG>` | Organization to import from (slug or id). Defaults to your only org; required with more than one |
| `--public` | Import a **public** collection with no credentials, via Browsertrix's public API. Requires `--org`; ignores any `BROWSERTRIX_*` variables |
| `--collection <ID\|SLUG\|NAME>` | Import only this Browsertrix collection (default: the whole org) |
| `--crawl <ID>` | Import only this archived item (crawl or upload), even if it hasn't been QA'd |
| `--into <NAME>` | Group the imported crawls into this indice collection. Without it, each crawl is its own collection |
| `--include-unreviewed` | Also import crawls no reviewer has QA'd (default: QA'd only) |
| `--min-review <N>` | Only import crawls whose QA rating is at least N (1–5). Implies reviewed-only |
| `--stream` | Index without downloading; replay re-resolves a presigned URL on demand, so `serve` needs the same credentials |
| `--limit <N>` | Import at most N items (`0` = all) |
| `--dry-run` | List what would be imported, without downloading or indexing |
| `--force` | Re-download and re-index items already imported |
| `--concurrency <N>` | Records fetched at once while CDX-guided indexing each WACZ |
| `-v`, `--verbose` | Debug logs instead of the progress bar |

See [Importing from Browsertrix](/docs/guides/import-browsertrix/) for the credential setup, why
downloading is the default (presigned URLs expire in ~48h), and when `--stream` is the better trade.

### `import archive-it`

Import crawls from an [Archive-It](https://archive-it.org/) account over WASAPI. Archive-It serves
WARC files rather than WACZs, so this downloads a crawl's WARCs, builds a WACZ from them behind the
scenes, and indexes that, one WACZ per crawl. Authenticates with `ARCHIVEIT_USER` +
`ARCHIVEIT_PASSWORD`.

```text
indice import archive-it [OPTIONS]
```

| Option | Does |
|---|---|
| `--host <URL>` | Archive-It host, where the Partner API and WASAPI live (default `https://partner.archive-it.org`) |
| `--collection <ID>` | Import only this Archive-It collection, by numeric id (default: all `ACTIVE` collections) |
| `--crawl <ID>` | Import only this crawl (job) id |
| `--into <NAME>` | Group the imported crawls into this indice collection. Without it, the Archive-It collection's name is used |
| `--crawl-time-after <DATE>` | Only WARCs crawled on or after this ISO-8601 date |
| `--crawl-time-before <DATE>` | Only WARCs crawled on or before it |
| `--include-deleted` | Also import crawls Archive-It marks deleted (default: finished, non-deleted only) |
| `--limit <N>` | Import at most N crawls per collection (`0` = all) |
| `--dry-run` | List what would be imported (collections, crawls, WARC files) without downloading |
| `--force` | Re-download and re-import crawls already imported |
| `-v`, `--verbose` | Debug logs instead of the progress bar |

See [Importing from Archive-It](/docs/guides/import-archive-it/).

## `indice wacz build`

The "I have WARCs, not WACZs" on-ramp: package one or more WARC files into a WACZ under
`<home>/archive/` and index it.

```text
indice wacz build [OPTIONS] --collection <NAME> <WARC>...
```

| Option | Does |
|---|---|
| `--collection <NAME>` | **Required.** The collection the built WACZ belongs to (created if new) |
| `--name <NAME>` | Display name for indexing (default: the title, else the filename) |
| `--title <T>` / `--title-file <FILE>` | WACZ title |
| `--description <D>` / `--description-file <FILE>` | Short description / abstract |
| `--creator <TEXT>` | Collecting organization or person (datapackage `organization`) |
| `--main-page-url <URL>` | The crawl's main page URL |
| `--keyword <K>` | A topical keyword (repeat for several) |
| `--license <L>` | A license label (repeat for several) |
| `--software <TEXT>` | The tool string recorded as `software` (default: `indice <version>`) |
| `--yes` | Skip the interactive metadata prompts; missing values then error instead |
| `-v`, `--verbose` | Debug logs instead of the progress bar |

The original WARC bytes are stored **verbatim**, uncompressed at the ZIP layer. indice only
*packages* your crawl data, it never rewrites it. A CDX index, `pages/pages.jsonl`,
`datapackage.json`, and `datapackage-digest.json` are generated so the WACZ both indexes here and is
**shaped to replay in ReplayWeb.page**. The CDX mirrors
[warcio.js](https://github.com/webrecorder/warcio.js)'s indexer, verified line-for-line against it,
and the packaging mirrors [browsertrix-crawler](https://github.com/webrecorder/browsertrix-crawler),
so the output matches what Webrecorder's own tools produce.

Metadata comes from the flags above; on an interactive terminal, missing values are prompted for, and
`--yes` skips the prompting for scripts and CI. Each input WARC is sniff-tested first: it must parse
as a WARC with at least one indexable record, so a bad file fails fast instead of producing a broken
WACZ.

This is also the building block for importing from services that serve WARCs rather than WACZs, which
is how [`import archive-it`](#import-archive-it) works. For the fuller story, including what a WARC
crawl can't give you that a browser-based capture can, see the [WACZ guide](/docs/guides/wacz/#i-have-warcs-not-waczs).
