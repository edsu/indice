---
title: Operator configuration
description: The optional home-level config.yaml and its footprint knobs.
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

**Omitting the file entirely means every authenticated user is an admin**, which is the behavior before roles existed. See [Who can do what](/docs/guides/manage/#who-can-do-what) for what each role may do, and the lock-yourself-out warning.

A malformed `users.yaml` stops startup rather than being ignored: silently skipping a typo in a permissions file is how you end up granting access you didn't intend.
