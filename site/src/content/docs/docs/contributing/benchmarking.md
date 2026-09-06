---
title: Benchmarking
description: Measure ingest performance and query latency at scale.
---

`scripts/bench-ingest.sh` measures indexing a WACZ — wall time, peak RSS, a per-phase timing breakdown, and the resulting index footprint:

```sh
scripts/bench-ingest.sh path/to/crawl.wacz            # benchmark the current build
scripts/bench-ingest.sh path/to/crawl.wacz <git-ref>  # compare a baseline ref vs. current
```

It builds `--release`, indexes the WACZ into a throwaway home under `/usr/bin/time`, and reports the phases the indexer times itself:

- **read+extract** — fetch each record and extract text (HTML/PDF)
- **index** — tokenize and add documents to the writer buffer
- **checksum** — whole-file fixity hash
- **commit** — flush the Tantivy segment to disk

The footprint comes from `indice stats` (bytes by index file type + bytes-per-doc, with projections). Passing a git ref builds that revision in a temporary worktree and prints a **before/after** — handy for confirming a change's effect on real data. Peak RSS + wall time use macOS's `/usr/bin/time -l`; on GNU/Linux use `/usr/bin/time -v` and adjust the field names.

For **query** latency at scale (rather than ingest), the `scale_bench` example builds a synthetic index of N docs and times the hot query paths (faceted search, the facet overview, collection page lookups), reporting p50/p95 — see [Scale up](/docs/guides/scale/):

```sh
cargo run --release --example scale_bench -- 1000000    # N docs, [iters], [target segments]
```
