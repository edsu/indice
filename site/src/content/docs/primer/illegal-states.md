---
title: Making illegal states unrepresentable
description: Newtypes, and pushing validation to the edge so the inside of the program cannot be wrong.
---

A collection id becomes a directory name:

```
<home>/collections/<slug>/README.md
```

So an id of `../../etc` is a path traversal. The obvious defence is to check
before using it, but then every use has to check, and the first one that misses
reopens the bug.

Rust lets you make the bad value impossible to hold in the first place.

## The newtype

```rust
pub struct CollectionId(String);
```

A struct wrapping one field is a **newtype**. At runtime it is a `String` and
nothing more; the wrapper exists for the compiler. The field is private, so
outside `collections` you cannot build one except through:

```rust
pub fn parse(s: &str) -> Option<Self> {
    let ok = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then(|| CollectionId(s.to_string()))
}
```

No `/`, no `.`, so neither an absolute path nor a `..` component can appear.
`parse` returns `Option`, so a caller must decide what to do with a bad one.

Now look at what that buys downstream:

```rust
pub fn collection_dir(home: &Path, slug: &CollectionId) -> PathBuf
```

This function does no validation. Its signature says
the caller already has a value that was checked, because there is no other way
to have one. Code that tries to build a collection path out of raw request input
does not compile.

That is the idea usually called **parse, don't validate**. Instead of checking a
`String` and passing the same `String` on, leaving the next function no way to
know whether anyone checked it, you turn it into a type that can only exist if
the check passed.

## The back door, and closing it

There is a hole in the plan. `serde` can build a struct field by field, and a
derived `Deserialize` would construct a `CollectionId` from whatever string sits
in the JSON, with no `parse` in sight.

So `CollectionId` derives `Serialize` and implements `Deserialize` by hand:

```rust
impl<'de> Deserialize<'de> for CollectionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        CollectionId::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "invalid collection id {s:?}: expected ASCII letters, digits and '-' only"
            ))
        })
    }
}
```

Read a `String`, then go through the same front door as everyone else. Every
`CollectionId` in the process went through `parse`, and that property is what
makes `collection_dir` safe to write without a check.

One wrinkle shows where the pure version meets an existing archive. The manifest
has a *lenient* deserializer for one field: an invalid id in `waczs.json` logs a
warning and becomes the "unset" placeholder rather than failing the whole
load. Refusing to open a manifest because one entry is odd
would be a worse outcome than carrying on. The strict path is the default; the
lenient one is opt-in, named, and explained where it is used.

## The same shape, different problem: `SubjectId`

```rust
#[serde(transparent)]
pub struct SubjectId(String);
```

This one identifies a person. It is the value behind `Wacz::added_by`, which
decides who may delete a crawl. It has the same private field and the same
no-`From<&str>` discipline, but `parse` does more than reject: it
**canonicalizes**, folding case so `Ed@X.edu` and `ed@x.edu` are one identity,
and normalising `mailto:` and the internal URN forms.

That is the second thing a newtype buys: every value sits in one canonical
shape, so comparison is `==` rather than a function everyone has to remember to
call.

`#[serde(transparent)]` means it serializes as the bare string rather than
`{"0": "…"}`, so the wrapper is invisible on disk. A separate `parse_remote`
exists on purpose, because an identity arriving from a proxy header must never
resolve to the local operator, so it applies stricter rules to the same type.

## When it is worth it

Not every `String` needs a newtype. Ask whether the value carries an invariant
and what forgetting it costs. Here an unchecked id is a filesystem escape, and an
uncanonicalized identity is a permission check against the wrong person, so both
answers came out yes.

A comment saying "callers must validate this first" is the usual sign you need
one.

## What to take forward

- A newtype with a private field makes a validating constructor the only way in.
- `parse`, returning `Option`, rather than `validate` returning `bool`.
- Deriving `Deserialize` reopens the door; write it by hand and route through
  the same constructor.
- Canonicalizing in the constructor makes `==` mean what you want.
