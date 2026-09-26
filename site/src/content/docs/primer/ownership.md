---
title: Ownership, and why signatures look like that
description: The one idea the rest of Rust hangs off, read out of indice's own function signatures.
---

Every value in Rust has exactly one owner. When the owner goes out of scope the
value is dropped, and no garbage collector is involved. That single rule is why
Rust signatures carry more punctuation than you may be used to: a signature has
to say whether it is *taking* a value, *borrowing* it, or *borrowing it to
change it*.

Three forms, and you can read them off almost any function here:

```rust
fn takes(m: Manifest)       // takes ownership; the caller no longer has it
fn borrows(m: &Manifest)    // reads it; the caller keeps it
fn changes(m: &mut Manifest) // may modify it; nobody else may touch it meanwhile
```

The rule the compiler enforces is that you may have **either** any number of `&`
borrows **or** exactly one `&mut`, never both. That is not bureaucracy for its
own sake — it is the same rule that makes data races impossible, and it shows up
again in [Guards, `Drop` and `!Send`](/primer/guards/) as the reason a lock can be
made unforgettable.

## Reading a real one

`index::ingest::index_one` is the middle of the ingest pipeline:

```rust
pub(in crate::index) fn index_one(
    cx: &Ingest,
    source: &Source,
    search: &Mutex<SearchIndex>,
    name: Option<&str>,
    collection: (&CollectionId, &str),
) -> Result<record::Indexed>
```

Every parameter is a borrow, so this function reads all of them and takes none of
them away from the caller. `search` is a `&Mutex<…>`, which is a shared borrow of
something that can still be mutated — that is the escape hatch, and it comes up in
[Concurrency](/primer/concurrency/).

The return type is where something interesting happened.

## Lifetimes, and why `Indexed` is owned

A borrow has to be backed by a value that outlives it. When a struct holds
borrowed data, it needs a **lifetime parameter** saying so. `index::ingest::Ingest`
is one:

```rust
pub struct Ingest<'a> {
    home: &'a Path,
    name: Option<&'a str>,
    // …
}
```

Read `'a` as "some scope"; the struct is promising not to outlive whatever it
borrowed. You never pick the scope yourself, the compiler does, and most of the
time you only notice lifetimes when one is too short.

`record::Indexed` — everything the pipeline learned about one WACZ — used to look
the same way, holding `&str` and `&Source` pointing into `index_one`'s own
variables. That was fine while `index_one` applied it to the manifest itself. When
the locking work changed the design so that `index_one` *returns* the value and
its caller decides when to write it, those borrows stopped being possible: the
value now outlives the function that built it.

So `Indexed` became owned — `String` instead of `&str`, `Source` instead of
`&Source`. That is a handful of clones per crawl, which is nothing next to reading
a WACZ, and it bought the ability to hold the value across a decision. That is the
usual shape of a lifetime problem in practice: not a puzzle to solve, but a
question about how long something needs to live, with cloning as a perfectly
respectable answer.

## Clone is not a failure

Coming from a GC language it is easy to read `.clone()` as something you got
wrong. Sometimes it is, in a hot loop. Usually it is the cheapest way to stop
threading a lifetime through six signatures to save an allocation that does not
matter. indice clones freely at the edges and is careful in `index::ingest::pages`,
where the per-record work actually is hot.

## What to take forward

- A `&` in a signature means the caller keeps the value.
- A `&mut` means exclusive access for the duration.
- A `'a` means the struct is holding a borrow and cannot outlive it.
- If a lifetime is fighting you, the question to ask is "how long does this
  actually need to live?" — and owning it is often the right answer.
