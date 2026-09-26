---
title: A Rust primer, via indice
description: Learning Rust by reading a real codebase, the one that runs this site's archive server.
---

Most ways into Rust teach the language and then look for something to build. This
goes the other way. indice is a working web-archive server, and nearly every part
of it exists because of a problem that had to be solved; this primer walks through
those problems and picks up the Rust on the way.

That ordering is the whole idea. A feature like `!Send` reads as trivia until you
have watched it stop an `.await` from holding a file lock. A private field is a
style choice until it is the reason a missing permission check fails to compile.
The compiler does more work here than in most languages, so the useful question
about a feature is which mistakes it makes impossible.

## How to read it

**Part I** is the short one: enough Rust to read the code without stopping every
third line. Ownership, `Result`, traits, modules. If you already write Rust, skim
it and move on.

**Part II** is the real material. Each chapter takes a decision in indice,
explains the problem, and shows the Rust that holds it in place. Several chapters
own up to a first attempt that was wrong, because the bug is usually the clearest
explanation of why the language has the feature at all.

You do not need to know anything about web archives. Where the domain matters
(what a WACZ is, why an index has to be rebuilt), the chapter explains it as it
comes up.

## Reading along in the code

Everything here points at real code in
[the repository](https://github.com/edsu/indice). Keep it open. References look
like `collections::CollectionId` or `index::ingest::Ingest`: a module path and an
item, which you can grep for.

None of them are line numbers. An earlier version of this primer cited
`collections.rs` by line, and by the time anyone checked, that line was blank and
the type had moved twice. A line number rots on every edit above it without
giving any signal, and it takes the reader's trust with it. An item name
survives, and when it does change, you can find the change.
