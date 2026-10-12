---
title: Operator configuration
description: The optional home-level config.yaml: index footprint knobs and upload limits.
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

These apply on `index` and `reindex`; use `indice stats` to see their effect on the footprint.

## Upload limits

```yaml
uploads:
  # The largest single upload through the browser, in MiB.
  max_mb: 2048
  # The most the archive may hold, in GiB.
  max_total_gb: 200
```

Both are unset by default, which means no limit. There is no default that suits everyone, since a WACZ of a large site can legitimately run to several gigabytes. **If you let people you don't know add crawls, set both.**

- `max_mb` bounds one upload. A larger one is refused with `413`, whether or not the browser declares its length up front.
- `max_total_gb` bounds the archive. It counts the local WACZ files the manifest records. A crawl indexed from a URL is streamed in place and uses no local disk, so it doesn't count. An upload that would go past the limit is refused with `507` and told how much room is left, and once the archive is full every upload and every download import is refused before it starts. A single Browsertrix or Archive-It import can still go over by its own size, because its size isn't known until its files are listed.

The accession desk shows both numbers, so a curator can see the room before starting.

These limits apply to the browser only. `indice index` on the command line is you at the machine, and is not limited. A value of `0` is refused when the file is read rather than taken to mean "no limit"; leave the key out for that. The file is read at startup, so a change needs a restart. After **upgrading** to a version that changes the index schema or these defaults, run `indice reindex` to rebuild. That's how existing homes pick up footprint improvements (a stale index built by an older version is detected, and indice tells you to reindex).

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

**Omitting the file entirely means every authenticated user is an admin**, which is the behavior before roles existed. A **server refuses to start** in that state, since it would hand the archive to anyone its identity provider admits; a workstation ignores the file either way. `users: []` is a real answer meaning nobody may write. See [Who can do what](/docs/guides/manage/#who-can-do-what) for what each role may do, and the lock-yourself-out warning.

Changes take effect without a restart. indice checks whether the file has moved and re-reads it when it has, so approving someone is an edit.

A malformed `users.yaml` is never ignored: silently skipping a typo in a permissions file is how you end up granting access you didn't intend. At startup it stops the server. On a reload it keeps the roster already in force and logs the error, which is the same rule applied to a running process: falling back to "no file" would promote everyone who is signed in. Deleting the file while indice is running does the same, so `rm users.yaml` is not a way to grant yourself admin.
