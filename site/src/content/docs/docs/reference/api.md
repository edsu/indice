---
title: REST API
description: The read-only JSON search API, the health check, and the machine-readable endpoints indice serves for replay.
---

`indice serve` exposes a small HTTP API alongside the human-facing pages. The base URL is wherever you serve (default `http://127.0.0.1:8080`); behind a reverse proxy everything sits under whatever path you mount it at.

:::note
indice is alpha software — these shapes may still change. The read-only endpoints below are the stable, public surface; the management write API (see the end) is an internal detail of the workroom UI.
:::

## `GET /api/search`

Full-text search as JSON — the same engine behind the [search page](/indice/docs/guides/searching/).

**Query parameters**

| Param | Required | Description |
|-------|----------|-------------|
| `q` | yes | The query. Supports the full [search syntax](/indice/docs/guides/searching/) — including field filters like `site:example.com`, `year:2021`, `type:pdf`, `lang:en`, and `collection:<slug>`, so facets are expressed inside `q`. |
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
      "title": "Climate report — annual summary",
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

- `total` — number of matching documents; `capped` is `true` when that count is approximate (very large result sets).
- `results[]` — up to `limit` hits, each with: `doc_type` (media bucket, e.g. `html`, `pdf`), `url`, `domain`, `timestamp` (the capture time), `title`, `crawl_id` (8-char id) and `crawl_name`, `collection` (slug), `snippet` (a hit-highlighted excerpt, with `<mark>` around matches), `capture_count` (how many times this URL was captured, grouped into one result), and `status` (the archived HTTP status).
- `facets[]` — facet groups, each `{ field, label, buckets: [{ value, count }] }`, matching the sidebar on the search page.

## `GET /health`

A liveness check: returns `200 OK` with the body `ok`. Useful for container/orchestrator health probes.

## Machine-readable replay endpoints

These serve ReplayWeb.page and are handy for tooling, though their primary consumer is the in-browser player:

- **`GET /files/{id}`** — the raw WACZ for a crawl, honoring HTTP `Range` requests (partial fetches). This is how replay reads archived resources without downloading the whole file.
- **`GET /collection/{id}/replay.json`** — a multi-WACZ replay manifest for a collection (every member crawl), handed to wabac.js so cross-crawl links resolve.
- **`GET /collection/{id}/pages`** — the collection's page list and URL→capture resolution, scoped to the collection.

See [How indice works](/indice/docs/reference/how-it-works/) for how these fit together.

## Management write API

When you run [`serve --manage`](/indice/docs/guides/manage/), indice additionally mounts write endpoints under `/api/` (add archives, create/edit collections, delete crawls, and Browsertrix/Archive-It import), plus SSE progress streams. These **power the workroom UI** and are gated by the same auth as the rest of management (loopback trust, or a forward-auth proxy). They are an implementation detail of that UI rather than a stable public API, and are never mounted by the default read-only `serve`.
