---
title: Traits, and the two ways to use one
description: Shared behaviour, and the `dyn` versus generic decision you make every time.
---

A trait is a set of methods a type can implement — an interface. indice's are
small:

```rust
pub trait SourceResolver: Send + Sync {
    fn resolve(&self, source: &Source) -> Result<String>;
}
```

That one exists because a Browsertrix download URL expires. The library knows it
needs a fresh URL; only the binary knows how to ask Browsertrix for one. The trait
is the seam between them, and it is why `indice-lib` has no idea Browsertrix
exists.

`Send + Sync` after the colon are **supertraits**: any implementor must also be
safe to move between threads and share between them. That is not decoration — it
is what lets a resolver be used from the worker threads in
the concurrency chapter, checked at compile time.

## The decision: `dyn` or generic

Every time you use a trait you pick one of two forms, and the difference is
*when* the type is known.

**Generic** — the type is known at compile time, and the compiler stamps out a
copy of the function for each one:

```rust
pub fn import_crawls<T: Transport>(client: &Client<T>, /* … */) -> Result<ImportOutcome>
```

`archiveit::Transport` is generic because there are exactly two implementors known
at compile time: the real HTTP client, and the fake one the tests use. Generics
cost nothing at runtime; they cost compile time and binary size.

**`dyn`** — the type is known only at runtime, reached through a pointer and a
vtable:

```rust
progress: &'a dyn IndexProgress,
```

`index::IndexProgress` is `dyn` because `index::Ingest` stores one and the pipeline
threads it through every phase. Making that generic would put a type parameter on
`Ingest` and on everything holding an `Ingest`, to save one pointer hop per
progress update — which is nothing next to the work being reported on.

The rule of thumb this codebase follows: **generic when the set of types is
closed and known; `dyn` when a value has to be stored and passed around.** If a
type parameter starts spreading through unrelated structs, that is the signal to
switch.

## Default methods, which do more than save typing

A trait method can have a body, and then implementors may skip it. `IndexProgress`
uses this to delete a decision:

```rust
pub trait IndexProgress: Sync {
    fn begin(&self, label: &str) {}
    fn phase(&self, phase: &str) {}
    // …all six, all no-ops by default
}
```

Every method does nothing unless overridden, so `index::NoProgress` is an empty
struct implementing an empty block. That is the **null object pattern**, and the
reason it is here rather than `Option<&dyn IndexProgress>` gets a chapter of its
own: a later chapter, on deleting an `Option` that asserts nothing.

## Deriving

Most trait impls in indice are not written out:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct CollectionId(String);
```

`derive` generates the obvious implementation. `Debug` gives `{:?}` formatting,
`Clone` gives `.clone()`, `Serialize` gives JSON.

Worth noticing what is *not* derived. `CollectionId` takes `Serialize` from the
macro but writes `Deserialize` by hand, so that a value arriving from JSON goes
through `CollectionId::parse` like every other one. A derived `Deserialize` would
have built the type straight from any string in the file, which is the back door
the type exists to close. Writing an impl yourself, rather than deriving it, is
sometimes the whole point — see [Making illegal states
unrepresentable](/primer/illegal-states/).

## What to take forward

- A trait is an interface; supertraits like `Send + Sync` are requirements on
  implementors.
- Generic when the types are known and closed; `dyn` when the value gets stored.
- Default method bodies let "do nothing" be a real implementation.
- A missing `derive` can be as deliberate as a present one.
