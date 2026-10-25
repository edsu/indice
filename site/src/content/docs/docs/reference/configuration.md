---
title: Operator configuration
description: "The optional home-level config.yaml: index footprint, how much of a WACZ indice will unpack, upload limits, and what the site calls itself."
---

An optional `<home>/config.yaml` holds home-level settings. The file is optional and every field has a default, so you only write the knobs you want to change; unknown keys are ignored (the file can grow over time). Run `indice config` to print the resolved values (your file merged over the defaults).

```yaml
index:
  # Bytes of page body text STORED per document for snippets, in KiB. The full
  # body is always indexed, so search recall is unaffected; this only bounds
  # the stored copy used to render snippets. Lower = smaller index. Omit for the
  # 16 KiB default; 0 stores the whole body (no cap).
  stored_body_cap_kb: 16
  # Tantivy indexing buffer, in MiB: the build-time RAM ceiling / throughput
  # knob. Higher = fewer, larger segments (faster bulk ingest, more RAM). Omit
  # for the 50 MiB default; values below Tantivy's ~15 MiB minimum are clamped
  # up (so 0 is *not* unlimited here).
  writer_heap_mb: 50
```

These apply on `index` and `reindex`; use `indice stats` to see their effect on the footprint. After **upgrading** to a version that changes the index schema or these defaults, run `indice reindex` to rebuild. That's how existing homes pick up footprint improvements (a stale index built by an older version is detected, and indice tells you to reindex).

## Reading a WACZ

```yaml
index:
  # The most indice inflates of any one file inside a WACZ, in MiB. Omit for
  # the 1024 MiB default; 0 is no limit.
  max_member_mb: 1024
```

A WACZ is a zip, and a file inside a zip can inflate to a thousand times its stored size, so a WACZ of a few megabytes can contain gigabytes of page list or index. indice reads no single file inside a WACZ past this limit. A WACZ whose metadata, page lists or index ask for more is refused, naming the file and this setting, and is not added. A single archived page or record over it is skipped with a warning, and the rest of the crawl is indexed. This applies to `indice index` and `reindex` as well as to the browser, since it protects the machine doing the indexing whoever started it.

The default leaves plenty of room for a genuine crawl. The largest file in one is usually `pages/extraPages.jsonl`, which holds the rendered text of every page. If a crawl you trust is refused, raise the limit to fit it. Indexing reads a WACZ a page at a time, so the memory it needs does not grow with the crawl; raising the limit only lets a single file or record be larger.

## Upload limits

```yaml
uploads:
  # The largest single WACZ a curator may add through the browser, in MiB.
  max_mb: 2048
  # The most the archive may hold, in GiB.
  max_total_gb: 200
```

Both are unset by default, which means no limit. No default suits everyone, since a WACZ of a large site can legitimately run to several gigabytes. **If you let people you don't know add crawls, set both.** The server logs the limits in force at startup.

- `max_mb` bounds a single WACZ: an upload, or a URL that can't be streamed and so has to be downloaded to index. A larger one is refused with `413`.
- `max_total_gb` bounds the size of `<home>/archive/` on disk, which is where uploads and download imports land. A crawl indexed from a URL is streamed in place, and a streamed Browsertrix import stores nothing, so neither counts. Anything that would take the archive past its limit is refused with `507`, together with how much room is left. A Browsertrix or Archive-It download import is checked against its own size before it downloads.

The accession desk shows both numbers, and refuses a file that won't fit before sending it.

These limits apply to the browser only. `indice index` on the command line is you at the machine, and isn't limited. When the server starts it refuses an `uploads:` section that would quietly mean "no limit": a key it doesn't recognise, a key with no value, or `0`. Leave the key out if you want no limit. The file is read at startup, so a change needs a restart.

## Site name, tagline and banner

```yaml
site:
  # The archive's name: the header, the homepage heading, the replay viewer's
  # link home, and the end of every page title. Omit for "indice".
  name: Riverside Library Web Archive
  # One line under the homepage heading saying what the archive holds. Omit
  # for "Web archive search and replay".
  tagline: Local news and community sites since 2019
  # A notice under the header on every page, in Markdown. Omit for none.
  banner: |
    This is a **demo**. Read about it on [indice.page](https://indice.page).
  # How to ask for more than read access, in Markdown. Shown to anyone signed
  # in but not listed in users.yaml. Omit to tell them only that they can read.
  # A mailto: link can set the subject; write spaces as %20.
  request_access: Email [the archivist](mailto:archivist@example.org?subject=Curator%20access) to curate.
```

Someone who signs in but isn't in [`users.yaml`](#usersyaml) can only read. They see a note under the header saying so, followed by `request_access` if you set it, and a workroom page they open answers with the same explanation rather than a bare refusal.

The banner and `request_access` use the same Markdown as finding aids and notes: raw HTML is shown as text, images become their alt text, and links must be `http`, `https`, `mailto` or relative. A blank value counts as unset. The file is read at startup, so a change needs a restart; `indice config` shows what is in force.

## `users.yaml`

This file is important because it describes who may do what, when running behind an auth proxy. It is separate from `config.yaml` because it's a different kind of decision: `config.yaml` tunes the index, `users.yaml` grants privilege. If you are using version control you'll probably want to include it alongside your finding aids.

```yaml
users:
  - id: alice@example.org
    name: Alice Ramírez
    role: admin
  - id: jun@example.org
    aliases: [j.tanaka@old.example.org]
```

| Key | Meaning |
|---|---|
| `id` | The identity your proxy forwards (usually an email). Matched case-insensitively. |
| `name` | Public display name. Omit to derive one from the id (an email's local part). |
| `role` | `admin`, `curator`, or `reader`. Defaults to `curator`. |
| `aliases` | Prior identities, so someone keeps notes written under an old address. |

**Omitting the file entirely means every authenticated user is an admin.** A **server refuses to start** in that state, since it would hand the archive to anyone its identity provider admits; a workstation ignores the file either way. `users: []` is a real answer meaning nobody may write. See [Who can do what](/docs/guides/manage/#who-can-do-what) for what each role may do, and the lock-yourself-out warning.

Changes take effect without a restart. indice checks whether the file has moved and re-reads it when it has, so approving someone is an edit.

A malformed `users.yaml` is never ignored: silently skipping a typo in a permissions file is how you end up granting access you didn't intend. At startup it stops the server. On a reload it keeps the roster already in force and logs the error, which is the same rule applied to a running process: falling back to "no file" would promote everyone who is signed in. Deleting the file while indice is running does the same, so `rm users.yaml` is not a way to grant yourself admin.
