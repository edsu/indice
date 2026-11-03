---
title: The home directory
description: How indice lays out a home directory, the WACZ archive, the editable finding aids you can version-control, annotations, and the rebuildable search index.
---

indice keeps everything for an archive under a single **home directory** (default: the current directory; pass `--home <DIR>` to any command to point elsewhere). A home is self-contained and portable, so you can copy the whole folder to another disk or machine and it still works.

```
<home>/
├── archive/<slug>/       your WACZ files, organized by collection
├── collections/<slug>/   finding aids you author + commit (README.md, thumbnails, notes)
├── index/                search index + derived metadata (rebuildable; git-ignore it)
└── config.yaml           optional operator settings
```

## `archive/`: the captures

The WACZ files themselves, grouped into a subfolder per collection slug. When you `indice index` a local WACZ it's filed in here for you: **moved** if it was already under `archive/`, **copied** otherwise, so your original is left intact. indice stores paths **relative to the home**, so the whole directory is portable.

A **remote** WACZ indexed by URL is *not* copied here by default; indice streams it and records the URL as the source (add `--download` to keep a local copy in `archive/` instead). So `archive/` holds exactly the bytes you've chosen to keep locally.

## `collections/`: the part worth keeping

This is the curatorial layer: plain files a person writes, meant for version control.

- **`collections/<slug>/README.md`** is the collection's **finding aid**: a small YAML front-matter block (creator, dates, rights, subjects) followed by a Markdown narrative (scope & content, custodial history, and so on). indice writes it from [`indice collection set`](/docs/reference/cli/) or the workroom's [Edit collection](/docs/guides/manage/) form. It's plain Markdown, so you can also edit it by hand in any editor and review the change as a diff.
- **`collections/<slug>/crawls/<id>.md`** is an optional per-crawl note (via `indice crawl set <id> --note`, or by hand).
- **Thumbnails / images.** A pinned collection or crawl image lives here too, so it survives reindexing.

People write everything under `collections/`; indice derives none of it. You can write these files from the command line, in the browser workroom, or in a text editor.

## `index/`: derived and disposable

The embedded [Tantivy](https://github.com/quickwit-oss/tantivy) full-text index plus a manifest of every source (its path or URL, and the SHA-256 of each local WACZ). It's **rebuilt from the WACZs** by [`indice reindex`](/docs/reference/cli/). A large index takes longer to rebuild, so you may want to back it up, but its binary format suits revision control poorly.

## `config.yaml`: optional settings

Home-level operator settings (index footprint knobs). Everything has a default, so the file is optional. See [Operator configuration](/docs/reference/configuration/).

## `users.yaml`: who may do what

Optional. When indice runs behind an auth proxy, this maps the identities the proxy forwards to roles. Absent means every authenticated user is an admin. See [Who can do what](/docs/guides/manage/#who-can-do-what).

Commit it alongside `collections/`: it records who can change the archive, and its git history shows when that changed.

## `events/`: the audit trail

An append-only record of every change made through the browser workroom, one JSON object per line, in monthly files (`events/2026-09.jsonl`). Each record says who acted, what they did, and to what:

```json
{"time":"2026-09-12T10:04:11Z","actor":"mailto:alice@example.org","action":"collection_delete","target":"sucho","detail":{"with_crawls":true}}
```

The manifest tells you *who holds this crawl now*; the event log tells you *who deleted that collection last week*. indice appends records and never edits or removes them, so the log stays intact when the files it describes change or disappear.

Two things to know about it:

- **It is never served over HTTP.** No route reads it, unlike `collections/`, which is public by design.
- **It holds login identities, not display names.** A record has to survive someone's name being corrected in `users.yaml` afterwards, so it stores the identity your proxy forwarded, which is usually an email address.

If you publish your home directory, either accept that the addresses are in the history, or exclude it:

```text
# .gitignore
/events
```

Changes made from the command line are not recorded here: the CLI has no request identity, and a shell already has its own history.

## Version control & backup

A home keeps *authored* and *derived* data apart, so you can treat each differently:

- **Commit `collections/`.** It's the intellectual work (descriptions, notes, chosen images), and it diffs and reviews cleanly as Markdown. A typical home in git ignores only the derived index:

  ```text
  # .gitignore
  /index
  ```

  Commit `users.yaml` too, if you have one. Consider whether to commit `events/`, which holds login identities (above).

- **Back up `archive/`** if you want durability. These are the captures; losing them means re-fetching (or, for stream-only remote sources, re-resolving the URLs). For a shared or offline library, keep them. They're large and opaque, so many people back them up separately from the git repo rather than committing them.
- **Ignore or include `index/`.** `indice reindex` rebuilds it from scratch, though that takes longer as your archive grows.

To relocate an archive, copy the whole `<home>` (or only `collections/` + `archive/`) and run `indice reindex` at the destination to rebuild `index/`.
