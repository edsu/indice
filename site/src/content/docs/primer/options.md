---
title: Deleting an `Option` that asserts nothing
description: Default trait methods, the null object, and a rule for when `Option` earns its place.
---

`Option<T>` is how Rust says "this might not be there", and it is one of the
language's better ideas: there is no null, so the compiler makes you handle the
absent case. The failure mode differs from null's. Instead of a crash you get
*ceremony*, code that keeps asking a question whose answer never mattered.

indice had a clear case of that. Progress reporting used to be threaded as:

```rust
progress: Option<&dyn IndexProgress>
```

and every site that reported anything asked first:

```rust
if let Some(p) = progress {
    p.phase("checksumming");
}
```

Twelve such parameters and twenty-five such conditionals. Every one of them
correct, and every one asking the same question: *is there a UI?*

## The wrong question, asked twelve times

A progress bar on screen is a fact about the caller's terminal. Indexing a WACZ
does not turn on it. The pipeline was carrying a decision that belonged to the
binary, and paying for it at every call site.

The fix is two Rust features together. First, trait methods can have bodies, and
`IndexProgress`'s are all no-ops:

```rust
fn begin(&self, _label: &str) {}
fn phase(&self, _phase: &str) {}
fn set_total(&self, _total: u64) {}
fn set_records(&self, _done: u64) {}
fn wacz_indexed(&self, _label: &str, _pages: u64) {}
fn finish(&self) {}
```

Second, a type that implements the trait by accepting all of them:

```rust
pub struct NoProgress;

impl IndexProgress for NoProgress {}
```

An empty struct, an empty impl. "Do nothing" is now a *value* rather than the
absence of one, which is the **null object** pattern. And since a caller needs
something to point at:

```rust
pub fn no_progress() -> &'static dyn IndexProgress {
    &NoProgress
}
```

`&'static` because `NoProgress` holds no data, so one of them can live for the
whole program and every caller can share it.

The parameter becomes `progress: &dyn IndexProgress`, all twelve `Option`s go,
all twenty-five conditionals go, and the one remaining `Option`, whether to draw
a bar, stops in the binary where the terminal is.

## The rule that came out of it

> An `Option` in a signature should assert something about the domain.

Applied to `Ingest`'s own fields, that rule keeps some and removes others, which
is what makes it useful rather than a slogan:

| Field | Kept? | Why |
|---|---|---|
| `resolver: Option<&dyn SourceResolver>` | **kept** | `None` means no credentials are configured, which `acquire::open` turns into a specific error. Real information. |
| `name: Option<&str>` | **kept** | `None` means "no override, use the WACZ's own title". A different behaviour, not a missing object. |
| `concurrency: Option<usize>` | **kept** | `None` means "use the per-source default", which differs for local and remote. |
| `actor: Option<&SubjectId>` | **kept** | `None` means nobody is credited, and an unattributed crawl is one only an admin may delete. The absence is load-bearing. |
| `progress` | **removed** | `None` meant "there may or may not be a UI", which says nothing about an ingest. |

Every kept `Option` has a sentence explaining what `None` *means*. The removed
one did not, and that was the tell.

## The shape to look for

Before reaching for the pattern, check that the trait is a *sink*, something you
tell things to, where doing nothing is a legitimate implementation. Progress
reporting is; a `SourceResolver` is not. Resolving a URL has no sensible no-op,
since returning nothing is a failure the caller must handle, which is why that
one keeps its `Option`.

So the test is whether a real implementation can do nothing. If one can, write it
and delete the branch. If none can, the `Option` is telling you something and
should stay.

## What to take forward

- Default method bodies let a trait ship a do-nothing implementation.
- A null object turns "absent" into an ordinary value, and `&'static` gives you
  one to point at for free.
- Keep an `Option` when you can say what `None` means in domain terms; be
  suspicious when you cannot.
- The pattern fits sinks, not functions whose result you need.
