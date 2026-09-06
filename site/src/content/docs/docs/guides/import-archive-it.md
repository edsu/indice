---
title: Importing from Archive-It
description: Pull WARC crawls from an Archive-It account over WASAPI; indice builds a WACZ per crawl and indexes it.
---

If your crawls live in an [Archive-It](https://archive-it.org/) account (the Internet Archive's subscription web-archiving service), `indice import archive-it` pulls them in. Unlike Browsertrix, Archive-It serves **WARC files** (via WASAPI), not WACZ — so indice downloads each crawl's WARCs, **builds a WACZ from them** (one WACZ per crawl, the same builder as `wacz build`), and indexes it. WACZ creation is an implementation detail; you select crawls.

Credentials come from the **environment**, never the command line:

```sh
export ARCHIVEIT_USER='you@example.org'
read -rs ARCHIVEIT_PASSWORD; export ARCHIVEIT_PASSWORD   # prompts, no echo
```

Then import — preview first with `--dry-run`, then pull for real:

```sh
# every active collection in the account, into ~/webarchive
indice import archive-it --home ~/webarchive --dry-run
indice import archive-it --home ~/webarchive

# just one collection (by numeric id) → a matching indice collection
indice import archive-it --collection 18491 --home ~/webarchive

# a single crawl, grouped into a named collection
indice import archive-it --crawl 2626199 --into "Stephen Ratcliffe Papers" --home ~/webarchive
```

## Notes

- **One WACZ per crawl.** WASAPI groups WARC files by crawl (job); indice bundles each crawl's WARCs into a single WACZ, preserving per-crawl temporality and giving a clean incremental unit. `--dry-run` lists the individual WARC files.
- **Finished, non-deleted crawls only.** A crawl Archive-It marks deleted, or one that didn't finish, is skipped (indice checks the Partner API's crawl status). `--include-deleted` overrides the deletion skip.
- **Selection.** `--collection <ID>` (the numeric Archive-It collection id) limits to one collection; `--crawl <ID>` to a single crawl; `--crawl-time-after` / `--crawl-time-before` filter by crawl date. The default is every collection in the account (including inactive ones, which still hold importable crawls).
- **Incremental.** Re-running skips crawls already imported (matched by host + collection + crawl id), so syncing is cheap; `--force` re-imports anyway.
- **Durable.** Because indice downloads the WARCs and builds a local WACZ, replay works offline and needs no Archive-It credentials at replay time. Downloads are staged under `<home>` (one crawl at a time, cleaned up as it goes) so a large crawl uses your disk, not `/tmp`. `--host <URL>` targets a non-default host.
- **Grouping &amp; metadata.** Without `--into`, each Archive-It collection maps to an indice collection of the same name, and its title/description seed the finding aid. `--into <NAME>` groups everything into one named collection (and is how to reach crawls that aren't in any Archive-It collection). `--limit <N>` caps how many crawls per collection.

Both importers are also available in [management mode](/docs/guides/manage/) as browse-and-import wizards on the accession desk, using the server's configured credentials.
