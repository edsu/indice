# Contributing to indice

Thanks for your interest in indice! Bug reports, fixes, documentation
improvements, and ideas are all welcome.

## Before you start

- **Bugs:** open an issue with steps to reproduce. If the problem involves a
  particular WACZ, say how it was created (e.g. Browsertrix, ArchiveWeb.page)
  and, if you can, link to a small example.
- **Features:** please open an issue to discuss before writing code. indice
  aims to be a focused reading room for WACZ collections, so not every good
  idea will fit.
- **Small fixes** (typos, docs, obvious bugs) can go straight to a pull request.

## Development

```sh
cargo build
cargo test
cargo fmt && cargo clippy
```

Please make sure tests pass and `clippy` is clean before opening a PR. Keep
pull requests focused on one change, and include tests where it makes sense.

## AI-assisted contributions

indice itself was built with extensive help from AI tools, so you're welcome
to use them too. A few expectations:

- **You own your contribution.** Understand, test, and be able to explain
  every change you submit. "The model wrote it" isn't an answer in review.
- **Disclose meaningful assistance** with a commit trailer, e.g.
  `Assisted-by: Claude Code (Claude Opus 4.5)`, or a sentence in the PR
  description. This helps review; it isn't used to judge your tools.
- **No unattended agents.** Pull requests opened autonomously by AI agents
  will be closed.

## Conduct

Be kind and assume good faith. Archiving work involves people's history and
communities, so please bring the same care to discussions here.

## License

By contributing, you agree that your contributions will be licensed under the
same license as the project (see [LICENSE](LICENSE)).
