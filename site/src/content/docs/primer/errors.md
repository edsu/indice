---
title: Result, `?`, and anyhow
description: Errors as values, and the one habit that makes them readable.
---

Rust has no exceptions. A function that can fail says so in its type:

```rust
enum Result<T, E> { Ok(T), Err(E) }
```

Failures arrive as return values. If you call something fallible you have to say
what happens when it fails, and the compiler will not let you forget: an ignored
`Result` is a warning, and this repo denies warnings in CI.

## `?` is the whole ergonomics story

Writing `match` around every call would be unbearable, so `?` does the common
thing: unwrap the `Ok`, or return the `Err` from the enclosing function.

```rust
let config = crate::config::Config::load(home)?;
```

If `load` fails, `index_location` returns that error to *its* caller, and so on up.
It is early-return with a sigil, and it is why Rust error handling reads closer to
exceptions than the type signatures suggest.

## Two kinds of error, and which indice uses

A library that wants callers to *match* on failures defines an error `enum`. That
is the right choice for something like a parser, where "file missing" and "bad
syntax" need different handling.

indice almost never does that, because its callers almost never match. When
indexing a WACZ fails, every caller does the same thing: tell the operator and
move on. So it uses [`anyhow`](https://docs.rs/anyhow), whose `anyhow::Error` holds
any error at all, and `Result<T>` is shorthand for `Result<T, anyhow::Error>`:

```rust
use anyhow::{Context, Result};
```

You will see that pair at the top of nearly every module.

## Context, and what to put in it

An error that says `No such file or directory (os error 2)` is nearly useless: you
know something is missing, not what or why. `.with_context()` adds a layer as the
error travels up:

```rust
let sha = file_sha256(path)
    .with_context(|| format!("computing sha256 of {}", path.display()))?;
```

Printed with `{e:#}`, the layers read as a chain, *computing sha256 of
/archive/x.wacz: No such file or directory*, so you learn both what the code was
attempting and what went wrong. The closure form is deliberate: it builds the
string only when there is an error.

Copy this habit: context says **what you were trying to do**. The underlying
error already says what failed.

## `unwrap`, and where it is honest

`.unwrap()` turns an error into a panic. In application code it is usually a bug
waiting for an unusual input. There are two places it is reasonable, and indice
uses both:

- **Tests.** A failed test should panic; that is the reporting mechanism.
- **Invariants the type system cannot see.** `search.lock().unwrap()` on a mutex
  fails only if another thread panicked while holding it, at which point the
  process is already in trouble.

Where indice tolerates a poisoned lock instead of unwrapping, it says so:
`unwrap_or_else(|e| e.into_inner())` appears wherever the guarded value carries no
state worth protecting, so one panic mid-operation does not wedge every later one.

## What to take forward

- Failure is a value, in the return type, and `?` propagates it.
- `anyhow` when callers won't match; an enum when they will.
- Add context saying what you were attempting.
- `unwrap` in tests, and where a panic is the right answer.
