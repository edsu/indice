# Cutting a release

Releases are produced entirely by the [`Release` workflow](.github/workflows/release.yml),
which runs when a `v*` tag is pushed. **Just push the tag — don't create the
release in the GitHub UI.** Creating it by hand publishes it immediately (which
bypasses the draft→publish safety gate below) and leaves a confusing duplicate
alongside the one the workflow makes.

## Steps

1. Bump the version in the workspace crates and commit (they move in lockstep):
   `crates/indice-lib/Cargo.toml` and `crates/indice-bin/Cargo.toml`
   (`version = "X.Y.Z"`), then `cargo build` so `Cargo.lock` updates too.
2. Merge to `main` and confirm CI is green.
3. Tag and push:
   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
4. Watch it (`gh run watch` or the Actions tab). The workflow will:
   - create a **draft** release,
   - build binaries on native runners — macOS (arm64/x86_64), Linux
     (arm64/x86_64), Windows (x86_64) — and attach each archive plus a
     `.sha256`,
   - build and push the multi-arch container image to `ghcr.io/edsu/indice`
     (`:X.Y.Z`, plus `:latest` for a final release),
   - and, only after every build succeeds, flip the release to **published**.

If any build fails the release stays a draft — delete it, fix, and re-tag (delete
the tag first: `git push origin :vX.Y.Z`).

## Prereleases

Tag with a pre-release suffix (e.g. `vX.Y.Z-rc1`): the image is published as
`:X.Y.Z-rc1` but **not** `:latest`.

## After a final release: update the Homebrew tap

Regenerate the formula from the new release's checksums and commit it to the
[`edsu/homebrew-indice`](https://github.com/edsu/homebrew-indice) tap:

```sh
scripts/homebrew-formula.sh vX.Y.Z > ../homebrew-indice/Formula/indice.rb
cd ../homebrew-indice && git commit -am "indice X.Y.Z" && git push
```

The script (first-party `gh` + shell) downloads the release's `.sha256` files and
emits the per-platform formula. Optionally verify before pushing with
`brew audit --tap edsu/indice indice` and `brew install edsu/indice/indice`.
Then `brew upgrade indice` picks up the new version.
