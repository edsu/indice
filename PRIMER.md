# A Rust primer, via indice

This has moved to the documentation site, and been rewritten:

**<https://indice.page/primer/>**

It was a single 2,200-line file arranged by Rust feature, with the codebase as
illustration. It is now a set of chapters on the site, arranged the other way
round: a short Part I covering just enough Rust to read the code, then chapters
on the problems indice actually solved, each introducing the Rust that solved
it — newtypes that make an invalid id unconstructible, witness types that make a
forgotten permission check a compile error, guards and `!Send`, closures as a
critical section.

The rewrite happened because the old version had gone quietly wrong rather than
merely stale. Most of the code it described had moved, and of its 126
`file.rs:line` citations, 42 pointed at files that no longer existed while the
survivors had drifted onto unrelated lines. The site version cites
`module::Item` instead, and `crates/indice-lib/tests/primer.rs` fails the build
if a line-number citation reappears or a referenced path stops existing.

The old text remains in the git history if you want it: `git show
v0.2.0:PRIMER.md`. `PRIMER.pdf` was removed in the same change — it was last
built three months before the markdown it was generated from, which is the
failure mode this move is meant to end.
