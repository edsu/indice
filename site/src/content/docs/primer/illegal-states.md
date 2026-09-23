---
title: Making illegal states unrepresentable
description: Newtypes, and pushing validation to the edge so the inside of the program cannot be wrong.
---

A collection id becomes a directory name:

```
<home>/collections/<slug>/README.md
```

So an id of `../../etc` is not a bad value, it is a path traversal. The obvious
defence is to check before using it — and the problem with checking is that it
has to be done at *every* use, by everyone, forever. Miss one and the bug is
back.

Rust offers a different move: make the bad value impossible to hold in the first
place.

## The newtype

```rust
pub struct CollectionId(String);
```

A struct wrapping one field — a **newtype**. At runtime it is a `String` and
nothing more; the wrapper exists entirely for the compiler. What makes it useful
is that the field is private, so outside `collections` nobody can build one
except through:

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

This function does no validation, and it does not need any. Its signature says
the caller already has a value that was checked, because there is no other way
to have one. Code that tries to build a collection path out of raw request input
does not compile.

That is the idea usually called **parse, don't validate**: rather than checking a
`String` and passing the same `String` on — where the next function has no idea
whether it was checked — turn it into a type that can only exist if the check
passed. The knowledge lives in the type instead of in everyone's memory.

## The back door, and closing it

There is a hole in the plan. `serde` can build a struct field by field, and a
derived `Deserialize` would happily construct a `CollectionId` from whatever
string is in the JSON — no `parse` in sight.

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

Read a `String`, then go through the same front door as everyone else. The rule
becomes "every `CollectionId` in the process went through `parse`", with no
exceptions to remember — which is the property that makes `collection_dir` safe
to write without a check.

There is a wrinkle worth seeing, because it is where purity meets an existing
archive. The manifest has a *lenient* deserializer for one field: an invalid id
in `waczs.json` logs a warning and becomes the "unset" placeholder rather than
failing the whole load. Refusing to open a manifest because one entry is odd
would be a worse outcome than carrying on. The strict path is the default; the
lenient one is opt-in, named, and explained where it is used.

## The same shape, different problem: `SubjectId`

```rust
#[serde(transparent)]
pub struct SubjectId(String);
```

This one identifies a person — the value behind `Wacz::added_by`, which decides
who may delete a crawl. It has the same private field and the same
no-`From<&str>` discipline, but `parse` does more than reject: it
**canonicalizes**, folding case so `Ed@X.edu` and `ed@x.edu` are one identity,
and normalising `mailto:` and the internal URN forms.

That is the second thing a newtype buys. Not just "this value is valid" but
"this value is in the one canonical shape", so comparison is `==` rather than a
function everyone has to remember to call.

`#[serde(transparent)]` means it serializes as the bare string rather than
`{"0": "…"}` — the wrapper is invisible on disk. And there is a deliberately
separate `parse_remote`, because an identity arriving from a proxy header must
never be allowed to resolve to the local operator; same type, different door,
different rules.

## When it is worth it

Not every `String` wants a newtype. The question is whether the value has an
invariant that matters, and whether the cost of forgetting it is high. Here both
answers were yes: an unchecked id is a filesystem escape, and an uncanonicalized
identity is a permission check against the wrong person.

The tell that you need one is a comment saying "callers must validate this
first". That comment is a type waiting to be written.

## What to take forward

- A newtype with a private field makes a validating constructor the only way in.
- `parse`, returning `Option`, rather than `validate` returning `bool`.
- Deriving `Deserialize` reopens the door; write it by hand and route through
  the same constructor.
- Canonicalizing in the constructor makes `==` mean what you want.
