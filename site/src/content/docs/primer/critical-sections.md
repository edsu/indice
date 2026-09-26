---
title: Closures as a critical section
description: Why locking the write was not enough, and how a closure makes the whole operation unskippable.
---

Here is a rule that sounds complete and is not: *lock the manifest before saving
it.*

```rust
let mut manifest = Manifest::open(&index_dir)?;   // read
manifest.upsert_wacz(entry);                      // modify
manifest.save()?;                                 // write  ← lock here?
```

`Manifest::save` rewrites `waczs.json` wholesale from an in-memory vec. So if
you lock only the save, two writers can still lose each other's work: both read,
both modify their own copy, both save in turn, and the second overwrites
everything the first added. The lock was held for the write and the damage was
done during the read.

**The lock has to span the read.** That is the whole problem, and it is not
obvious from looking at either half — each looks like ordinary correct code.

## Making it unskippable

You could document it. A comment saying "take the lock before opening the
manifest" is exactly the kind of instruction that holds until someone adds a
sixth call site at 5pm. So indice does not document it; it makes the shape the
only one available:

```rust
pub(crate) fn manifest_write<T>(
    home: &Path,
    what: &str,
    f: impl FnOnce(&mut Manifest) -> Result<T>,
) -> Result<T> {
    let _guard = super::lock::lock_manifest(home, what)?;
    let mut manifest = Manifest::open(&index_dir)?;
    let out = f(&mut manifest)?;
    manifest.save()?;
    Ok(out)
}
```

The caller supplies only the *modify* step:

```rust
manifest_write(home, "a crawl deletion", |manifest| {
    manifest.remove_wacz(crawl_id);
    Ok(())
})?;
```

They never see `open`, never call `save`, and cannot get between them. The read
and the write are inside one hold because there is no way to express them
apart.

## `impl FnOnce(&mut Manifest) -> Result<T>`

Worth reading that parameter slowly, because closure types are where Rust gets
unfamiliar.

**`FnOnce`** is the least demanding of the three closure traits, so it accepts
the most closures. `FnOnce` may consume what it captured and can be called once;
`FnMut` may be called repeatedly and mutate its captures; `Fn` may be called
repeatedly without mutating. Since `manifest_write` calls `f` exactly once,
`FnOnce` is the honest bound — asking for `Fn` would reject perfectly good
closures that move a value in.

**`impl Trait`** in argument position means "some concrete type implementing
this", resolved at compile time. Every closure has its own anonymous type, so
this is a generic parameter with nicer syntax, and the closure is inlined rather
than called through a pointer. `&mut Manifest` gives the closure exclusive
access for its duration.

**`-> Result<T>`** does two jobs. The `T` lets a caller get a value back out —
`delete_collection` returns whether a collection was actually removed — and the
`Result` means a closure that fails leaves the file untouched, because
`manifest_write` only reaches `save` on `Ok`. Bailing out mid-change needs no
undo.

## The re-entrancy trap

The manifest lock is re-entrant, because it shares a mechanism with the index
lock, where re-entrancy is needed. Here it is actively dangerous:

```rust
manifest_write(home, "outer", |m| {
    manifest_write(home, "inner", |m2| { … })   // ← would sail straight through
})
```

The nested call takes the lock again without blocking, opens a *second* manifest
from disk — without the outer one's uncommitted edits — saves it, and is then
silently overwritten when the outer `save` runs. That is precisely the loss the
module exists to prevent, arrived at *through* the lock rather than in spite of
it.

Nothing nests today. It is refused anyway:

```rust
if super::lock::held_by_this_thread(home, Scope::Manifest) {
    anyhow::bail!("manifest_write ({what}) was called inside another manifest_write; …");
}
```

An error, not a hang and not a silent loss. The point is that the refactor which
introduces nesting should break loudly, rather than the data quietly.

## When this shape is worth it

A closure-scoped API costs the caller some flexibility: they cannot hold the
resource across unrelated work, and they cannot easily return a borrow of it.
That cost is the feature when the whole risk is holding it too long or forgetting
a step.

Reach for it when **a sequence must not be interrupted**, and especially when
the first step being wrong is invisible at the last step. Do not reach for it
when callers legitimately need to interleave other work — you will end up with
an escape hatch, and then the guarantee is gone anyway.

## What to take forward

- If a read-modify-write must be atomic, the lock spans the read; locking the
  write alone fixes nothing.
- A closure parameter turns "do these in this order" from documentation into
  the only available shape.
- `FnOnce` is the weakest bound and therefore the most permissive — ask for the
  least you need.
- Returning `Result<T>` from the closure gives both a value out and
  nothing-saved-on-failure for free.
