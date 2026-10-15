# Security

indice is alpha software (0.2.0). There is no stable release, no supported
release line, and no deployment I know of. Fixes land on `main`, and there is
nothing behind it to backport them to.

If you *are* running indice somewhere, please tell me, in an issue or privately.
It changes how I weigh everything below.

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

Filed, documented, and not news. The tracker is in this repo, so
`grep <id> .beads/issues.jsonl` gets you the whole writeup.

- Every capture a browser replays shares the replay origin, so a hostile
  capture's script can read the others replayed in that browser.
  See *Not defended* in the threat model.
- `datapackage.json`, `pages/pages.jsonl` and the CDX are read whole with no
  size cap, so a WACZ can be a decompression bomb.
  `rustyweb-wacz-read-caps-q39p`
- Upload size and total archive size are limited only when `uploads.max_mb`
  and `uploads.max_total_gb` are set in `config.yaml`; both are unset by default.
  The limits apply to the whole archive, not to each curator.
  `rustyweb-upload-quota-hryt`
- A failed upload leaves its file in `archive/`. It counts against
  `uploads.max_total_gb` but appears nowhere in the workroom, so only someone
  with a shell can remove it. `rustyweb-failed-ingest-leaves-file-d1dw`
- Error responses carry server internals, including filesystem paths.
  `rustyweb-open-web-hardening-u9y6.5`
- No crawler or scraper policy. `rustyweb-open-web-hardening-u9y6.7`
- Request logs record client IP and full query strings.
  `rustyweb-open-web-hardening-u9y6.8`

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
  `ssh -L`, a `tailscale serve`. indice does not try to narrow that by
  inspecting `Host`, because header inspection cannot authenticate a caller.
  Sharing an archive is the server shape.
- **A crowd from many addresses can make a server answer `503` to everyone.**
  Each client is limited, and global caps keep the process alive, but a crowd
  large enough to exhaust those caps degrades the site for everyone until it
  passes. Telling that crowd from real readers is a CDN's or a WAF's job.
- **`/files/{id}` sends `access-control-allow-origin: *` on purpose,** so any
  page can replay a public archive. It never sends credentials with it.
- **Anything with write access to the home directory owns the instance.** The
  roster governs users of the web interface, not users of the host.

## Disclosure

Coordinated, and the bar is deliberately low while nobody is running this: for
most things I will just file the issue publicly and fix it in the open, because
that is how the rest of the project works and there is no deployed population to
protect in the meantime. Tell me if you would rather wait, and we will wait.

Happy to credit you by whatever name you prefer, or not at all.
