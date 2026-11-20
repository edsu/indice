# How indice has been tested

indice has had no independent security review. Everything here was done by the project, much of it with an AI assistant, which is also what wrote the code. It is written down so that someone from outside can check it, repeat it, and look for what it missed. [#229](https://github.com/edsu/indice/issues/229) is the plan for that review, and [#223](https://github.com/edsu/indice/issues/223) the plan this work followed.

What indice promises, and the test that pins each promise, is the [threat model](../DESIGN.md#threat-model) in DESIGN. Start there: the testing below is attempts to break it.

## What was done

| | What | Against | Found | Where |
| --- | --- | --- | --- | --- |
| Fuzzing | cargo-fuzz targets for the WARC and WACZ parsers, the CDX index, the page lists, HTML text extraction, thumbnail images, search queries, `Range` headers and curator Markdown. CI runs each for an hour a week, keeping its corpus, and for a minute on a pull request that changes code it reaches | `main`, continuously | Two problems in the archive parsers, fixed in 0.6.1 ([GHSA-p9fc-fmpp-x86j](https://github.com/edsu/indice/security/advisories/GHSA-p9fc-fmpp-x86j)). Memory kept for every search, two overflows from a visitor's request, and a small image that decoded to over a gigabyte, fixed in 0.6.2 | [`fuzz/`](../fuzz/) |
| Scanning | OWASP ZAP, passive and active, as an anonymous visitor, a reader, a curator and an admin | 0.6.2 | No injection or cross-site scripting. Server errors for a few requests, fixed in 0.6.3 | [`zap/`](zap/) |
| Pen test | The threat model attacked by hand: forward auth, authorization, cross-site requests, replay isolation in Chrome, fetching by URL (redirects and DNS rebinding included), paths, stored cross-site scripting, the drafting model, presigned URLs and resource exhaustion. Behind the shipped `Caddyfile` and oauth2-proxy, with hosts and DNS answers the test controlled | 0.6.3 | Eight problems, none of them injection or cross-site scripting, fixed in 0.6.4. A ninth, found since, fixed in 0.6.5 | [`pentest/`](pentest/) |

Alongside these, on every change: the test suite, which pins each guarantee in the threat model; CodeQL; and `cargo deny`, which checks every dependency against the [RustSec advisory database](https://rustsec.org/), and again once a week.

Each directory's README says how to run it again. Each fix has a test, named in the log beside the finding, so `cargo test` keeps it fixed; a finding in the documentation was fixed in the guide.

## What was not

- **PDF text extraction is not fuzzed.** It runs in a child process with memory and time limits (`index.pdf_memory_mb` on Linux, and `index.pdf_timeout_secs`), so a bad PDF costs that child rather than the server, but the extractor itself has not been searched for crashes.
- **Some of the pen test's attacks were not run.** The log's last section lists them: import metadata from Browsertrix and Archive-It reaching the pages that show it, a real language model told what to write, a capture setting cookies for the parent domain, slow upload bodies, and checking in a browser that the content security policy stops stored markup. The Browsertrix presigned-URL checks were done by reading the code, without a Browsertrix account.
- **Nothing was tested on a deployment other than the shipped `compose.yaml` shape.** Another proxy in front of indice, or another identity provider, was not.
- **The scans and the pen test were each run once,** against the versions above. The fuzzing is the only part that runs again by itself.

## Reporting what we missed

Use GitHub's [private vulnerability reporting](https://github.com/edsu/indice/security/advisories/new). [SECURITY.md](../SECURITY.md) says what helps in a report, what is in scope, and what is already known.
