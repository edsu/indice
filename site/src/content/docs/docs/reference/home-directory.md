---
title: The home directory
description: How indice lays out a home — the WACZ archive, the editable finding aids you can version-control, and the rebuildable search index.
---

indice keeps everything for an archive under a single **home directory** (default: the current directory; pass `--home <DIR>` to any command to point elsewhere). A home is self-contained and portable — copy the whole folder to another disk or machine and it still works.

```
<home>/
├── archive/<slug>/       your WACZ files, organized by collection
├── collections/<slug>/   finding aids you author + commit (README.md, thumbnails, notes)
├── index/                search index + derived metadata (rebuildable; git-ignore it)
└── config.yaml           optional operator settings
```

## `archive/` — the captures

The WACZ files themselves, grouped into a subfolder per collection slug. When you `indice index` a local WACZ it's filed in here for you — **moved** if it was already under `archive/`, **copied** otherwise, so your original is left intact. Paths are stored **relative to the home**, which is what makes the whole directory portable.

A **remote** WACZ indexed by URL is *not* copied here by default — indice streams it and records the URL as the source (add `--download` to keep a local copy in `archive/` instead). So `archive/` holds exactly the bytes you've chosen to keep locally.

## `collections/` — the part worth keeping

This is the curatorial layer: plain files a person writes, meant for version control. It's the part of a home worth committing to git.

- **`collections/<slug>/README.md`** — the collection's **finding aid**: a small YAML front-matter block (creator, dates, rights, subjects) followed by a Markdown narrative (scope & content, custodial history, and so on). indice writes it from [`indice collection set`](/docs/reference/cli/) or the workroom's [Edit collection](/docs/guides/manage/) form — and because it's just Markdown, you can also edit it by hand in any editor and review the change as a diff.
- **`collections/<slug>/crawls/<id>.md`** — an optional per-crawl note (via `indice crawl set <id> --note`, or by hand).
- **Thumbnails / images** — a pinned collection or crawl image lives here too, so it survives reindexing.

Everything under `collections/` is authored, not derived — the same files whether you wrote them at the command line, in the browser workroom, or in a text editor.

## `index/` — derived and disposable

The embedded [Tantivy](https://github.com/quickwit-oss/tantivy) full-text index plus a manifest of every source (its path or URL, and the SHA-256 of each local WACZ). It's **rebuilt from the WACZs** by [`indice reindex`](/docs/reference/cli/), so you never need to back it up — treat it as a cache and **git-ignore it**.

## `config.yaml` — optional settings

Home-level operator settings (index footprint knobs). Everything has a default, so the file is optional. See [Operator configuration](/docs/reference/configuration/).

## Version control & backup

Because a home separates *authored* from *derived* data, backup is straightforward:

- **Commit `collections/`.** It's the intellectual work — descriptions, notes, chosen images — and it diffs and reviews cleanly as Markdown. A typical home in git ignores just the derived index:

  ```text
  # .gitignore
  /index
  ```

- **Back up `archive/`** if you want durability. These are the actual captures; losing them means re-fetching (or, for stream-only remote sources, re-resolving the URLs). For a shared or offline library, keep them. They're large and opaque, so many people back them up separately from the git repo rather than committing them.
- **Ignore `index/`.** It's fully reproducible with `indice reindex` — no need to store or move it.

To relocate an archive, copy the whole `<home>` (or just `collections/` + `archive/`) and run `indice reindex` at the destination to rebuild `index/`.
