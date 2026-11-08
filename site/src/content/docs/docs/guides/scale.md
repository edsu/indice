---
title: Scale up
description: How indice's footprint and query latency hold up from a laptop to institutional collections.
---

indice runs the same way on a laptop with a handful of WACZs and on a server holding an institution's collections: one binary, an embedded index, no Solr/Elasticsearch cluster to operate. As a corpus grows, indice keeps a **predictable on-disk footprint**, and **query latency grows sub-linearly**.

## Footprint

Only a capped prefix of each page's body is *stored* (for snippets); the full text is indexed for search but not stored, so the doc store stays bounded. On a real corpus that works out to **~1.7 KB per document**. Two commands report the footprint:

```sh
indice stats            # size by index file type, bytes/doc, projections to 1M/100M docs
indice stats --fields   # what fills the doc store, by stored field
```

At ~1.7 KB/doc the index projects to roughly **1.6 GB at 1M docs** and **~160 GB at 100M**. If that's still too much, the `stored_body_cap_kb` knob (see [Operator configuration](/docs/reference/configuration/)) trades snippet depth for a smaller store: a laptop keeps generous snippets, an institution dials it down.

## Query latency

Search stays interactive as the corpus grows because indice does the result-grouping and snippet work over a fixed candidate window, whatever the corpus size. Measured on a laptop with the `scale_bench` example (single binary, one process):

| corpus | full-text search | facet overview | collection page lookup |
|--------|-----------------:|---------------:|-----------------------:|
| 100k docs | ~20 ms | ~2.5 ms | ~1.5 ms |
| 1M docs   | ~40 ms | ~16 ms  | ~1.5 ms |

10× the data costs roughly 2× the search latency; everything stays well under 100 ms at 1M docs. Indexed lookups (opening a collection, resolving a URL for replay) are corpus-size-independent. You can reproduce with:

```sh
cargo run --release --example scale_bench -- 1000000   # docs, [iters], [segments]
```

## Memory while indexing

Indexing reads a WACZ a page at a time, so the memory it needs does not grow with the crawl. What it does depend on is the largest page it reads, and PDFs are the hard case: how much memory reading one takes cannot be told from its size. One 8 MB report took 2.2 GB.

So each PDF is read in a process of its own, one at a time, and that process is stopped past `index.pdf_memory_mb` (default 1024 MiB, Linux only) or `index.pdf_timeout_secs` (default 60). A PDF that is stopped is still found by its file name and still replays. To size a server, allow for indice itself plus one PDF's limit, and lower `pdf_memory_mb` on a small machine. See [Operator configuration](/docs/reference/configuration/#reading-a-wacz) for these and the other indexing limits.

## Index health

A search fans out across every segment, so a fragmented index is a slow one. indice compacts the index on its own: `index` and the management-mode import **auto-compact** when they detect fragmentation, `serve` warns at startup if the index it opens is fragmented, and `indice optimize` merges segments on demand, which reclaims disk from deleted crawls *and* sweeps orphaned segment files left behind by an interrupted (Ctrl-C'd) run. The next `index` run cleans up after an interrupted ingest.
