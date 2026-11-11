# Security

indice is alpha software (0.6.2). There is no stable release, no supported
release line, and no deployment I know of. Fixes land on `main`, and there is
nothing behind it to backport them to.

If you *are* running indice somewhere, please tell me, in an issue or privately.
It changes how I weigh everything below.

indice has had no independent security review. It has fuzz targets in
`fuzz/`, which CI runs for an hour each week and for a minute on a pull
request that changes code they reach: for the WARC and WACZ parsers, the CDX
index included, whose two findings are fixed in 0.6.1
([GHSA-p9fc-fmpp-x86j](https://github.com/edsu/indice/security/advisories/GHSA-p9fc-fmpp-x86j)),
and for what anyone can send a server: search queries, `Range` headers, and
the curator Markdown every visitor's browser renders, whose findings (memory
held for every search, and two overflows) are fixed in 0.6.2. HTML text
extraction, the page lists and thumbnail images are fuzzed too, and an image
of a few kilobytes that decoded to over a gigabyte is fixed in 0.6.2. PDF
text extraction is not fuzzed. The rest of its testing is the test suite,
which pins each guarantee in the threat model, CodeQL and a RustSec advisory
check on every change, and AI-assisted review.

## Reporting

Use GitHub's private vulnerability reporting:
[**Report a vulnerability**](https://github.com/edsu/indice/security/advisories/new).
It is enabled here, so the report stays between us until we both want it public.
Read *Already known* first; most of what is currently wrong is already written
down.

Useful in a report, roughly in order of how much it helps:

- which shape it applies to, workstation or server (see
  [*Threat Model*](DESIGN.md#threat-model))
- what the attacker gets, and what they needed in order to get it
- a request, a diff, or a few lines of script that show it happening

I maintain this on my own time. Expect an acknowledgement within a week and a
straight answer about whether I think it is a bug and whether I plan to fix it.
I would rather tell you "yes, and it will be a while" than leave you guessing.

## Scope

indice's threat model is written down:
[*Threat Model* in DESIGN.md](DESIGN.md#threat-model). For an operator, the
same ground is covered in
[Securing a server](https://indice.page/docs/guides/security/). It names the two
deployment shapes, the principals in each, every guarantee paired with the test
that pins it, and the things indice deliberately does not defend.

The most useful report breaks a stated guarantee, or shows the model is wrong
about who can do what. The second most useful one shows a guarantee is real but
unpinned, so it can regress without CI noticing.

### Already known

Filed, documented, and not news. Each linked issue has the whole writeup.

- Every capture a browser replays shares the replay origin, so a hostile
  capture's script can read the others replayed in that browser.
  See *Not defended* in the threat model.
- Upload size and total archive size are limited only when `uploads.max_mb`
  and `uploads.max_total_gb` are set in `config.yaml`; both are unset by default.
  The archive limit also bounds what indexing a curator's WACZ inflates to temp
  files, so without it a small hostile WACZ can fill the temp volume. The
  limits apply to the whole archive, not to each curator.
  [#232](https://github.com/edsu/indice/issues/232)

A report that one of these is worse than I have described, or reachable from a
principal I did not consider, is worth sending.

### Not a vulnerability

Design decisions, with the reasoning in
[*Threat Model*](DESIGN.md#threat-model) under *Not defended*. Reporting one of
these tells me the document needs to be clearer, which is worth knowing, but it
is not a finding.

- **indice authenticates nobody.** Login belongs to the reverse proxy, so
  password policy, MFA, session lifetime and credential stuffing are the proxy's
  problem. One header contract then covers Shibboleth, OIDC, and whatever an
  institution already runs.
- **A loopback bind promises nothing about who reaches the port.** On a
  workstation every caller is the operator: another account on the machine, an
  `ssh -L`, a `tailscale serve`. Sharing an archive is the server shape.
  A workstation answers only to a loopback `Host`, which stops a web page from
  reaching it by DNS rebinding and refuses a browser through `tailscale serve`.
  That is not access control: a client that sets its own `Host` gets through,
  and header inspection cannot authenticate a caller.
- **A crowd from many addresses can make a server answer `503` to everyone.**
  Each client is limited, and global caps keep the process alive, but a crowd
  large enough to exhaust those caps degrades the site for everyone until it
  passes. Telling that crowd from real readers is a CDN's or a WAF's job.
- **On a server, `/files/{id}` sends `access-control-allow-origin: *` on
  purpose,** so any page can replay a public archive. It never sends
  credentials with it. A workstation sends no such header, so no other site
  can read its archive.
- **Anything with write access to the home directory owns the instance.** The
  roster governs users of the web interface, not users of the host.

## Disclosure

Coordinated. A finding that someone could use against a running instance,
including the demo at app.indice.page, stays in a private advisory until the
fix is merged, and is written up publicly after that, with the test that pins
it. A hardening idea that is not an exploitable weakness today can go straight
into a public issue. Tell me if you would rather wait longer, and we will wait.

Happy to credit you by whatever name you prefer, or not at all.
