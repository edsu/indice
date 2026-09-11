---
title: WACZ
description: What a WACZ file is, why indice is built on it, how to make one with each of the common capture tools, and what's inside once you unzip it.
---

A WACZ file is indice's unit of input. Every collection is a set of them, `indice index` reads
them, and `indice serve` replays them. If you already have WACZs, you can skip to
[making a WACZ](#making-a-wacz) or [the anatomy tour](#anatomy-of-a-wacz); if you're wondering why
indice asks for that format rather than plain WARCs, start here.

## What a WACZ file is

A **WACZ** (Web Archive Collection Zipped, pronounced "wax") is a ZIP file with an agreed-on
layout, specified by [Webrecorder](https://webrecorder.net/) as the
[WACZ specification](https://specs.webrecorder.net/wacz/latest/). Unzip one and you find four kinds
of thing:

- **`archive/`**: the crawl data itself, as ordinary [WARC](https://iipc.github.io/warc-specifications/)
  files. This is the preservation payload, and the format is unchanged: a WACZ *packages* WARCs, it
  doesn't replace them.
- **`indexes/`**: a CDX index over those WARCs, mapping each archived URL and capture time to a
  byte offset and length inside a named WARC.
- **`pages/`**: a JSON Lines list of the pages a human or crawler actually visited, with titles,
  timestamps, and (usually) the browser-extracted text of the page.
- **`datapackage.json`**: a manifest, in the
  [Frictionless Data Package](https://specs.frictionlessdata.io/data-package/) shape, listing every
  file in the archive with its size and hash, plus descriptive and provenance metadata.

## Why indice is built on it

A directory of bare WARCs is a fine preservation object but an awkward *serving* object. It has no
manifest, no index, no list of which URLs were pages as opposed to incidental subresources, and no
record of what/who made it. To search or replay it you first have to build all of that somewhere
else, which is why traditional web archive stacks come with a separate index store and a database.

A WACZ already carries those parts, and it carries them inside a ZIP, which has a central directory
at the end of the file and stores each member at a known offset. Two useful properties fall out of
that:

**The index travels with the data.** indice can read `indexes/index.cdxj` and know where every
record lives without scanning gigabytes of WARC. The page list tells it which of those records are
pages worth putting in a full-text index, and `pages/pages.jsonl` often *already contains* the
rendered text of each page, which is the text a browser saw after JavaScript ran, and frequently is not
the text in the HTML response body. That is why indice needs no separate extraction pipeline and no
headless browser at index time.

**One file can be read in pieces, remotely.** Because a WACZ is a ZIP with stored (uncompressed)
members, any byte range inside it is addressable with an HTTP range request. indice exploits this in
both directions: at index time it streams a remote WACZ, reading only the central directory, the CDX,
and the HTML/PDF records and skipping images, video, CSS, and JS entirely (see
[remote WACZ files](/docs/quickstart/#remote-wacz-files)); at replay time the browser reads the same
file the same way, because [wabac.js](https://github.com/webrecorder/wabac.js) (the engine behind
[ReplayWeb.page](https://replayweb.page/)) was designed to use this format. indice doesn't have to
unpack anything to serve it.

The rest is curatorial. `datapackage.json` hashes every member, so a WACZ is checkable after the
fact (`indice verify`), and its metadata is where indice gets the provenance it shows on a crawl
page: what software made the capture, when, for whom. And one crawl being one file makes a
collection something you can copy to a disk, hand to a colleague, or put in an object store without
a migration plan.

:::note[WACZ is not a lock-in]
The WARC bytes inside are untouched and standard. `unzip yourfile.wacz` gets you your WARCs back,
readable by every other web archive tool. indice depends on the *arrangement*, not on a proprietary
container.
:::

## Making a WACZ

indice reads WACZ files but does not create web archives itself: there is no built in crawler. That is
deliberate: capture is legitimately hard, Webrecorder and others do it very well, and the format is the seam that
lets those tools and this one meet. Which capture tool you want depends mostly on how much of the
web you're after and how much of a human needs to be in the loop.

| Tool | Shape of the work | Reach for it when |
|---|---|---|
| [ArchiveWeb.page](#archivewebpage) | Browser extension / desktop app; you browse, it records | A human has to click, log in, scroll, or decide |
| [Scoop](#scoop) | CLI, one URL per run, high fidelity | You need a single page as *evidence*, with provenance and a signature |
| [Browsertrix](#browsertrix) | Hosted service, scheduled crawls, multi-user | Recurring crawls, a team, review workflows, no servers to run |
| [browsertrix-crawler](#browsertrix-crawler) | Docker CLI crawler, YAML config | You want the same crawler locally, scripted, on your own machine |
| [browsertrix-crawler-claude](#browsertrix-crawler-claude) | Claude Code plugin wrapping the above | You'd rather describe the crawl than write the YAML |

### ArchiveWeb.page

[ArchiveWeb.page](https://archiveweb.page/) is Webrecorder's interactive capture tool, available both
as a Chromium extension and as a standalone desktop app. You turn on recording and browse; whatever
loads in the tab is captured, at full fidelity, including anything that only appears because you
clicked it. When you're done you download the session as a `.wacz`.

Use it when the page can't be crawled without a person: sites behind a login you hold, content that
appears only after a search or a form submission, infinite feeds you want to scroll a specific
distance, or a one-off "capture this before it changes" errand. It is the least automatable and the
most faithful to intent: you archived exactly what you looked at.

The catch is that it doesn't scale, and it doesn't repeat. For a whole site, or for the same site
every month, you probably want a crawler.

### Scoop

[Scoop](https://github.com/harvard-lil/scoop) is Harvard Library Innovation Lab's capture engine:
"a high fidelity, browser-based, web archiving capture engine for witnessing the web." It is the
engine behind [Perma.cc](https://perma.cc/), and its framing is the giveaway: *witnessing*. Scoop
captures **one URL per run** and puts its effort into making that single capture defensible.

```sh
npm install -g @harvard-lil/scoop     # needs Node.js 22+
scoop "https://lil.law.harvard.edu"   # → ./archive.wacz
```

Alongside the WARC records, Scoop can gather a provenance summary of the capture, a screenshot, a
PDF snapshot, a DOM snapshot, the SSL/TLS certificates presented by the server, extracted video with
subtitles, and the raw exchanges for forensic inspection. It also implements the
[WACZ Signing and Verification](https://specs.webrecorder.net/wacz-auth/latest/) specification,
so a capture can be cryptographically signed and later verified as having existed at a given time.

Reach for Scoop when the capture is a citation or an exhibit whose authenticity may be questioned
later.

### Browsertrix

[Browsertrix](https://browsertrix.com/) is Webrecorder's hosted crawling service: you point it at
seeds, set scope rules, give it a schedule, and it runs real browsers in the cloud and hands back
WACZs. Since it is open source you can also run it relatively easily using Docker Desktop.

On top of the crawling it adds the things a team needs, including organizations and user roles,
collections, browser profiles for authenticated sites, and a QA workflow where a reviewer rates a
finished crawl before it counts as good.

This is the path of least infrastructure for recurring crawls, and indice imports directly from it:

```sh
indice import browsertrix --collection us-govarchive --home ~/webarchive
```

See [Importing from Browsertrix](/docs/guides/import-browsertrix/) for credentials, incremental
syncing, and the `--stream` mode that indexes crawls without keeping a copy of the bytes. indice
honors the QA review status by default, so only crawls a human has vetted get indexed.

### browsertrix-crawler

[browsertrix-crawler](https://github.com/webrecorder/browsertrix-crawler) is the crawler underneath
the hosted service, and it runs perfectly well on its own machine as a Docker container. You
describe a crawl in YAML (seeds, scope, depth, page limits, worker count, custom behaviors) and it
writes a WACZ into a collection directory:

```sh
docker run -v $PWD/crawls:/crawls webrecorder/browsertrix-crawler crawl \
  --url https://example.org/ --scopeType prefix \
  --generateWACZ --collection example
# → ./crawls/collections/example/example.wacz
indice index crawls/collections/example/example.wacz --collection "Example"
```

Choose this over the hosted service when the crawl should stay on your hardware, when you want it in
a cron job or a CI pipeline, or when you need to write a **custom behavior** (a snippet of
JavaScript that performs custom clicks that feed back into the crawl queue. 
Behaviors are where crawling stops being declarative, and they're the main reason
to be running the crawler yourself. Although you can also use these behaviors in the Browsertrix service itself.

### browsertrix-crawler-claude

[browsertrix-crawler-claude](https://github.com/edsu/browsertrix-crawler-claude) is a
[Claude Code](https://claude.com/claude-code) plugin that puts a conversational front end on
browsertrix-crawler. The crawl still runs in Webrecorder's container; the plugin supplies the
scaffolding, the scripts, and a skill for writing and debugging custom behaviors.

```
/plugin marketplace add edsu/browsertrix-crawler-claude
/plugin install btrix@browsertrix-crawler-claude
```

Then, from a working directory:

| Command | Does |
|---|---|
| `/btrix:new <name> <url>` | Scaffold `config/<name>.yaml`, asking about scope |
| `/btrix:profile <url> [name]` | Create a browser login profile for an authenticated site |
| `/btrix:run <name>` | Run the crawl in the background |
| `/btrix:status <name>` | Progress and rate-limit check |
| `/btrix:review <name>` | Finished-crawl summary and replay pointers |
| `/btrix:view <name>` | Replay the finished WACZ locally in ReplayWeb.page |

Output lands in `./collections/<name>/<name>.wacz`, ready for `indice index`. The real advantage is
the awkward middle of a crawl: describing scope in a sentence instead of remembering whether you
want `prefix` or `host`, and getting help writing the behavior for the one site whose content only
appears after three clicks. It needs Docker or Podman, since the crawler is still a container.

## I have WARCs, not WACZs

Plenty of web archive data predates WACZ or comes from tools that emit WARCs: `wget --warc-file`,
`wpull`, Heritrix, or an export from an institutional archive. For those, indice packages the WARCs
into a WACZ and indexes the result in one step:

```sh
indice wacz build crawl.warc.gz --collection "My Crawl"
indice wacz build *.warc.gz --collection "My Crawl" --title "City Council, 2019–2024" \
  --creator "Municipal Archives" --keyword "local government" --yes
```

As with `indice index`, `--collection <NAME>` is required: every crawl belongs to something a
curator has named. The built WACZ lands under `<home>/archive/<slug>/` and is indexed immediately.

What `wacz build` does, and doesn't do:

- **Your WARC bytes are stored verbatim.** They go into `archive/` as-is, uncompressed at the ZIP
  layer, and the CDX offsets are read from the originals. indice never rewrites crawl data; if you
  unzip the result you get back the exact files you handed it, byte for byte.
- **It generates the parts a WACZ needs.** A sorted `indexes/index.cdxj`, a `pages/pages.jsonl` with
  one seed page per HTML `200`, a `datapackage.json`, and a `datapackage-digest.json`.
- **The output is shaped to match Webrecorder's own tools.** The CDX generation mirrors
  [warcio.js](https://github.com/webrecorder/warcio.js)'s indexer, including its SURT
  canonicalization, which is pinned by a conformance test against warcio.js itself, and the
  packaging mirrors browsertrix-crawler. That matters because it means the WACZ doesn't just index
  here, it replays in ReplayWeb.page like any other.
- **Metadata comes from flags, or a prompt.** `--title`, `--description`, `--creator`, `--keyword`,
  `--license`, `--main-page-url`, `--software`. On an interactive terminal indice asks for anything
  missing; `--yes` skips the prompting for scripts and CI, and errors instead.
- **Bad input fails fast.** Each WARC is sniff-tested first: it has to parse and contain at least
  one indexable record, so you get an error rather than a WACZ that's quietly broken.
- **It does not extract rendered text.** A WARC from a non-browser crawler has no browser-extracted
  page text to carry, so `pages.jsonl` is written with `hasText: false` and indice indexes the text
  of the HTML response bodies instead. This is the real fidelity difference between a wget crawl and
  a browser-based one, and no amount of repackaging fixes it.

This same packaging is what the [Archive-It importer](/docs/guides/import-archive-it/) uses
internally, since Archive-It serves WARCs rather than WACZs; there, the source crawl and collection
records are also carried along in `datapackage.json` so the provenance survives the trip.

## Anatomy of a WACZ

Here is a real WACZ, a single-page capture of NASA's Astronomy Picture of the Day, made by
browsertrix-crawler, the same file the [quick start](/docs/quickstart/) downloads:

```sh
$ unzip -l apod.wacz
  Length      Date    Time    Name
---------  ---------- -----   ----
  5622021  08-28-2026 21:12   archive/rec-25e2f5aba4bd-apod-20260828211227069-0.warc.gz
   476432  08-28-2026 21:12   archive/screenshots-20260828211230621.warc.gz
     1739  08-28-2026 21:12   archive/text-20260828211230632.warc.gz
    45059  08-28-2026 21:12   indexes/index.cdxj
       83  08-28-2026 21:12   pages/extraPages.jsonl
     2632  08-28-2026 21:12   pages/pages.jsonl
     3472  08-28-2026 21:12   logs/20260828211225769.log
     1662  08-28-2026 21:12   datapackage.json
      117  08-28-2026 21:12   datapackage-digest.json
```

### `archive/`: the WARCs

Note that there are three, not one, and that the split is meaningful. The big
`rec-…-apod-….warc.gz` holds the HTTP traffic: requests and responses for the page and every
subresource. `screenshots-….warc.gz` holds page screenshots, and `text-….warc.gz` holds the text the
browser extracted after rendering. Both are written as WARC `resource` records under synthetic
`urn:` URLs (`urn:view:…`, `urn:text:…`, `urn:pageinfo:…`) rather than real ones. That is a Browsertrix
convention rather than a WACZ requirement, and it shows how the format handles this generally:
anything a crawler wants to preserve becomes a WARC record, and the container doesn't need to know
what it means.

These files are `.gz`, but the compression is *inside* the WARC (gzip per record, which is how WARCs
normally live) and the ZIP entry itself is **stored**, not deflated:

```sh
$ unzip -v apod.wacz | head -4
 Length   Method    Size  Cmpr    Date    Time   CRC-32   Name
--------  ------  ------- ---- ---------- ----- --------  ----
 5622021  Stored  5622021   0% 08-28-2026 21:12 39f787ba  archive/rec-…-0.warc.gz
```

That `Stored` is important, because the WARC sits in the ZIP at a fixed offset with no ZIP-level
compression, a byte range in the WACZ maps directly onto a byte range in the WARC, and each record's
gzip member decompresses on its own. Range-request a record out of a 5 MB, or 5 GB file, and you
get that record. The spec says `archive/` members *should* be stored this way; a few tools deflate
them anyway, and when indice meets one it falls back to downloading the whole file.

### `indexes/`: the CDX index

One line per archived record, sorted, as CDXJ: a
[SURT](https://en.wikipedia.org/wiki/Sort-friendly_URI_Reordering_Transform) key, a 14-digit
timestamp, then a JSON blob.

```
com,google)/js/th/khsyravfdane6osyyx6mt_lep2fcce5kkmz6ejogm04.js 20260828211228 \
  {"url":"https://www.google.com/js/th/KhSYraVfDanE6OSyYx6mt_lEP2fCce5kkMZ6ejOGM04.js",
   "mime":"text/javascript","status":"200","digest":"2a1498ada55f0da9…","length":"28744",
   "offset":"1423243","filename":"rec-…-apod-…-0.warc.gz"}
```

The SURT (host reversed into `com,google)`, lowercased, `www.` stripped, query parameters sorted) is
what makes the file sort into a useful order: everything from one host lands together, and a lookup
becomes a binary search. `filename` + `offset` + `length` is the address of the record inside
`archive/`. Very large WACZs may also carry an `indexes/index.idx`, a sparse index *of the index*, so
even the CDX doesn't have to be read whole. And older or hand-made WACZs may name this file
`index.cdx` or `index.cdx.gz` instead; indice accepts all of these.

### `pages/`: what counts as a page

`pages/pages.jsonl` is a header line followed by one JSON object per page. Its first line declares
the format and, crucially, whether the entries carry text:

```json
{"format":"json-pages-1.0","id":"pages","title":"Seed Pages","hasText":"true"}
{"id":"d73f0fb1-774b-4611-bbaa-d1220e372577",
 "url":"https://apod.nasa.gov/apod/ap260823.html",
 "title":"APOD: 2026 August 23 – Cassini Approaches Saturn",
 "loadState":4,"ts":"2026-08-28T21:12:26.840Z","mime":"text/html","status":200,
 "seed":true,"depth":0,"favIconUrl":"https://apod.nasa.gov/favicon.ico",
 "text":"Astronomy Picture of the Day\nDiscover the cosmos!\n…"}
```

`pages/extraPages.jsonl` has the same shape and holds pages that were reached but weren't seeds. The
distinction matters to a crawler's own bookkeeping; indice reads both, since a page is a page.

This file is what lets indice tell pages from everything else. The CDX knows that a URL was
fetched with status 200 and MIME `text/html`; it has no idea whether that URL was a page a person
would read or a fragment loaded by a script.

### `datapackage.json`: the manifest

```json
{
  "resources": [
    {"name": "index.cdxj", "path": "indexes/index.cdxj", "bytes": 45059,
     "hash": "sha256:961081825ad732346ed557a32e49fc0548bdeac73fc7e5e0e419c4d2b3336466"},
    …
  ],
  "created": "2026-08-28T21:12:32Z",
  "wacz_version": "1.1.1",
  "software": "Browsertrix-Crawler 1.14.3 (with warcio.js 2.4.11)"
}
```

Every other member of the ZIP is listed here with its byte count and hash, which is what makes a
WACZ verifiable rather than merely well-organized. `software` and `created` are where a crawl page in
indice gets "made by Browsertrix-Crawler 1.14.3 on 28 August 2026". Being a Frictionless Data
Package, custom top-level properties are legal, and indice uses that when it packages WARCs itself:
an Archive-It import carries the source crawl and collection records along in an `archiveit` object,
so the provenance lives in the file indice already parses rather than in a sidecar that can get
separated from it.

`datapackage-digest.json` then hashes `datapackage.json`:

```json
{"path": "datapackage.json",
 "hash": "sha256:6a98252aefebdf2319116171e9809d631d75255bd9e8c25d1bc79ce3dfd95fb2"}
```

which closes the chain: one hash covers the manifest, the manifest covers everything else. This is
also the hook for the [WACZ signing](https://specs.webrecorder.net/wacz-auth/latest/) spec that
Scoop implements: sign this one small file and you have signed the whole archive.

### `logs/`

Optional, and unread by indice: the crawler's own log of the run. Nice to have when you're trying to
work out why a page came back empty.

### Expect variation

The layout is a spec, but WACZs in the wild span several years of it. A 2021-vintage file looks
like this:

```sh
$ unzip -l github-bitcoin-mining.wacz
    18072  pages/pages.jsonl
  1197817  archive/data.warc          # not .gz
     9043  archive/text.warc
   100655  indexes/index.cdx          # not .cdxj
      973  datapackage.json
```

and its `datapackage.json` declares `"wacz_version": "1.0.0"`, nests descriptive fields under a
`metadata` object, and hashes resources with **MD5** under a `stats` key rather than the flat
`sha256:` of 1.1.1. There's no `datapackage-digest.json`, no `extraPages.jsonl`, no `logs/`.

indice reads all of these variants, which is worth knowing so that an old WACZ not matching the
description above isn't a surprise. When something is missing, indice degrades rather than refusing:
no `pages.jsonl` means it falls back to the CDX and the response bodies, no CDX means a full scan of
the WARCs. See [How indice works](/docs/reference/how-it-works/#how-indexing-reads-a-wacz) for the
fallback ladder.

## Inspecting a WACZ yourself

A WACZ is a ZIP, so the tools you already have work:

```sh
unzip -l  yourfile.wacz                        # what's in it
unzip -v  yourfile.wacz                        # ...and whether it's Stored
unzip -p  yourfile.wacz datapackage.json | jq  # the manifest
unzip -p  yourfile.wacz pages/pages.jsonl | head -1 | jq   # does it carry text?
unzip -p  yourfile.wacz indexes/index.cdxj | wc -l         # how many records
```

indice can tell you some of this too, without unzipping: `indice collection list` and
`indice crawl list` show what's indexed, and `indice verify` re-hashes every indexed WACZ to check
it against the fixity recorded at index time. To look at a WACZ in a browser before
indexing it, drop it on [replayweb.page](https://replayweb.page/), it never leaves your machine.
