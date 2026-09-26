---
title: Stopping a signature from growing
description: Builders, grouping types, and why a `Copy` builder needs `#[must_use]`.
---

Here is a function signature that went wrong slowly:

```rust
pub fn index_location(
    location: &str,
    home: &Path,
    name: Option<&str>,
    collection: &str,
    download: bool,
    force: bool,
    concurrency: Option<usize>,
    progress: Option<&dyn IndexProgress>,
) -> Result<()>
```

Eight parameters, four of them optional or boolean, and two `bool`s adjacent so
that swapping them compiles and silently does the wrong thing. Nobody wrote
that on purpose. It arrived one parameter at a time, each addition obviously
justified.

Rust's linter noticed. `clippy::too_many_arguments` fires above seven, and it
fired here — and was silenced, with `#[allow(clippy::too_many_arguments)]`, eight
separate times across the ingest module. One of those allows was suppressing
nothing at all: the function under it had exactly seven parameters, so the lint
was never going to fire. A dead suppression nobody noticed, which is the
clearest evidence they had stopped being read.

That is the part worth taking seriously. The lint was not a style complaint, it
was a design report, and silencing it cost two real bugs. Crawl custody could
not be threaded through the pipeline without an eleventh parameter, so it was
written out-of-band afterwards instead — which required diffing the manifest
before and after, which produced a lost-update race and a fail-open that handed
one curator ownership of every unattributed crawl.

## The builder

```rust
pub struct Ingest<'a> {
    home: &'a Path,
    name: Option<&'a str>,
    download: bool,
    force: bool,
    concurrency: Option<usize>,
    resolver: Option<&'a dyn SourceResolver>,
    progress: &'a dyn IndexProgress,
    actor: Option<&'a SubjectId>,
}
```

Every field private, one constructor, and a setter per field that takes `self`
and returns `Self`:

```rust
#[must_use]
pub fn download(mut self, yes: bool) -> Self {
    self.download = yes;
    self
}
```

Taking `self` by value rather than `&mut self` is what allows chaining, because
each call hands the whole value back:

```rust
let ingest = Ingest::new(&home)
    .name(name.as_deref())
    .download(download)
    .force(force)
    .concurrency(concurrency)
    .progress(progress);

for location in &locations {
    ingest.index_location(location, collection)?;
}
```

`Ingest::new(home)` is already complete and valid; each setter narrows it. That
shape is the one `std::process::Command` uses, and it is worth copying for the
same reason: the call site now names each value, so `download` and `force` cannot
be transposed, and everything invariant is stated once above the loop.

The real payoff is not tidiness. **A new collaborator is one field and one
setter**, reachable by whichever phase needs it, without a single signature in
between changing. Custody proved it: `actor` was added as a field, a setter, and
one expression — and about fifty lines of out-of-band machinery were deleted.

## Why a `Copy` builder needs `#[must_use]`

`Ingest` derives `Copy`, because it is a handful of references and two bools and
gets passed to every phase. That combination has a trap:

```rust
let ingest = Ingest::new(&home);
ingest.force(true);          // compiles. does nothing.
ingest.index_location(loc, coll)?;
```

On an ordinary builder the borrow checker saves you: `force` consumed `ingest`,
so using it afterwards is a use-after-move and the compiler objects. On a `Copy`
builder there is no move to object to — the setter gets a *copy*, configures it,
and returns it into nothing.

`#[must_use]` on each setter is what restores the warning. It says "the return
value of this function is the point", so discarding it is a diagnostic. That
attribute is doing work no other part of the language does here.

## Grouping, for values that travel together

A builder suits *configuration*. It does not suit values that are computed and
handed along, and two of those got their own types.

```rust
pub(super) struct Docs<'a> {
    pub crawl_id: &'a str,
    pub crawl_name: &'a str,
    pub collection: &'a str,
    pub search: &'a Mutex<SearchIndex>,
}
```

These four went through every page-indexing function as positional arguments,
and three of them are `&str` in a fixed order. Transposing `crawl_id` and
`crawl_name` would compile, and would mis-tag every document in the index while
leaving the manifest perfectly correct — so the manifest-focused tests would all
still pass. Naming the fields at each construction site is what makes that
mistake visible.

That risk was real enough to be worth a test on its own, asserting the
*document's* tags rather than the manifest's, with the three values deliberately
distinct so a swap cannot slip through.

`record::Indexed` is the same idea for the other end of the pipeline:
everything learned about one WACZ, as one value, so `upsert` takes two
parameters instead of eight.

## When to reach for which

- Many optional or defaulted knobs, set once, read in several places → builder.
- A fixed set of values that always travel together → a struct, with named
  fields.
- Two or three parameters that are genuinely independent → leave them alone. A
  grouping type with one caller is just indirection.

And the thing to actually watch for: a lint firing repeatedly in the same
module. Here it fired eight times over months and was silenced eight times,
which turned a design signal into invisible debt and then into two bugs. The
lint was right the first time.

## What to take forward

- `self`-taking setters returning `Self` give you chaining.
- Private fields plus one constructor mean the type can only be built one way.
- `#[must_use]` on a `Copy` builder's setters, or a dropped call compiles
  silently.
- Group values that travel together, especially same-typed ones where a
  transposition would still compile.
