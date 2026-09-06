---
title: Importing from Browsertrix
description: Download and index WACZ crawls from a Browsertrix account, or stream them without keeping a copy.
---

If your WACZs live in a [Browsertrix](https://browsertrix.com/) account (Webrecorder's hosted crawler), `indice import browsertrix` downloads them into `<home>/archive` and indexes them as durable local sources.

Credentials come from the **environment**, never the command line, so they don't show up in the process list:

```sh
export BROWSERTRIX_USER='you@example.org'
read -rs BROWSERTRIX_PASSWORD; export BROWSERTRIX_PASSWORD   # prompts, no echo
# or, instead of user/password:  export BROWSERTRIX_TOKEN='<a JWT>'
```

Then import — preview first with `--dry-run`, then pull for real:

```sh
# everything you've QA'd in your (only) org, into ~/webarchive
indice import browsertrix --home ~/webarchive --dry-run
indice import browsertrix --home ~/webarchive

# just one collection (by id, slug, or name) → a matching indice collection
indice import browsertrix --collection us-govarchive --home ~/webarchive

# a single crawl
indice import browsertrix --crawl <item-id> --home ~/webarchive
```

## Notes

- **QA'd crawls only, by default.** Browsertrix lets a reviewer rate a crawl (`reviewStatus`); indice imports only reviewed crawls so you publish vetted content. Add `--include-unreviewed` to import everything, or `--min-review <1-5>` for a rating threshold. A single named `--crawl` is always imported. When crawls are skipped for this reason, indice says so.
- **Selection.** `--collection <ID|SLUG|NAME>` limits to one Browsertrix collection; `--crawl <ID>` to a single archived item; neither imports the whole org. `--org <SLUG>` picks the org when your account has more than one.
- **Incremental.** Re-running skips crawls already imported (matched by content hash), so syncing an account is cheap; `--force` re-imports anyway.
- **Durable by default.** WACZs are downloaded into `<home>/archive/<item-id>/` (a subfolder per Browsertrix item, so items can't clash on a shared filename), because Browsertrix's presigned URLs expire after ~48h — a downloaded copy keeps replay working long-term. `--host <URL>` targets a self-hosted Browsertrix (default is `https://app.browsertrix.com`).
- **`--stream` (index-only footprint).** Instead of downloading, index the WACZ in place from Browsertrix and store only its stable identity, not a copy. Since presigned URLs expire, indice re-resolves a fresh one on demand — so **`serve` needs the same `BROWSERTRIX_*` credentials** to replay these crawls (they show a 503 otherwise). Good for a self-hoster who wants search over their own crawls without keeping the bytes; download (the default) is better for a durable, offline, or shared library.
- **Grouping.** Importing a `--collection` groups its crawls into an indice collection of the same name. `--into <NAME>` overrides that name (and is the way to group an org-wide or single-`--crawl` import, which otherwise land as individual collections). `--limit <N>` caps how many are imported; `--dry-run` lists them without downloading.

Both importers are also available in [management mode](/docs/guides/manage/) as browse-and-import wizards on the accession desk, using the server's configured credentials.
