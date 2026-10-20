---
title: Securing a server
description: What a public indice server defends against, how, and the settings an operator has to get right.
---

This page is about the **server** shape: indice behind an authenticating proxy, reachable by anyone on the web. [Deploy & run](/docs/guides/deploy/) says how to set one up; this page says what that setup protects, and why each piece is there. The full argument, with the test that pins each guarantee, is the [threat model](https://github.com/edsu/indice/blob/main/DESIGN.md#threat-model) in DESIGN.md.

A workstation is a different promise. `indice serve` on loopback keeps indice off the network and trusts whoever reaches the port, which includes every other account on the machine and anyone at the far end of an `ssh -L` or a `tailscale serve`. If anyone you do not trust can reach that port, run a server instead. See [Manage & curate](/docs/guides/manage/#on-a-workstation).

## What a server promises

- **Reading is public; changing needs a role you granted.** Anyone may search and replay. Only people listed in `users.yaml` may add, describe, annotate or delete, and only as their role allows.
- **indice checks the proxy's work.** It trusts a forwarded identity only when the proxy's shared secret arrives with it.
- **Archived pages cannot act as the reader.** Replay runs on a hostname of its own, where a capture's JavaScript can reach neither indice's API with the reader's session nor the page around it.
- **Your replay hostname shows only your archive.** It replays only this server's own WACZs, and only in indice's replay frame, never as a page of its own.
- **Other websites cannot drive a signed-in curator's browser.** Writes started by another site's page are refused, and no other site may frame indice's pages.
- **A failure does not describe the server.** A visitor whose request fails is told so, with a reference to quote; the cause, which can name files on the server, goes to the log under that reference.
- **One client cannot take the site down for everyone.** Requests are rate-limited per client and capped in total, and slow work has a time limit.

## Who can change the archive

indice authenticates nobody. Your proxy does the login, against whatever your institution runs, and passes two headers to indice on each request: who the caller is (`--auth-proxy-header`, e.g. `X-Forwarded-Email`), and a shared secret (`X-Indice-Auth-Secret`) that only the proxy knows. indice believes the identity only when the secret is there too, so a client that forges the identity header, or reaches indice without going through the proxy, is a stranger. The proxy must strip any copy of either header the client sends; the shipped `Caddyfile` does.

Being signed in grants nothing by itself. `users.yaml` decides: a `curator` may accession, describe and annotate, an `admin` may also delete. Everyone else, signed in or not, is a reader. Three properties of the roster matter on a server:

- **A server will not start without one.** With no roster indice would treat everyone your identity provider admits as an admin, so it refuses and says what to write. `users: []` is a valid roster meaning nobody may write.
- **Edits apply without a restart**, and a roster that becomes malformed or goes missing keeps the last good one, rather than falling back to "no roster".
- **Notes never show a login address.** Annotations are public, so a note shows a display name: for an email login, only the part before the `@`.

indice also sets a small signed cookie at login, so the workroom controls can be drawn on pages your proxy does not authenticate. It only decides what to *show*; a write still needs the proxy's headers.

## Archived pages run their own code

A replayed page runs the JavaScript it was captured with. That is how replay stays faithful, and it means a page someone got crawled in good faith runs its author's script in your readers' browsers.

So a server needs a second hostname for replay (`--replay-url`), and will not start without one. indice serves replay from a second listener on that hostname, and the browser keeps the two apart:

- A capture's script cannot call indice's API as the reader: its requests carry the replay hostname as their origin, and indice refuses them.
- It cannot reach into the viewer, the page with the banner and the notes panel, because that page is on the site's hostname.
- The replay listener has no write routes, no pages and no sign-in, so there is nothing on the replay hostname for it to use either.

What a capture's script *can* still do:

- **Read other captures replayed in the same browser.** Every capture shares the replay origin's storage. This matters only if one instance holds both captures from sources you do not control and material that is not public.
- **Set cookies on your parent domain, if replay is a subdomain of it.** indice's own cookie cannot be planted this way and authorizes nothing anyway, but other services on that domain may care. A separate domain for replay (say `example-replay.org`) avoids this; see [Replay gets a hostname of its own](/docs/guides/deploy/#replay-gets-a-hostname-of-its-own).
- **Suggest, but not save, a note.** The frame tells the viewer where the reader is and what they selected. Script in the frame can send those messages too, so the viewer only uses them to move the banner or pre-fill a note; saving takes a click, after the note composer has shown the page URL and the quoted passage.

### Only your own archives

ReplayWeb.page, the player, will replay whatever WACZ its page's address names. Left alone, that would let anyone show a WACZ of their own making under your replay hostname, with your institution's name on it. indice serves a small guard ahead of the player, in the same file, so every page that starts the player checks first: the WACZ must be one of this server's own, by its path, and the player must be running in indice's replay frame. Anything else, a link naming a WACZ from elsewhere, or the player's page opened on its own, shows a refusal instead. Which pages may embed that frame is the next section's question. The check runs in the browser because, once a reader has replayed something, the player's service worker answers some of those pages itself without asking the server.

## Requests from other websites

**Cross-site writes.** A web page on another site can make your browser send a request to indice, and your browser would attach your login to it. indice refuses any write whose `Origin` header is not the site's own address, which on a server is `--site-url`. A page cannot forge that header: the browser sets it, and page script is not allowed to. A client outside a browser can send any `Origin` it likes, but it is then sending its own request with its own credentials, which a forged header does not improve. A write with no `Origin` at all is accepted, since browsers always send one, so its absence means a script or `curl` rather than someone's browser.

**Clickjacking.** A hostile page can load indice's workroom in an invisible frame and line up a decoy over it, so that a curator's click lands on Delete. The cross-site check cannot help, because that click is on indice's own page. Instead indice tells browsers that nobody may frame its pages at all (`frame-ancestors 'none'`).

**Framing replay.** The replay hostname may be framed only by the site and by itself, and opened on its own it refuses to replay.

## What indice tells the browser

| Header | Site | Replay host |
|---|---|---|
| `Content-Security-Policy` | Script, style and images from the site only; may frame only the replay host; framed by nobody | Framed only by itself and the site; no script policy, since running archived script is replay's job |
| `X-Content-Type-Options: nosniff` | Yes | Yes |
| `Referrer-Policy: same-origin` | Yes | Yes |
| `Strict-Transport-Security` | Set by the proxy | Set by the proxy |

`nosniff` matters because `/files/{id}` serves archived bytes, and a browser that guesses a file's type can be talked into running it as a page. The referrer policy keeps an archive's URLs, which say what someone searched for and which page they are reading, out of the logs of every site a reader follows a link to. HSTS belongs to the proxy, which is what terminates TLS; the shipped `Caddyfile` sets it on both hostnames for a year.

## What `--site-url` does

The site's public URL, the one readers type, e.g. `https://archive.example.org`. A server will not start without it, because three protections depend on it:

1. **The cross-site check compares against it.** A write is accepted only when its `Origin` is this address. indice does not work its address out from `Host` or `X-Forwarded-Host`, because each proxy rewrites those differently and indice cannot tell which it is behind.
2. **The replay host refuses to be framed by other sites.** Its `Content-Security-Policy` names this site as the only one that may embed it.
3. **indice checks that replay is on a different host.** A different port on the same host is a different origin, but cookies ignore ports, so indice refuses to start with `--replay-url` on `--site-url`'s host.

Give the address browsers use, scheme and all; a port only if it is not the scheme's default. It is one address: if the site also answers to other names, such as `www.` or an internal hostname, have the proxy redirect them to this one, because a write from a page at any other name is refused. If a write is refused as cross-site while you use the workroom normally, the message names the `Origin` it saw and the site address it compared with, and the two will differ. In the shipped stack it is `https://$SITE_ADDRESS`, unless you set `SITE_URL`.

## Limits

- **Uploads.** `uploads.max_mb` caps one WACZ and `uploads.max_total_gb` caps the archive, in [`config.yaml`](/docs/reference/configuration/#upload-limits). Both are **unlimited until you set them**. If your curators include people you do not know, set them.
- **Requests.** Each client gets a burst of 100 quick requests, then 20 a second, and may hold 64 archive streams open; the whole server allows 256 quick requests in flight and 512 open streams. A client past its limit is told to wait (`429`), and a server past its total answers `503` until the load passes. Curators are not rate-limited, so an institution's own scripts need a roster entry rather than a setting. See the [API reference](/docs/reference/api/) for the details.
- **Slow work.** A search query is capped at 2 KiB, and every quick request at 30 seconds.
- **What a WACZ unpacks to.** A WACZ is a zip, so a file of a few megabytes can contain gigabytes once inflated. indice reads no file inside a WACZ past `index.max_member_mb` (1 GiB by default) and refuses one that asks for more; see [Reading a WACZ](/docs/reference/configuration/#reading-a-wacz).

## When something fails

A request that fails answers `500` with *Something went wrong on the server (reference 3f9a1c2e07d4b6a1).* The reference is an id indice gives every request and puts on every line it logs while handling it, as `id=…`, so searching the log for that finds the request and what went wrong:

```sh
docker compose logs indice | grep id=3f9a1c2e07d4b6a1
```

The cause stays out of the answer because it usually names a file, such as `parsing /data/index/waczs.json`, and that tells a stranger how the server is laid out. Curators and admins are the exception: when something they started fails, such as an upload, an import or an edit, the answer says what went wrong, since they are the ones who can act on it, and still gives the reference to pass on.

## Adding archives by URL

A curator can add an archive by its URL, and indice then fetches that URL from the server. On a server it refuses a URL that resolves to the machine itself or to your internal network, so the form cannot be used to probe services behind your firewall. If your WACZs really do live on an internal host, `--allow-internal-fetch` allows it. There is no way to add an archive by a path on the server's disk over the web; that is `indice index` at the command line.

## Keeping the archive private

A server serves reads to anyone. To keep an archive off the open web, gate reads at the proxy, on the replay hostname as well as the site, since the replay host serves the WACZ bytes themselves. [Keeping an instance private](/docs/guides/deploy/#keeping-an-instance-private) has the three changes the shipped `Caddyfile` needs.

## Known gaps

These are filed and not yet fixed. The tracker is in the repository: `grep <id> .beads/issues.jsonl` shows the whole writeup.

- Request logs record client addresses and full query strings (`rustyweb-open-web-hardening-u9y6.8`).
- There is no crawler or scraper policy (`rustyweb-open-web-hardening-u9y6.7`).
- A WACZ whose WARCs are deflated is inflated to a temp file to be indexed, and that is not yet bounded, so a hostile one can fill the temp volume (`rustyweb-6vjz`).

## Checklist

- [ ] A second hostname for replay, passed as `--replay-url` (`REPLAY_ADDRESS` in the shipped stack).
- [ ] `--site-url` set to the site's public URL (`SITE_URL`); indice will not start without it.
- [ ] `users.yaml` lists the people who may change things, and no one else.
- [ ] The proxy strips any client copy of the identity and secret headers, and forwards the identity on every request.
- [ ] The shared secret is long, random, and only in the proxy's configuration and indice's environment.
- [ ] Upload limits set in `config.yaml`, unless every curator is a colleague.
- [ ] Both hostnames on HTTPS, with HSTS from the proxy.
- [ ] If the archive is not public: reads gated at the proxy, on both hostnames.

## Reporting a vulnerability

See [SECURITY.md](https://github.com/edsu/indice/blob/main/SECURITY.md) for how to report one, and for what is already known.
