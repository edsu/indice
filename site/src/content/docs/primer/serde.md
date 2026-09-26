---
title: Serde, where the types *are* the schema
description: Deriving a file format, migrating it without a migration, and choosing how strict to be.
---

indice keeps its records as files a curator can read and commit: `waczs.json` for
the crawl ledger, `collections/<slug>/README.md` for finding aids,
`users.yaml` for the roster, JSONL for annotations. There is no database and no
schema file. The Rust types *are* the schema, and
[serde](https://serde.rs) generates the reading and writing.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wacz {
    pub id: String,
    pub name: String,
    pub file_size: u64,
    // …
}
```

That is the whole mechanism: `Serialize` writes, `Deserialize` reads, and the
field names become the keys. The rest of this chapter is the attributes, because
those are where the design decisions live.

## Adding a field without breaking yesterday's file

Crawl custody arrived long after the first manifests were written, as a new
field on `Wacz`. Every existing `waczs.json` lacks it. Two attributes make that
a non-event:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub added_by: Option<SubjectId>,
```

`default` says a missing key is `None` rather than an error, so old files load.
`skip_serializing_if` says don't write the key when it is `None`, so a manifest
that never had custody round-trips **byte-identically** — it does not suddenly
sprout `"added_by": null` and show up as noise in a curator's `git diff`.

That pairing is the single most useful serde idiom in this codebase. It is what
"additive fields" means in practice, and there is a test named
`a_manifest_without_custody_still_loads` that pins both halves: a legacy record
parses, and re-serializing it does not gain the key.

## Renaming a field without a migration

The manifest used to call the WACZ location `path`. When it grew to cover remote
sources, `path` became the wrong word, and the new name is `source`:

```rust
/// The WACZ location. Older manifests used the key `path`.
#[serde(alias = "path")]
pub source: Source,
```

`alias` accepts the old key on read while writing only the new one. Old files
load; the next save quietly modernises them. No migration script, no version
field, no flag day.

It is worth noticing what this buys in a tool people run on their own laptops at
their own pace: you cannot require everyone to upgrade in order. A format that
tolerates its own history is doing real work.

## Storing an enum as one string

`Source` is an enum with four variants, some carrying several fields. By default
serde would write it as a tagged object. Instead:

```rust
#[serde(from = "String", into = "String")]
pub enum Source { File(PathBuf), Url(String), Browsertrix { … }, … }
```

with `From<Source> for String` and back. So a Browsertrix source appears in JSON
as `browsertrix|host|org|item|resource` — one flat string.

The reason is the human reading the file. A curator scanning `waczs.json` sees a
line they can understand and grep; nested objects for a value that is
conceptually "where this came from" would be worse to read for no benefit. The
type stays rich in Rust and flat on disk, which is the trade this whole approach
is about.

## How strict should reading be?

Serde's default is permissive: unknown keys are ignored. That default is right
for most files and wrong for one, and indice picks deliberately in three places.

**Strict, and the strictest thing in the codebase** — the roster:

```rust
/// `deny_unknown_fields` because this is a permissions file: a typo'd `Role:`
/// or `rol:` would otherwise be ignored and silently fall back to the
/// `curator` default, granting more than the operator wrote down.
#[serde(deny_unknown_fields)]
```

This is the argument for strictness in one sentence. Permissiveness usually
costs you a shrug; here it would silently grant privilege the operator did not
intend. A misspelled key must be an error.

**Strict by construction** — `CollectionId` hand-writes `Deserialize` so every
value goes through `parse`, as
[Making illegal states unrepresentable](/primer/illegal-states/) covers.

**Deliberately lenient** — the same id, in one place:

```rust
#[serde(default, deserialize_with = "lenient_collection")]
pub collection: CollectionId,
```

An invalid collection id in `waczs.json` logs a warning and becomes the "unset"
placeholder, which `Manifest::open` then heals, instead of failing the whole
load. Refusing to open an archive's entire ledger because one entry is
malformed would be the worse outcome — you would have taken away the tool they
need to fix it.

So the rule is not "be strict" or "be lenient" but: **be strict where a mistake
grants something, and lenient where strictness would deny access to the data.**
And when you choose lenient, say so out loud — that attribute is named
`lenient_collection`, not `collection_de`, and it warns when it fires.

## The cost of types-as-schema

Being honest about the trade-off. The upside is that there is one definition, the
compiler checks the code against it, and files stay human-readable and
diffable. The downside is that the *file format is now as stable as your
structs*, so renaming a field is a compatibility decision rather than a
refactor. `alias` and `default` are what make that survivable, and they only
help if you remember to reach for them.

There is also no enforced version number anywhere. indice leans entirely on
additive fields and aliases instead, which works as long as every change is
backwards-compatible — and stops working the day one is not.

## What to take forward

- `derive(Serialize, Deserialize)` makes the type the schema.
- `#[serde(default, skip_serializing_if = "…")]` is how a field is added
  without touching existing files or dirtying their diffs.
- `#[serde(alias = "old")]` renames a key with no migration.
- `#[serde(from/into)]` keeps a type rich in memory and simple on disk.
- Choose strictness per file: `deny_unknown_fields` where a typo would grant
  something; lenient where a hard failure would lock someone out of their data.
