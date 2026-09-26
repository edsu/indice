---
title: Guards, `Drop`, and `!Send`
description: RAII in Rust, and a bug where the guard owned the wrong thing.
---

Rust has no `finally`. It has `Drop` instead: a destructor that runs when a value
goes out of scope, guaranteed, including on an early `return` and during a
panic. Combine that with a value whose whole job is to be held, and you
get a **guard**.

```rust
let _index = super::lock::lock_index(home, "reindex", progress)?;
// …the rebuild…
// lock released here, whatever happens above
```

No unlock call, and no path through the function that can skip it. That is the
pattern behind `MutexGuard`, `File`, and indice's two write locks.

Note the `_index` name. The leading underscore says "I am not going to read
this", which silences the unused-variable warning. A bare `_` on its own is a
different thing: it drops the value at once, releasing the lock on the next line.
That single character is the difference between a held lock and no lock, and
nothing warns you.

## What the guard should own

indice's index lock is an advisory `flock` on a file, and it is re-entrant: the
same thread may take it again, because a handler takes it and then the library
function it calls takes it too. Re-entrancy needs a count, and the count lives
in a thread-local:

```rust
struct Held {
    file: File,
    depth: u32,
}
```

with the guard holding only enough to find its entry:

```rust
pub struct IndexLock {
    path: Option<PathBuf>,
    _not_send: PhantomData<*const ()>,
}
```

and `Drop` decrementing, releasing at zero:

```rust
impl Drop for IndexLock {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else { return };
        HELD.with(|h| {
            let mut h = h.borrow_mut();
            let Some(held) = h.get_mut(&path) else { return };
            held.depth -= 1;
            if held.depth == 0 {
                if let Some(held) = h.remove(&path) {
                    let _ = held.file.unlock();
                }
            }
        });
    }
}
```

### The bug

The first version put the `File` **in the guard** rather than in the
thread-local. That looks more natural, since the guard owns the thing it is
guarding, and it is wrong.

With the file in the guard, the outermost guard owned the lock and nested guards
owned nothing. So this sequence:

```rust
let outer = lock_index(&home, …)?;
let inner = lock_index(&home, …)?;   // nested: depth 2
drop(outer);                          // ← releases the flock
// `inner` still believes it holds the lock. It does not.
```

dropped the `File`, releasing the flock, while the depth count still said 1.
The next acquisition on that thread saw a non-zero count, handed back a guard
backed by nothing, and an ingest ran with no exclusion at all. No panic, no
error, no hang.

Moving the `File` into the thread-local fixes it by making release depend on
**the count reaching zero** rather than on which guard happens to drop first.
Out-of-order drops become correct.

The lesson generalises past locks: when a guard's cleanup has to coordinate with
other guards, the shared thing cannot live in any one of them. It is also a
reminder that `Drop` runs in reverse declaration order *by default*, and that an
early `drop(x)`, or guards held in a `Vec` or a struct, breaks that assumption.

## `!Send`, doing real work

```rust
_not_send: PhantomData<*const ()>,
```

`PhantomData<T>` is a zero-sized field that makes the compiler treat your type
as though it contained a `T`. Raw pointers are not `Send`, so this one field
makes `IndexLock` un-sendable between threads. It costs nothing at runtime and
buys two separate things:

**The thread-local count is only correct if the guard stays put.** The depth
lives in *this* thread's map; a guard moved to another thread would decrement
the wrong count. `!Send` makes that a compile error rather than a subtle bug.

**An axum handler cannot hold the lock across an `.await`.** This is the one to
remember. In async Rust, a value held across an await point must be `Send` if the
future is going to run on a multi-threaded executor, because the task can resume
on a different thread. So:

```rust
let _index = lock_index(&home, …)?;
some_async_thing().await;     // ← does not compile
```

The compiler refuses. Without `!Send` this would compile and hold a cross-process
file lock across an arbitrary await, for as long as an HTTP request takes. You
would find that out in production. Here the type system says no.

This is why indice does its locked work inside `spawn_blocking` (see
[Concurrency](/primer/concurrency/)): a blocking closure has no await points, so
the guard can live there safely.

## Two invariants ownership carries

Ownership holds two more properties of the lock in place, where otherwise
discipline would have to:

- **Release happens on panic.** The OS drops an `flock` when the process dies,
  so unlike Tantivy's own writer lock there is no stale lock file to delete by
  hand after a crash. `Drop` and the kernel cover the two cases between them.
- **The lock file is never unlinked.** An `flock` belongs to the inode, so
  deleting and recreating the file would leave one holder on an orphaned inode
  while the next process locks a fresh one: two exclusive holders, no error.
  `Drop` unlocks; it does not remove. There is a test asserting the inode is
  stable across release and re-acquire, because that is the sort of "tidy up
  after yourself" change someone makes later in good faith.

## What to take forward

- `Drop` is Rust's `finally`, and a guard is a value whose lifetime *is* the
  critical section.
- `let _name` keeps a guard alive; a bare `let _` drops it immediately.
- If cleanup must coordinate across guards, the shared resource cannot live in
  one of them.
- `PhantomData<*const ()>` makes a type `!Send`, which stops it crossing a
  thread *or* an await point, turning a runtime hazard into a compile error.
