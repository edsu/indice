---
title: Modules, privacy, and the shape of the crate
description: How the code is laid out, and the privacy rule that decides where things go.
---

indice is two crates in one workspace:

- **`indice-lib`** — everything that does the work: indexing, search, replay,
  the web server, the import clients.
- **`indice-bin`** — the `indice` command. Argument parsing, the progress bar,
  and the credentials the library asks for through traits.

The split is not ceremony. It is what forces the library to stay usable without a
terminal: anything that wants to print has to go through `index::IndexProgress`,
and anything that needs a secret has to go through a trait like
`index::SourceResolver`. Both are described in [Traits](/primer/traits/). If the
library could `println!`, that discipline would quietly disappear.

## Modules are the file tree

A `mod` declaration pulls a file in, and the paths follow the directories.
`crates/indice-lib/src/lib.rs` is a list of them:

```rust
pub mod annotations;
pub mod archiveit;
pub mod collections;
// …
```

A module with children is a directory with a `mod.rs`. `index` is the big one:

```
index/
  mod.rs        the public entry points and shared types
  ingest/       bringing a WACZ in: acquire → pages → record
  lock.rs       the two write locks
  manifest.rs   the manifest's critical section
  reindex.rs    rebuild from recorded sources
  delete.rs     removing crawls and collections
```

## Privacy is per-module, and includes descendants

Everything is private by default. `pub` opens an item up, but the interesting
part is the middle ground, because Rust's rule is more generous than most people
expect: **a private item is visible to its own module and to that module's
descendants.**

That is not a detail; it shaped the ingest pipeline. `index::ingest::Ingest` keeps
all of its fields private:

```rust
pub struct Ingest<'a> {
    home: &'a Path,
    download: bool,
    // …
}
```

and yet `ingest::acquire`, `ingest::pages` and `ingest::record` read them
directly, because they are *inside* `ingest`. No getters, no ceremony. Meanwhile
`index::reindex` — a sibling of `ingest`, not a child — cannot, and goes through
`home_dir()` and `progress_sink()`. The module tree is doing access control, and
laying the phases out as children of `ingest` is what makes that work.

The graduated forms you will meet, narrowest first:

| Form | Visible to |
|---|---|
| *(nothing)* | this module and its descendants |
| `pub(super)` | the parent module too |
| `pub(in crate::index)` | everything under `index` |
| `pub(crate)` | the whole crate, but not outside it |
| `pub` | the world, part of the library's API |

Real examples of each: `WaczAccess` is `pub(super)`; `record::Indexed` is
`pub(in crate::index)`, widened from `pub(super)` exactly when `reindex` needed to
hold one; `index::lock` is `pub(crate)`; `collections::Manifest` is `pub`.

Widening one of these is a real decision. `pub` means you now maintain it for
outside callers.

## Re-exports flatten the paths

`index/mod.rs` ends with a block of `pub use`:

```rust
pub use delete::*;
pub use ingest::*;
```

So callers write `index::index_path` even though it lives in `index::ingest`. The
file layout can then change without moving the API — which is exactly what
happened when a single `index.rs` became the directory above, and callers outside
`index` did not notice.

## What to take forward

- The lib/bin split is what keeps the library free of a terminal.
- Private means "this module and everything under it", which makes a parent
  module a natural privacy boundary.
- Choose the narrowest visibility that works, and widen deliberately.
- `pub use` lets the file layout move without breaking callers.
