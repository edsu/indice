---
title: "Concurrency: threads, async, and the boundary between them"
description: Three kinds of concurrency in one codebase, and the rule that keeps them apart.
---

indice runs three different concurrency models at once, which sounds like a mess.
Each one does a job the others are bad at:

| Where | What | Why that one |
|---|---|---|
| Indexing a WACZ | **rayon**, a thread pool | CPU-bound work over a list: extract text from every record |
| The web server | **tokio**, async | Thousands of mostly-idle connections |
| Between them | **`spawn_blocking`** | Indexing must not run on an async thread |

The reason it holds together is that Rust makes the boundary a type error rather
than a convention.

## `Send` and `Sync`, in one paragraph

Two marker traits the compiler derives for you:

- **`Send`**: safe to *move* to another thread.
- **`Sync`**: safe to *share* by reference between threads (`&T` is `Send`).

Almost everything is both. The interesting types are the ones that are not:
`Rc` is neither (its count is not atomic), `MutexGuard` is not `Send`, and
indice's own `IndexLock` is neither by design, as
[Guards, `Drop` and `!Send`](/primer/guards/) explains. You rarely write these;
you notice them when the compiler refuses something, and the refusal is usually
correct.

They show up as requirements. `SourceResolver` demands both:

```rust
pub trait SourceResolver: Send + Sync {
```

because a resolver is shared across the pool below. `IndexProgress: Sync` needs
only the sharing half, since workers hold `&dyn IndexProgress` and never move it.
Writing the supertrait is how you say "implementors must be usable from the
threads I am about to spawn", checked at compile time rather than discovered at
runtime.

## rayon: parallelism as a change of iterator

```rust
let per_warc: Vec<(Vec<RawRecord>, Option<Warcinfo>)> = warc_paths
    .par_iter()
    .map(|entry_name| { … })
```

`par_iter()` instead of `iter()`, and the work spreads across a thread pool.
That is most of it. rayon's data-parallel API is a drop-in for the iterator you
already wrote, and it will not compile unless the closure and its captures are
`Send`.

Where the work needs a bounded pool rather than the global one, indice builds
its own:

```rust
let pool = rayon::ThreadPoolBuilder::new()
    .num_threads(concurrency)
    .build()?;
let mut out: Vec<RawRecord> = pool.install(|| { … par_iter() … });
```

The reason is politeness rather than performance: each task fetches a byte range
over HTTP, and the pool size is how many requests a remote host sees at once.
Inside `pool.install`, the `par_iter` uses that pool instead of the global one.

Shared mutable state in there goes behind the usual things: an `AtomicU64` for a
progress counter, a `Mutex<SearchIndex>` for the writer.

The `Mutex` deserves a precise explanation, because the obvious guess about it is
wrong. `SearchIndex` *is* `Sync`, so sharing a `&SearchIndex` across threads
would be fine. The problem is that writing a document takes `&mut self`, and
several workers cannot each hold an exclusive borrow at once. `Mutex` turns "one
exclusive borrow, statically" into "one at a time, checked at runtime". That is
interior mutability, the standard answer when shared access has to be mutable.

## tokio: async, and what it is for

An async function returns a future, a value describing work, which does nothing
until polled. `.await` polls it and yields control if it is not ready, so one OS
thread can drive many tasks that are mostly waiting on sockets. That is the case
async is *for*: lots of concurrent waiting.

It is not for CPU work. An `.await`-free stretch of computation cannot yield, so
it blocks the executor thread, and a handful of those stall every other request
on that thread.

## The boundary is a hard rule

Indexing is that kind of work: read a WACZ, extract text, commit to
Tantivy. Seconds to hours, no awaits, all CPU and file I/O. So the server never
does it on an async thread:

```rust
tokio::task::spawn_blocking(move || {
    // ingest, delete, annotate: blocking work, on a pool sized for it
})
```

`spawn_blocking` moves the closure to a separate pool meant for it, leaving the
async threads free. The closure must be `Send + 'static`, which is
the compiler making sure you are not smuggling a borrow across.

This is also where `!Send` pays off. Because `IndexLock` cannot cross an await
point, a blocking closure is the only place you can hold one, which is where it
belongs. An attempt to hold a cross-process file lock across an HTTP await does
not compile.

## Read-mostly state: `RwLock<Arc<T>>`

The server holds its searcher like this:

```rust
search: RwLock<Arc<SearchIndex>>,
```

Two wrappers, doing two jobs. `RwLock` allows many concurrent readers or one
writer. `Arc` is an atomically reference-counted pointer, so a value can have
several owners across threads and is dropped when the last one goes.

Together they give a cheap swap. A search handler takes the read lock only long
enough to **clone the `Arc`**, which bumps a counter, then releases it and
queries the snapshot it now owns. A reload replaces the whole thing:

```rust
fn reload_searcher(&self) -> Result<()> {
    let fresh = SearchIndex::open_read_only(self.index_dir.join("full_text").as_path())?;
    *self.search.write().unwrap() = Arc::new(fresh);
    Ok(())
}
```

The write lock is held for one pointer assignment. Requests already querying the
old index keep their `Arc` alive and finish against it; the old index is dropped
when the last of them lets go. So an ingest can hot-reload the searcher without
restarting the server and without pausing a single in-flight search.

The pattern generalises: for state that is read constantly and replaced
occasionally, put the `Arc` *inside* the lock and clone it out, rather than
holding the lock for the whole read.

## What to take forward

- `Send` is "can move threads", `Sync` is "can share by reference"; supertraits
  are how you demand them.
- rayon for CPU-bound work over a collection; build a bounded pool when the
  concurrency is a politeness limit.
- async for waiting, never for computing; `spawn_blocking` is the doorway.
- `RwLock<Arc<T>>` plus clone-and-release keeps readers off the lock.
- When the compiler refuses to send something between threads, it is usually
  describing a real hazard rather than an inconvenience.
