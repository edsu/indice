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

These apply on `index` and `reindex`; use `indice stats` to see their effect on the footprint. After **upgrading** to a version that changes the index schema or these defaults, run `indice reindex` to rebuild, so an existing home picks up the new layout. indice detects an index built by an older version and tells you to reindex.

## Reading a WACZ

```yaml
index:
  # The most indice inflates of any one file inside a WACZ, in MiB. Omit for
  # the 1024 MiB default; 0 is no limit.
  max_member_mb: 1024
  # The most of any one page indice reads for its text, in MiB. Omit for the
  # 16 MiB default; 0 is no limit.
  max_extract_mb: 16
```

A WACZ is a zip, and a file inside a zip can inflate to a thousand times its stored size, so a WACZ of a few megabytes can contain gigabytes of page list or index. indice reads no single file inside a WACZ past this limit. A WACZ whose metadata, page lists or index ask for more is refused, naming the file and this setting, and is not added. A single archived page or record over it is skipped with a warning, and the rest of the crawl is indexed. This applies to `indice index` and `reindex` as well as to the browser, since it protects the machine doing the indexing whoever started it.

The default leaves room for a legitimate crawl. The largest file in one is usually `pages/extraPages.jsonl`, which holds the rendered text of every page. If a crawl you trust is refused, raise the limit to fit it. Indexing reads a WACZ a page at a time, so the memory it needs does not grow with the crawl; raising the limit only lets a single file or record be larger.

Finding a page's text costs far more memory than the page itself. HTML takes around 20 times its size to read, and a PDF's cost barely follows its size: one of 3.3 MB took 750 MB, one of 12 MB took 1 GB, and one of 33 MB took 107 MB. So indice reads no more than `max_extract_mb` of any page for its text. HTML and rendered text past it are not searchable. A PDF can't be read in part, so a larger one is indexed by its file name alone, and a warning names it; it still replays. A page over 2 MiB is also read on its own rather than alongside others, so only one large page is in memory at a time. Raise the limit only on a machine with memory to match.

On Linux, the system's memory allocator holds on to memory that reading a large page has freed, so each one can leave the process bigger. The Docker image sets `GLIBC_TUNABLES` so that memory is handed back. If you run the `indice` binary directly on a Linux machine with little memory to spare, set the same in its environment:

```sh
export GLIBC_TUNABLES=glibc.malloc.arena_max=2:glibc.malloc.trim_threshold=131072:glibc.malloc.mmap_threshold=131072
```

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
- `max_total_gb` bounds what indice keeps on disk under `<home>/archive/`, where uploads and download imports land, and `<home>/tmp/`, where an upload is staged and indexing writes its temp files. A crawl indexed from a URL is streamed in place, and a streamed Browsertrix import stores nothing, so neither adds to the archive, but a full archive accepts nothing new, these included. Anything that would take the archive past its limit is refused with `507`, together with how much room is left. A Browsertrix or Archive-It download import is checked against its own size before it downloads. The room left also bounds what indexing one crawl may write to temp files: a remote WACZ's page lists, a URL downloaded because it can't be streamed, and a WACZ whose WARCs, or whose inner WACZ, are compressed inside the zip and so have to be inflated. Past the room left, indexing stops and says where. Each temp file is removed when indexing is done with it; one a crash leaves behind keeps counting until it is deleted.

The accession desk shows both numbers, and refuses a file that won't fit before sending it.

These limits apply to the browser only. `indice index` on the command line is you at the machine, and isn't limited. When the server starts it refuses an `uploads:` section that would quietly mean "no limit": a key it doesn't recognise, a key with no value, or `0`. Leave the key out if you want no limit. The file is read at startup, so a change needs a restart.

## Site name and tagline

```yaml
site:
  # The archive's name: the header, the homepage heading, the replay viewer's
  # link home, and the end of every page title. Omit for "indice".
  name: Riverside Library Web Archive
  # What the archive holds, under the homepage heading, in Markdown. Omit for
  # "Web archive search and replay". A mailto: link can set the subject;
  # write spaces as %20.
  tagline: |
    Local news and community sites since 2019. This is a **demo**: to curate,
    email [the archivist](mailto:archivist@example.org?subject=Curator%20access).
```

The tagline is the homepage's only text from you, so it is the place to say anything a visitor should read first, such as that the archive is a demo, or that someone who signs in but isn't in [`users.yaml`](#usersyaml) can only read, and how to ask for more. A workroom page such a reader opens answers with an explanation rather than a bare refusal.

The tagline uses the same Markdown as finding aids and notes: raw HTML is shown as text, images become their alt text, and links must be `http`, `https`, `mailto` or relative. A blank value counts as unset. The file is read at startup, so a change needs a restart; `indice config` shows what is in force.

## Interface language

indice's pages are available in every language it ships with: English (`en`) and Italian (`it`). Each reader gets the first language in their browser's `Accept-Language` list that indice has, so a browser asking for Italian gets Italian on any instance, whatever you set here. `site.language` is the language for a reader whose browser asks for none that indice has, and it defaults to `en`.

```yaml
site:
  language: it
```

An unsupported value stops the server at startup with the list of supported languages. Regional tags match their language (`it-CH` reads as `it`), and a blank value counts as unset. The documented API (`/api/search`, `/api/annotations`, `/health`) and command-line output stay in English. `indice config` shows the language in force.

Translations are compiled into the binary, so adding a language is a change to indice's source: a translation file, one line listing it in `i18n::CATALOGS`, and a `language-name-<tag>` message in every catalog. [DESIGN.md](https://github.com/edsu/indice/blob/main/DESIGN.md#interface-languages) explains the steps.

## Web analytics

indice can add a web analytics service's snippet, such as Plausible, Matomo, GoatCounter or Google Analytics, to its pages. It is off unless you set `head`:

```yaml
analytics:
  # The HTML your analytics service gives you to paste into your pages.
  head: |
    <script defer data-domain="archive.example.org" src="https://plausible.io/js/script.js"></script>
  # The origins that HTML loads script from and sends to.
  origins:
    - https://plausible.io
```

The snippet goes, exactly as you write it, into the `<head>` of every page on the site: the homepage, search, collections, the workroom and the replay viewer. It never goes on the replay host, so it can't see or change the archived page in the viewer's frame, only the page around it.

indice's pages refuse script, requests and images from anywhere else (see [the security guide](/docs/guides/security/#what-indice-tells-the-browser)), so `origins` lists what the snippet needs: the host its script comes from and the host it reports to, when they differ. A full URL is fine; only its origin is used. An inline `<script>` in the snippet is allowed by its hash, which indice works out, so you can paste the snippet as your service gives it. Script in attributes, such as `onload="…"`, is still refused.

A tracker sees what its script can see, which is each page's full address. On indice that includes what a reader searched for (`/search?q=…`) and the archived address of each page they replay, which indice itself keeps out of its logs. Check what your service records and keeps, and say so to your readers.

The file is read at startup, so a change needs a restart; `indice config` shows the origins and how many inline scripts the policy allows.

## Drafting descriptions with a language model

indice can ask a language model for a first draft of a collection's description, as a starting point the curator reviews and edits; the [Drafting descriptions](/docs/guides/drafting/) guide explains it. It is off unless you configure a model:

```yaml
llm:
  # Any service that speaks the OpenAI chat completions API: a hosted one, or
  # a local one such as Ollama, llama.cpp or vLLM. The base URL, ending in /v1.
  endpoint: http://localhost:11434/v1
  model: llama3.1:8b
  # How long to wait for an answer, in seconds. Omit for 120.
  timeout_secs: 120
  # What curators should know about the service before sending it archived
  # text, in Markdown: who runs it, what it keeps, a link to its policy. Shown
  # beside the editor's Draft button and printed by `collection draft`.
  notice: |
    Runs on a GPU at the library; nothing leaves the building.
    [About our model service](https://example.org/llm)
describe:
  # The most archived text and metadata to send, in characters. Omit for 24000.
  # Lower it for a small model with a short context; it can't go below 2000.
  max_input_chars: 24000
  # A prompt of your own instead of indice's, relative to the home, sent exactly
  # as written (so it names its own language). Omit to use the built-in one,
  # which writes in site.language.
  prompt_file: prompts/describe.md
```

If the service needs an API key, put it in the `INDICE_LLM_API_KEY` environment variable, never in this file.

- **What it drafts:** the description, the narrative, subjects and a dates statement. Never the creator, curator or rights, which the model could only guess at. Applying a draft fills only fields that are empty, unless the curator names one to replace.
- **What it sends:** the collection's name and existing fields, each crawl's provenance and seed pages, counts of its sites, years, types and languages, and a sample of pages with the text the search index stored for them, up to `max_input_chars`. Text that opens most pages, such as a site's menu, is sent once at most. **That text goes to the endpoint you configure.** With a hosted service it leaves your machine; with a local model it doesn't.
- **The prompt:** `indice collection draft <name> --show-prompt` prints exactly what would be sent, which is the place to start when writing a `prompt_file`. Whatever your prompt says, indice asks for JSON and accepts only the four descriptive fields from the answer.
- **Provenance:** a finding aid records which fields hold a model's drafted text, which model and which service (the endpoint's host), in a `drafted:` entry in its front matter. A field leaves the entry when someone rewrites it.

The `notice` uses the same safe Markdown as the site tagline. `indice config` shows the model and service in force, and whether a notice is set. A mistake in either section, such as `describe:` without `llm:`, a missing endpoint or model, or a `prompt_file` that can't be read, is reported when you use it.

## `users.yaml`

This file says who may do what when indice runs behind an auth proxy. It is separate from `config.yaml` because `config.yaml` tunes the index and `users.yaml` grants privilege. If you use version control, commit it alongside your finding aids.

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
