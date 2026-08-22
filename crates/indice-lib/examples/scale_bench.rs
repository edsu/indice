//! Query-latency-at-scale benchmark for the size/scale model (rustyweb-kx53's
//! sibling `qw5.5`). Builds a synthetic index of N pages, compacts it, then times
//! the hot query paths — faceted search, the global facet overview, and the
//! collection page listing — reporting p50/p95 so we can see how latency grows
//! with the corpus and spot any cliffs.
//!
//! Synthetic but faithful: it drives the *real* `SearchIndex::index_page` schema
//! and the real query methods; only the page text is generated. Bodies are drawn
//! from a small vocabulary with controlled selectivity so the queries are
//! meaningful (a term that matches ~everything vs. a rare one).
//!
//! Usage:
//!   cargo run --release --example scale_bench -- [N_DOCS] [ITERS] [TARGET_SEGMENTS]
//!     N_DOCS          documents to index (default 100000)
//!     ITERS           timed repetitions per query (default 30)
//!     TARGET_SEGMENTS compact to this many segments before timing (default 8)

use std::time::Instant;

use indice_lib::search::{Page, SearchIndex};

const VOCAB: &[&str] = &[
    "archive",
    "web",
    "page",
    "crawl",
    "history",
    "record",
    "river",
    "mountain",
    "forest",
    "ocean",
    "climate",
    "policy",
    "data",
    "network",
    "server",
    "index",
    "search",
    "query",
    "document",
    "library",
    "museum",
    "culture",
    "language",
    "memory",
    "signal",
    "carbon",
    "energy",
    "water",
    "city",
    "region",
    "report",
    "study",
    "survey",
    "model",
    "system",
    "future",
    "digital",
    "public",
    "open",
    "access",
    "collection",
    "curator",
    "finding",
    "aid",
    "capture",
    "replay",
    "snapshot",
    "timeline",
    "corpus",
    "footprint",
];

// A tiny deterministic PRNG (xorshift64) so runs are reproducible without a dep.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn pick<'a>(&mut self, xs: &'a [&'a str]) -> &'a str {
        xs[(self.next() as usize) % xs.len()]
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100_000);
    let iters: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);
    let target_segments: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);

    let tmp = tempfile::TempDir::new()?;
    let dir = tmp.path().join("index").join("full_text");
    std::fs::create_dir_all(&dir)?;
    let home = tmp.path();

    println!("Building a synthetic index of {n} docs…");
    let build_start = Instant::now();
    {
        let mut idx = SearchIndex::open(&dir)?;
        for i in 0..n {
            let mut rng = Rng(0x9E3779B97F4A7C15 ^ (i as u64).wrapping_mul(2654435761));
            // ~120 vocab words + a fixed common phrase → "the archive" and
            // "archive" match ~all (worst case), vocab words vary in frequency,
            // and a rare needle lands in 1/1000 docs.
            let mut body = String::from("the archive of the web. ");
            for _ in 0..120 {
                body.push_str(rng.pick(VOCAB));
                body.push(' ');
            }
            if i % 1000 == 0 {
                body.push_str("zzqneedle ");
            }
            let site = i % 200; // 200 sites
            let coll = i % 20; // 20 collections
            let year = 2015 + (i % 10) as u64;
            let month = 1 + (i % 12) as u64;
            let url = format!("https://site{site}.example.com/path/{i}");
            let title = format!("Page {i} about {}", rng.pick(VOCAB));
            let ts = format!("{year}{month:02}01120000");
            let collection = format!("c{coll}");
            let crawl_id = format!("crawl{}", i % 500); // 500 crawls
            idx.index_page(&Page {
                url: &url,
                timestamp: &ts,
                title: &title,
                body: &body,
                description: "",
                headings: "",
                keywords: "",
                author: "",
                media_type: if i % 7 == 0 { "pdf" } else { "html" },
                lang: "en",
                status: Some(if i % 50 == 0 { 404 } else { 200 }),
                modified_year: Some(year),
                crawl_id: &crawl_id,
                crawl_name: "Bench crawl",
                collection: &collection,
            })?;
            if i > 0 && i % 50_000 == 0 {
                idx.commit()?;
                println!("  … {i} indexed");
            }
        }
        idx.commit()?;
        println!("  compacting to {target_segments} segment(s)…");
        idx.optimize(target_segments, None)?;
    }
    let build = build_start.elapsed();

    let stats = indice_lib::index::index_stats(home)?;
    println!(
        "\nBuilt {} docs in {:.1}s · {} on disk ({}/doc) · {} segment(s)\n",
        stats.docs,
        build.as_secs_f64(),
        human(stats.total_bytes),
        human(stats.bytes_per_doc().round() as u64),
        SearchIndex::open_read_only(&dir)?.segment_count()?,
    );

    let idx = SearchIndex::open_read_only(&dir)?;

    println!("Query latency ({iters} iters each):");
    // Faceted search (the main hot path: query + facets + URL grouping + snippets).
    for (label, q) in [
        ("search: common term (all docs)", "archive"),
        ("search: mid-freq term", "river"),
        ("search: two-word AND", "river mountain"),
        ("search: phrase (all docs)", "\"the archive\""),
        ("search: rare term", "zzqneedle"),
        ("search: filtered (collection:)", "collection:c3 river"),
    ] {
        bench(label, iters, || {
            Ok(idx.search_faceted(q, 25, 0)?.total_hits)
        });
    }
    // The homepage facet overview (global aggregation, no result fetch).
    bench("facet_overview (global)", iters, || {
        Ok(idx.facet_overview()?.len())
    });
    // Collection page listing + exact-URL resolution (the replay sidebar path).
    bench("collection_pages: listing (25)", iters, || {
        Ok(idx.collection_pages("c3", None, None, 0, 25)?.1.len())
    });
    bench("collection_pages: exact url", iters, || {
        Ok(idx
            .collection_pages("c3", Some("https://site3.example.com/path/3"), None, 0, 25)?
            .1
            .len())
    });

    Ok(())
}

/// Time `f` `iters` times (after one warm-up), reporting p50 / p95 / max in ms.
fn bench<F: FnMut() -> anyhow::Result<usize>>(label: &str, iters: usize, mut f: F) {
    let hits = f().unwrap_or(0); // warm up: primes the reader / caches
    let mut ms: Vec<f64> = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        let _ = f().unwrap_or(0);
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| ms[((ms.len() as f64 * q) as usize).min(ms.len() - 1)];
    println!(
        "  {:<34} p50 {:>8.2}ms  p95 {:>8.2}ms  max {:>8.2}ms   (~{} hits)",
        label,
        p(0.50),
        p(0.95),
        p(1.0),
        hits
    );
}

fn human(bytes: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut b = bytes as f64;
    let mut i = 0;
    while b >= 1024.0 && i < U.len() - 1 {
        b /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{b:.1} {}", U[i])
    }
}
