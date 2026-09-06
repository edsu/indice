---
title: Scale up
description: How indice's footprint and query latency hold up from a laptop to institutional collections.
---

indice is built to run the same way on a laptop with a handful of WACZs and on a server holding an institution's collections — one binary, an embedded index, no Solr/Elasticsearch cluster to operate. Two things make that hold up as a corpus grows: a **frugal, predictable on-disk footprint** and **query latency that grows sub-linearly**.

## Footprint

Only a capped prefix of each page's body is *stored* (for snippets); the full text is indexed for search but not stored, so the doc store stays bounded. On a real corpus that works out to **~1.7 KB per document**. Two commands make the footprint legible:

```sh
indice stats            # size by index file type, bytes/doc, projections to 1M/100M docs
indice stats --fields   # what fills the doc store, by stored field
```

At ~1.7 KB/doc the index projects to roughly **1.6 GB at 1M docs** and **~160 GB at 100M**. If that's still too much, the `stored_body_cap_kb` knob (see [Operator configuration](/indice/docs/reference/configuration/)) trades snippet depth for a smaller store — a laptop keeps generous snippets, an institution dials it down.

## Query latency

Search stays interactive as the corpus grows because the result-grouping and snippet work is bounded (a fixed candidate window), not proportional to the corpus. Measured on a laptop with the `scale_bench` example (single binary, one process):

| corpus | full-text search | facet overview | collection page lookup |
|--------|-----------------:|---------------:|-----------------------:|
| 100k docs | ~20 ms | ~2.5 ms | ~1.5 ms |
| 1M docs   | ~40 ms | ~16 ms  | ~1.5 ms |

10× the data costs roughly 2× the search latency — everything stays well under 100 ms at 1M docs. Indexed lookups (opening a collection, resolving a URL for replay) are corpus-size-independent. Reproduce with:

```sh
cargo run --release --example scale_bench -- 1000000   # docs, [iters], [segments]
```

## Index health

A search fans out across every segment, so a fragmented index is a slow one. indice keeps the index compact automatically: `index` and the management-mode import **auto-compact** when they detect fragmentation, `serve` warns at startup if the index it opens is fragmented, and `indice optimize` merges segments on demand — reclaiming disk from deleted crawls *and* sweeping orphaned segment files left behind by an interrupted (Ctrl-C'd) run, which otherwise linger. So an interrupted ingest self-heals on the next `index`.
