---
title: REST API
description: The read-only JSON search API, the health check, and the machine-readable endpoints indice serves for replay.
---

`indice serve` exposes a small HTTP API alongside the human-facing pages. The base URL is wherever you serve (default `http://127.0.0.1:8080`); behind a reverse proxy everything sits under whatever path you mount it at.

:::note
indice is alpha software, so these endpoints may still change. The read-only endpoints below are the stable, public surface; the management write API (see the end) is an internal detail of the workroom UI.
:::

## `GET /api/search`

Full-text search as JSON, using the same engine behind the [search page](/docs/guides/searching/).

**Query parameters**

| Param | Required | Description |
|-------|----------|-------------|
| `q` | yes | The query. Supports the full [search syntax](/docs/guides/searching/), including field filters like `site:example.com`, `year:2021`, `type:pdf`, `lang:en`, and `collection:<slug>`, so facets are expressed inside `q`. Capped at **2048 bytes**; a longer one gets `400` naming the limit. |
| `limit` | no | Max results to return. Default `20`, capped at `200`. |

```sh
curl 'http://127.0.0.1:8080/api/search?q=climate+site:example.com&limit=5'
```

**Response** (illustrative, abbreviated):

```json
{
  "total": 264,
  "capped": false,
  "results": [
    {
      "doc_type": "html",
      "url": "https://example.com/report",
      "domain": "example.com",
      "timestamp": "20250802185600",
      "title": "Climate report: annual summary",
      "crawl_id": "4eefdff3",
      "crawl_name": "example crawl",
      "collection": "example-collection",
      "snippet": "…an annual <mark>climate</mark> report…",
      "capture_count": 3,
      "status": 200
    }
  ],
  "facets": [
    { "field": "collection", "label": "Collection",
      "buckets": [ { "value": "example-collection", "count": 220 } ] },
    { "field": "year", "label": "Year",
      "buckets": [ { "value": "2025", "count": 129 } ] }
  ]
}
```

**Fields**

- `total`: number of matching documents; `capped` is `true` when that count is approximate (very large result sets).
- `results[]`: up to `limit` hits, each with: `doc_type` (media bucket, e.g. `html`, `pdf`, or `annotation` for a [note](/docs/guides/annotations/)), `url`, `domain`, `timestamp` (the capture time), `title`, `author` (set for annotation hits), `crawl_id` (8-char id) and `crawl_name`, `collection` (slug), `snippet` (a hit-highlighted excerpt, with `<mark>` around matches), `capture_count` (how many times this URL was captured, grouped into one result), and `status` (the archived HTTP status).
- `facets[]`: facet groups, each `{ field, label, buckets: [{ value, count }] }`, matching the sidebar on the search page.

## `GET /api/annotations`

The public read side of [annotations](/docs/guides/annotations/): the notes on one capture, or every note in a collection.

**Query parameters**

| Param | Required | Description |
|-------|----------|-------------|
| `collection` | yes | The collection slug. |
| `url` | no | A capture URL. Provide with `ts` to get the notes on that one capture; omit both for every note in the collection. |
| `ts` | no | The 14-digit capture timestamp. Must be given together with `url`. |

```sh
curl 'http://127.0.0.1:8080/api/annotations?collection=example-collection'
```

**Response** (abbreviated): `{ "can_annotate": false, "annotations": [ … ] }`, where each note carries its `id`, `author` (a [display name](/docs/guides/annotations/#authorship-and-privacy), never the login identity, since this endpoint is public), `created`/`modified`, the target `url` + `timestamp`, the `selector` (the quoted passage, for a passage note), and the note body as both `note_md` (Markdown) and `note_html` (sanitized). `can_annotate` reflects whether the current request is allowed to write.

Each note also carries `href`, the path of [its own page](#get-collectionidannotationskey).

Creating, editing, and deleting notes are management-gated write endpoints (`POST /api/annotations`, `POST /api/annotations/{id}`, `POST /api/annotations/{id}/delete`). See the [management write API](#management-write-api) below.

## `GET /collection/{id}/annotations/{key}`

One [note](/docs/guides/annotations/), at the URL that identifies it. `key` is the hex after `urn:indice:annotation:` in its `id`. Public, like the notes themselves.

With an `Accept` header that ranks `application/ld+json` or `application/json` above HTML, it answers with the note as a [W3C Web Annotation](https://www.w3.org/TR/annotation-model/), as `application/ld+json; profile="http://www.w3.org/ns/anno.jsonld"`. A browser gets a page showing the note, with the same JSON-LD in a `<script type="application/ld+json">`.

The JSON-LD is also at `/collection/{id}/annotations/{key}.jsonld`, whatever the request asks for, so it works as a plain link. The note's page links to it with `rel="alternate"`, in the page and in a `Link` header. Its `id` is still the URL without `.jsonld`.

```sh
curl -H 'Accept: application/ld+json' \
  'http://127.0.0.1:8080/collection/example-collection/annotations/3f9c0d2e8b7a41c6a5d0e9f8b7c6a5d4'
```

**Response** (abbreviated):

```json
{
  "@context": "http://www.w3.org/ns/anno.jsonld",
  "id": "http://127.0.0.1:8080/collection/example-collection/annotations/3f9c0d2e8b7a41c6a5d0e9f8b7c6a5d4",
  "type": "Annotation",
  "created": "2026-11-05T15:04:05Z",
  "creator": { "type": "Person", "name": "alice" },
  "body": { "type": "TextualBody", "format": "text/markdown", "value": "Worth reading." },
  "target": {
    "source": "https://example.org/",
    "selector": { "type": "TextQuoteSelector", "exact": "Example Domain" },
    "state": {
      "type": "TimeState",
      "sourceDate": "2026-01-01T00:00:00Z",
      "cached": "http://127.0.0.1:8080/collection/example-collection/20260101000000/https://example.org/"
    }
  }
}
```

`id` is built from the site's URL (`--site-url`), or on a workstation from `http://localhost:` and its port. On a server the JSON-LD is sent with `Access-Control-Allow-Origin: *`, so annotation tools on other sites can read it; a workstation sends no such header, so its notes stay private to the machine. `creator` is the author's display name only, never the login identity. A whole-page note has no `selector`. `state` says when the page was captured and where the archived copy replays. An unknown note, or a key that isn't one, is a 404. A note whose id indice didn't create (in a hand-edited `annotations.jsonl`, say) has no page of its own, and `/api/annotations` gives it no `href`.

## `GET /health`

A liveness check: returns `200 OK` with the body `ok`. Useful for container/orchestrator health probes.

## Timeouts

Routes that should answer quickly carry a 30-second budget and return `503` if they exceed it. The budget is a backstop: these routes answer in milliseconds, so a 503 here means something is wrong on the server.

Four kinds of request have no budget, because they have no sensible time bound: `GET /files/{id}` streams whole archives, the SSE progress stream follows a job that may run for hours, uploads are as slow as the link, and the Browsertrix and Archive-It browse endpoints wait on someone else's service.

## Rate limits

A server limits each client. A workstation doesn't.

- **Most routes** allow a burst of 100 requests per client, then 20 a second. Past that the answer is `429` with `Retry-After`. Across all clients, at most 256 of these requests are in flight at once, and beyond that the answer is an immediate `503` with `Retry-After`.
- **`GET /files/{id}`** isn't rate-limited, because one replay makes hundreds of ranged reads. Instead a client may hold 64 streams open at once, and 512 may be open in total. A stream stays open until its body has been read or dropped.
- **`/health`** is never limited.

A client is its IP address, and for IPv6 its /64. Behind a reverse proxy on the same machine or network, indice uses the rightmost `X-Forwarded-For` entry that isn't itself internal. The shipped Caddy stack sets that header. Curators and admins are exempt from the per-client limits, so a script that reads hard needs a roster entry.

The operator sees a refusal in the server log twice: once when it starts, naming the address and the limit, and once a minute later with how many requests were refused. indice does not log each refused request, so a flood of requests does not become a flood of log lines.

## Machine-readable replay endpoints

These serve ReplayWeb.page's in-browser player, and tooling can use them too. They answer on both the site and the replay origin; the player reads them from the replay origin:

- **`GET /files/{id}`**: the raw WACZ for a crawl, honoring HTTP `Range` requests (partial fetches). This is how replay reads archived resources without downloading the whole file. On a server it is sent with `Access-Control-Allow-Origin: *`, so another site's ReplayWeb.page can replay a public archive; a workstation sends no such header.
- **`GET /collection/{id}/replay.json`**: a multi-WACZ replay manifest for a collection (every member crawl), handed to wabac.js so cross-crawl links resolve.
- **`GET /collection/{id}/pages`**: the collection's page list and URL→capture resolution, scoped to the collection.
- **`GET /crawl/{id}/replay.json`** and **`GET /crawl/{id}/pages`**: the same for one crawl that Browsertrix wrote into several WACZs, listing every file of the crawl, so a page replays with the stylesheets and scripts stored in the crawl's other files.

See [How indice works](/docs/reference/how-it-works/) for how these fit together.

## Management write API

indice also carries write endpoints under `/api/` (add archives, create or edit collections, delete crawls, Browsertrix and Archive-It import), plus SSE progress streams. The workroom UI calls them. They are always mounted; who may call them is decided by [`Access`](/docs/guides/manage/), which is loopback trust on a workstation and a forward-auth proxy plus `users.yaml` on a server. An anonymous caller gets a `403`. Treat them as an implementation detail of that UI rather than a stable public API.
