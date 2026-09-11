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

The WACZ files themselves, grouped into a subfolder per collection slug. When you `indice index` a local WACZ it's filed in here for you: **moved** if it was already under `archive/`, **copied** otherwise, so your original is left intact. Paths are stored **relative to the home**, which is what makes the whole directory portable.

A **remote** WACZ indexed by URL is *not* copied here by default; indice streams it and records the URL as the source (add `--download` to keep a local copy in `archive/` instead). So `archive/` holds exactly the bytes you've chosen to keep locally.

## `collections/`: the part worth keeping

This is the curatorial layer: plain files a person writes, meant for version control.

- **`collections/<slug>/README.md`** is the collection's **finding aid**: a small YAML front-matter block (creator, dates, rights, subjects) followed by a Markdown narrative (scope & content, custodial history, and so on). indice writes it from [`indice collection set`](/docs/reference/cli/) or the workroom's [Edit collection](/docs/guides/manage/) form. Because it's just Markdown, you can also edit it by hand in any editor and review the change as a diff.
- **`collections/<slug>/crawls/<id>.md`** is an optional per-crawl note (via `indice crawl set <id> --note`, or by hand).
- **Thumbnails / images.** A pinned collection or crawl image lives here too, so it survives reindexing.

Everything under `collections/` is authored, not derived. These can be written using the command-line, in the browser workroom, or in a text editor.

## `index/`: derived and disposable

The embedded [Tantivy](https://github.com/quickwit-oss/tantivy) full-text index plus a manifest of every source (its path or URL, and the SHA-256 of each local WACZ). It's **rebuilt from the WACZs** by [`indice reindex`](/docs/reference/cli/). As your index gets larger, and more time consuming to rebuild, you may find yourself wanting to back it up, but it is a binary format that may not work well with revision control.

## `config.yaml`: optional settings

Home-level operator settings (index footprint knobs). Everything has a default, so the file is optional. See [Operator configuration](/docs/reference/configuration/).

## `users.yaml`: who may do what

Optional. When indice runs behind an auth proxy, this maps the identities the proxy forwards to roles. Absent means every authenticated user is an admin. See [Who can do what](/docs/guides/manage/#who-can-do-what).

Worth committing alongside `collections/`: it is a deliberate, reviewable statement of who can change the archive, and its history is useful.

## `events/`: the audit trail

An append-only record of every change made through the browser workroom, one JSON object per line, in monthly files (`events/2026-09.jsonl`). Each record says who acted, what they did, and to what:

```json
{"time":"2026-09-12T10:04:11Z","actor":"mailto:alice@example.org","action":"collection_delete","target":"sucho","detail":{"with_crawls":true}}
```

It answers the question the manifest cannot: not *who holds this crawl now*, but *who deleted that collection last week*. Records are only ever appended, never edited or removed, which is the point of keeping it separately from the files it describes.

Two things to know about it:

- **It is never served over HTTP.** No route reads it, unlike `collections/`, which is public by design.
- **It holds login identities, not display names.** A record has to survive someone's name being corrected in `users.yaml` afterwards, so it stores the identity your proxy forwarded, which is usually an email address.

That second point matters if you publish your home directory. Either accept that the addresses are in the history, or exclude it:

```text
# .gitignore
/events
```

Changes made from the command line are not recorded here: the CLI has no request identity, and a shell already has its own history.

## Version control & backup

Because a home separates *authored* from *derived* data, backup is straightforward:

- **Commit `collections/`.** It's the intellectual work (descriptions, notes, chosen images), and it diffs and reviews cleanly as Markdown. A typical home in git ignores just the derived index:

  ```text
  # .gitignore
  /index
  ```

  Commit `users.yaml` too, if you have one. Consider whether to commit `events/`, which holds login identities (above).

- **Back up `archive/`** if you want durability. These are the actual captures; losing them means re-fetching (or, for stream-only remote sources, re-resolving the URLs). For a shared or offline library, keep them. They're large and opaque, so many people back them up separately from the git repo rather than committing them.
- **Ignore or include `index/`.** It's fully reproducible with `indice reindex`, but could get more expensive to regenerate from scratch as your archive grows.

To relocate an archive, copy the whole `<home>` (or just `collections/` + `archive/`) and run `indice reindex` at the destination to rebuild `index/`.
