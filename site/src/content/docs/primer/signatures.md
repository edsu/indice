---
title: Stopping a signature from growing
description: Builders, grouping types, and why a `Copy` builder needs `#[must_use]`.
---

One function in indice grew this signature:

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

Eight parameters, four of them optional or boolean, and two `bool`s adjacent, so
swapping them compiles and does the wrong thing without a word. You do not write
that signature in one sitting. It arrives one parameter at a time, and each
addition looks justified on its own.

Rust's linter noticed. `clippy::too_many_arguments` fires above seven, and it
fired here. Someone silenced it with `#[allow(clippy::too_many_arguments)]` eight
separate times across the ingest module. One of those allows suppressed nothing:
the function under it had seven parameters, so the lint could not fire. A dead
suppression sitting in the file shows that nobody was reading these allows any
more.

The lint was a design report, and silencing it cost two real bugs. Crawl custody
could not reach the pipeline without an eleventh parameter, so the code set it
out of band afterwards. That required diffing the manifest before and after,
which produced a lost-update race and a fail-open that handed one curator
ownership of every unattributed crawl.

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
shape is the one `std::process::Command` uses, for the same reason: the call site
names each value, so you cannot transpose `download` and `force`, and everything
invariant sits once above the loop.

The payoff is bigger than tidiness. **A new collaborator is one field and one
setter**, reachable by whichever phase needs it, without a single signature in
between changing. Custody proved it: `actor` needed a field, a setter and one
expression, and it let us delete about fifty lines of out-of-band machinery.

## A `Copy` builder needs `#[must_use]`

`Ingest` derives `Copy`, because it is a handful of references and two bools and
gets passed to every phase. That combination has a trap:

```rust
let ingest = Ingest::new(&home);
ingest.force(true);          // compiles. does nothing.
ingest.index_location(loc, coll)?;
```

On an ordinary builder the borrow checker saves you: `force` consumed `ingest`,
so using it afterwards is a use-after-move and the compiler objects. On a `Copy`
builder there is no move to object to: the setter gets a *copy*, configures it,
and returns it into nothing.

`#[must_use]` on each setter is what restores the warning. It says "the return
value of this function is the point", so discarding it is a diagnostic. That
attribute is doing work no other part of the language does here.

## Grouping, for values that travel together

A builder suits *configuration*. Values that the pipeline computes and hands
along need something else, and two of those got their own types.

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
leaving the manifest correct, so the manifest-focused tests would all still pass.
Naming the fields at each construction site is what makes that mistake visible.

That risk earned a test of its own, asserting the *document's* tags rather than
the manifest's, with the three values kept distinct so a swap cannot slip
through.

`record::Indexed` is the same idea for the other end of the pipeline:
everything learned about one WACZ, as one value, so `upsert` takes two
parameters instead of eight.

## When to reach for which

- Many optional or defaulted knobs, set once, read in several places → builder.
- A fixed set of values that always travel together → a struct, with named
  fields.
- Two or three independent parameters → leave them alone. A grouping type with
  one caller adds indirection and nothing else.

The thing to watch for is one lint firing over and over in the same module. Here
it fired eight times across months, and eight times someone silenced it, which
turned a design signal into invisible debt and then into two bugs. The lint was
right the first time.

## What to take forward

- `self`-taking setters returning `Self` give you chaining.
- Private fields plus one constructor mean the type can only be built one way.
- `#[must_use]` on a `Copy` builder's setters, or a dropped call compiles with no
  warning.
- Group values that travel together, especially same-typed ones where a
  transposition would still compile.
