---
title: Drafting descriptions
description: Ask a language model for a first draft of a collection's finding aid, as a starting point to review and edit, using any OpenAI-compatible service, hosted or local.
---

A finding aid for a crawl of thousands of pages takes time to write, and a blank one is a hard place to start. indice can ask a language model for a first draft of a collection's description, built from what indice already knows about it. The draft is a starting point: you review it, correct it, and decide what to keep. It never replaces your description, and it never fills in a field you've already written.

The feature is off unless you configure a model, and it runs only when you ask.

## What it drafts, and what it leaves to you

A draft covers the collection's:

- **description**, a one- or two-sentence abstract;
- **narrative**, a scope and content note;
- **subjects**;
- **dates**, a coverage statement.

It never drafts the **creator**, **curator** or **rights**. Those are facts about who made the records, who holds them and how they may be used, which a model could only guess at from the pages.

## What the model is shown

indice sends what it already knows, without fetching any WACZ again:

- the collection's name, and any fields you've already written, so the draft fits them;
- each crawl's provenance: when it was captured, how many pages, the crawler, and its seed pages;
- how many pages come from each site, year, file type and language;
- a sample of pages: the seed pages first, then pages spread across the collection's sites, each with its title, description and the start of the text indice stored when it indexed them.

A site's menu, which opens nearly every page, is sent once at most, so the sample spends its room on content. The whole message stays within a size you set (`max_input_chars`, 24,000 characters by default).

:::caution
That text goes to the service you configure. With a hosted service, archived pages leave your machine and are processed under that service's terms. With a local model they stay on your machine. Choose with your collections' sensitivity in mind.
:::

Archived pages are treated as material, not instructions: each is fenced off in the message, and the model is told to ignore any instructions a page contains. Whatever comes back, indice keeps only the four fields above, and only as a draft for you to review.

## Setting it up

Any service that speaks the OpenAI chat completions API will do. That includes hosted services and local ones such as [Ollama](https://ollama.com), llama.cpp and vLLM.

**With a local model.** Install Ollama, start it, and pull a model:

```sh
ollama serve                 # in its own terminal
ollama pull llama3.1:8b
```

Then add to your home's `config.yaml`:

```yaml
llm:
  endpoint: http://localhost:11434/v1
  model: llama3.1:8b
```

Say something about the service for curators to read before they send it text. It's optional, but it's the place to explain who runs the model and what happens to what's sent:

```yaml
llm:
  endpoint: http://localhost:11434/v1
  model: llama3.1:8b
  notice: Runs on this machine; archived text never leaves it.
```

**With a hosted service,** use its OpenAI-compatible base URL and a model it offers, and put its API key in the environment, never in `config.yaml`:

```sh
export INDICE_LLM_API_KEY=...
```

`indice config` shows the model and service in force. The [configuration reference](/docs/reference/configuration/#drafting-descriptions-with-a-language-model) lists every setting, including how long to wait for an answer.

## Drafting a description

See what would be sent first. This works before any model is configured:

```sh
indice collection draft "Climate.gov" --show-prompt
```

Then ask for a draft:

```sh
indice collection draft "Climate.gov"
```

It prints the draft as YAML, headed by the model and service that wrote it, and changes nothing. It shows every field the model drafted, including ones you've already written, so you can compare.

When you're happy with it, add `--apply`. That fills the fields that are still **empty** and leaves everything you've written as it is. To have the draft replace a field you've already written, a placeholder narrative say, name it:

```sh
indice collection draft "Climate.gov" --apply --replace narrative
```

`--replace` takes `description`, `narrative`, `subjects` or `dates`, repeated or separated by commas, and overwrites only those. Then open the finding aid, in the [workroom](/docs/guides/manage/) or in `collections/<slug>/README.md`, and edit the draft into your own description.

## In the editor

When a model is configured, a collection's **Edit collection** form has a **Draft a description** button beside **Save changes**. Below it, before you send anything, you can see:

- which model, at which service, it will use;
- what your operator says about that service, if they've written a note: who runs it, what it keeps, and a link to its policy;
- **What will be sent**, which opens to list the pages in the sample and the full prompt, and says how many characters that comes to.

The button sends the form as it stands, so text you've typed but not saved counts as yours. While the model works, the buttons are disabled and a line counts the seconds, up to the time limit.

The form comes back with the draft in it:

- **Fields that were empty** now hold the drafted text, marked "drafted, review before saving".
- **Fields you had written** keep your text. The draft is shown beneath each one, with a **Use the draft instead** box to tick if you prefer it.

Nothing is saved until you press **Save changes**, and you can edit any of it first, or press **Draft again** for another attempt. A model can take a minute to answer. If it can't be reached, or answers with nothing usable, the form comes back with your text intact and says what went wrong.

On the collection page, a quiet line then says which fields hold drafted text, with which model, and who saved them, for example *"Description and subjects drafted with llama3.1:8b (localhost:11434), reviewed by Ed."* A draft applied with `indice collection draft --apply` has no reviewer to name, so the line leaves that part out.

Anyone who can edit a finding aid can ask for a draft. Each request is recorded in the [audit trail](/docs/reference/home-directory/#events-the-audit-trail), with the model and service, since it sends archived text to that service.

## Provenance

When fields come from a draft, the finding aid records it in its front matter: which model, which service (the endpoint's host, never the key), which fields and when. The collection page shows it, as above. A reader can then tell a description that is a machine draft from one a curator wrote, in the same spirit as the crawl provenance indice records.

The record follows the text. Edit a drafted field before or after you save it and it's yours: it leaves the record, and the collection page stops calling it drafted. Fields you leave as the model wrote them stay in the record, across later saves and later drafts. When no drafted field is left, the record goes. Press **Draft again** and fields still holding the earlier draft's text are drafted afresh, not treated as yours.

```yaml
drafted:
  model: llama3.1:8b
  service: localhost:11434
  fields: [description, subjects, narrative]
  at: 2026-11-01T14:02:11Z
```

## Writing your own prompt

indice's built-in prompt asks for neutral, third-person archival prose in the manner of a DACS finding aid, using only what the material shows. To change the voice or the emphasis, write your own and point to it:

```yaml
describe:
  prompt_file: prompts/describe.md   # relative to the home
```

`--show-prompt` prints the message your prompt will be paired with. However you phrase it, indice still asks for JSON and still accepts only the four descriptive fields.
