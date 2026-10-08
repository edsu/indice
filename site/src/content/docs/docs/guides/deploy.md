---
title: Deploy & run
description: Run indice as a server, using the container image and a Caddy + oauth2-proxy stack with automatic HTTPS and an identity provider in front.
---

indice runs in one of two shapes, and the only question that separates them is who may use the management commands.

- **Workstation.** `indice serve` on loopback. Whoever reaches the port is the operator and can do everything, with nothing to configure and no login. That is the whole story for local use; see [Install](/docs/install/) and [Try it in a minute](/docs/quickstart/).
- **Server.** Behind a TLS-terminating proxy and an identity provider. Anyone may read the archive; the people listed in `users.yaml` may change it. This page is about that shape.

No read-only server shape exists. A server you cannot write to sends you back to the command line the first time you want to fix a finding aid, and a server with an empty `users.yaml` already reads as read-only to every visitor.

## Container image

A multi-arch image (`linux/amd64` + `linux/arm64`) is published to the GitHub Container Registry on every release. It carries no default arguments, because serving on a public interface needs an authenticating proxy and indice refuses to start without one, so there is no sensible command to guess. `docker run` with no arguments prints the help.

The stack below supplies the real command. To run the container yourself, behind a proxy you already have:

```sh
docker run -p 127.0.0.1:8080:8080 -v indice-data:/data \
  -e INDICE_AUTH_PROXY_SECRET=a-long-random-string \
  ghcr.io/edsu/indice:latest \
  serve --bind 0.0.0.0:8080 --home /data --auth-proxy-header X-Forwarded-Email
```

`/data` is indice's home, holding `archive/`, `collections/`, and `index/`. Mount a volume so it survives restarts.

## The stack

[`compose.yaml`](https://github.com/edsu/indice/blob/main/compose.yaml) runs three containers: Caddy terminates TLS, [oauth2-proxy](https://oauth2-proxy.github.io/oauth2-proxy/) brokers the login against your identity provider, and indice serves the archive.

Three containers for one application deserves a word. indice authenticates nobody: it reads an identity from a request header and checks it against `users.yaml`. That is what lets one mechanism cover SAML, OIDC and anything else your institution already runs, and it is why the Shibboleth section below needs no oauth2-proxy at all. The price is that something has to turn a login into that header, and for an OIDC issuer oauth2-proxy is the smallest well-tested thing that will. It proxies nothing here, despite the name; Caddy does that, while oauth2-proxy answers one question per request and handles the sign-in and sign-out routes.

Swap it for whatever suits you. The two containers that matter are indice and something terminating TLS.

```sh
SITE_ADDRESS=archive.example.org docker compose up -d
```

Caddy passes byte-range requests through untouched, so ReplayWeb.page's ranged reads of large WACZs replay through the proxy. Named volumes persist indice's `/data` and Caddy's certificates. (`compose.yaml` builds the image from the repo by default; to pull the published image instead, follow the comment in the file.)

Load archives by indexing into the running container:

```sh
docker compose cp your.wacz indice:/data/your.wacz
docker compose exec indice indice index --collection "Your Collection" /data/your.wacz
```

## Setting it up

indice speaks to no identity provider and stores no passwords. oauth2-proxy brokers the login against yours over plain OIDC, so Google, Microsoft Entra, Okta, Auth0, Keycloak, Authentik, Dex, GitLab or any issuer with a discovery document will do.

1. Register a confidential client with your issuer, with redirect URI `https://your-domain/oauth2/callback`.
2. Put the details in a `.env` file next to `compose.yaml`:
   ```sh
   OIDC_ISSUER_URL=https://accounts.google.com
   OIDC_CLIENT_ID=...
   OIDC_CLIENT_SECRET=...
   OIDC_REDIRECT_URL=https://your-domain/oauth2/callback
   OAUTH2_PROXY_COOKIE_SECRET=      # a 32-char secret from: openssl rand -hex 16
   INDICE_AUTH_PROXY_SECRET=a-long-random-string
   # OIDC_EMAIL_DOMAINS=example.edu    # who may sign in at all; defaults to anyone
   # OAUTH2_PROXY_COOKIE_SECURE=false  # only to run the stack on plain-HTTP localhost
   ```
3. Write a [`users.yaml`](/docs/reference/configuration/#usersyaml) in indice's home saying who may do what.
4. `SITE_ADDRESS=your-domain docker compose up -d`.

Clicking **Log in** sends you to your issuer. After you authenticate you come back signed in, with the workroom chrome if `users.yaml` lists you as a curator or an admin. You may land on the homepage rather than the page you left, because the final hop carries the issuer's address as its referrer and indice will not redirect to an off-site one.

### What it asks your issuer for

`openid email`, and nothing else. indice identifies people by email address, which is what `users.yaml` lists and what it canonicalises internally, so it has no use for a profile, a photo or a group list.

Earlier versions of this stack used oauth2-proxy's GitHub integration, which asks for `read:org` whether or not you restrict anyone by organization: it calls GitHub's `/user/orgs` on every login to populate groups for apps that authorize on them. indice authorizes on `users.yaml`, so it was asking visitors for something it never read. Plain OIDC makes no calls beyond the issuer's own endpoints.

GitHub publishes no OIDC discovery document for user login, so signing in with a GitHub account now means putting a broker in front, such as [Dex](https://dexidp.io/) with its GitHub connector, which then presents OIDC to oauth2-proxy. That costs a fourth container and buys back a minimal consent screen.

## Who gets in, and who can write

Two questions, and two places to answer them.

**Your identity provider decides who may sign in.** Anyone it authenticates may, and signing in on its own grants nothing beyond public read. To narrow it, set `OIDC_EMAIL_DOMAINS` to your domain, or restrict who may use the client at the issuer.
**`users.yaml` decides who may write.** List someone as a `curator` and they can accession and describe; list them as an `admin` and they can also deaccession. Everyone else, signed in or not, is a reader. See [Who can do what](/docs/guides/manage/#who-can-do-what).

We split them so you approve a colleague by editing one line in a file you can commit and diff, instead of reconfiguring your identity provider.

## Using a different identity provider

The shipped stack is one worked example. indice's side of it is **two request headers**, so anything that can authenticate a request and set them will work:

- the authenticated identity, in a header you name with `--auth-proxy-header`. The shipped stack uses `X-Forwarded-Email`; Authelia calls it `Remote-Email`.
- a shared secret in `X-Indice-Auth-Secret`, matching `INDICE_AUTH_PROXY_SECRET`. indice trusts the identity header only on requests carrying this secret, so it refuses anything that forged the identity or skipped your proxy.

Besides oauth2-proxy, that covers [Authelia](https://www.authelia.com/), [Authentik](https://goauthentik.io/), [Pomerium](https://www.pomerium.com/), and an AWS or GCP load balancer doing OIDC.

### Shibboleth

[Shibboleth](https://www.shibboleth.net/) is the common case on campus, and it speaks SAML, so the OIDC stack above is the wrong shape for it. Use the header contract directly instead, and drop oauth2-proxy from the picture.

Run a **Shibboleth SP** in front of indice, in the usual Apache `mod_shib` or nginx arrangement. The SP handles SAML with your institution's IdP and then exposes the released attributes to the backend. Point `--auth-proxy-header` at whichever one it sets, and add the shared secret in the web server config:

Use a **lazy session**, so the SP runs on every request without forcing anyone to log in:

```apache
<Location />
    AuthType shibboleth
    ShibRequestSetting requireSession 0
    Require shibboleth

    # Never trust a copy the client sent.
    RequestHeader unset X-Forwarded-Email
    RequestHeader unset X-Indice-Auth-Secret
    # Set them only when the SP has a session, so an anonymous reader stays anonymous.
    RequestHeader set X-Forwarded-Email %{mail}e env=mail
    RequestHeader set X-Indice-Auth-Secret "your-shared-secret" env=mail

    ProxyPass http://127.0.0.1:8080/
</Location>

# The one path that forces a login. indice's Log in link points here.
<Location /manage/login>
    ShibRequestSetting requireSession 1
    Require valid-user
</Location>
```

`requireSession 0` plus `Require shibboleth` is the SP's lazy-session mode: it does not send anyone to the IdP, and it fills in attributes whenever a session already exists. That gives indice what it wants, an identity on every request from someone signed in and nothing from someone who is not, so the workroom chrome appears on ordinary pages rather than only under `/manage`. Protecting the whole site with `requireSession 1` instead would force a campus login on every anonymous visitor and make the archive private, which is the subject of the last section rather than this one.

Three things to get right:

**Clear the headers before setting them.** With a lazy session there is no attribute to read when nobody is signed in, and depending on the Apache version `RequestHeader set ... %{mail}e` either skips the directive, leaving a client-supplied value in place, or writes the literal `(null)`. The `unset` lines above close that, and `env=mail` sets both headers only when the attribute exists. Adjust `mail` to whichever env var your `attribute-map.xml` produces.


**Choose the attribute with care.** A typical attribute release includes `eduPersonPrincipalName` (`alice@example.edu`) and `mail`. Both look like addresses, so indice canonicalises either one, but they are not the same thing: `mail` is a mailbox and changes when someone changes their address, while eppn is the campus identifier. eppn is the steadier choice for a roster. Some institutions reassign eppn after a person leaves, which is what `eduPersonTargetedID` and `pairwise-id` exist to avoid, though those are opaque strings nobody wants to type into `users.yaml` by hand. Whichever you choose, `aliases` covers the day an identity changes.

**Set the secret in the server config, not from an attribute.** It is a static value proving the request came through your SP, so it must not be anything the browser can influence.

If your IdP runs the [OIDC OP plugin](https://shibboleth.atlassian.net/wiki/spaces/IDPPLUGINS/pages/1376878976/OIDC+OP), officially supported since IdP 4.1 and still maintained, you can skip all of this and point the stack above at it like any other issuer. A SAML-to-OIDC broker such as [SATOSA](https://github.com/IdentityPython/SATOSA) or Keycloak is the third option, and worth it only if you want OIDC for other reasons too.

Two requirements of whatever you put in front:

- **Forward the identity on every request, including the ordinary pages.** indice draws the workroom chrome on the homepage and the collection pages for a signed-in curator, so it needs to know who you are there too. The shipped `Caddyfile` asks oauth2-proxy on each request and forwards the answer when there is one.
- **Strip any client-supplied copy of both headers**, and set the secret only on requests your proxy authenticated.

## Using a different web server

Caddy is doing four separable jobs here, and none of them is Caddy-specific. Anything that can do all four works:

1. **Route `/oauth2/*`** to your login service.
2. **Ask who the caller is on every request, and continue anonymously when the answer is nobody.** This is the one people get wrong. Gating only `/manage` and the write APIs looks right and costs you the workroom chrome on ordinary pages, because indice decides whether to draw the edit controls from the forwarded identity.
3. **Inject the identity and the shared secret together**, only on a request your proxy authenticated, replacing any copy the client sent.
4. **Leave byte ranges alone.** No gzip on a ranged response, no buffering of large bodies, or WACZ replay breaks.

Add one indice-side detail: if your proxy rewrites `Host` without setting `X-Forwarded-Host`, pass `--site-url` so the cross-site check knows the address the browser used. nginx and Apache both rewrite it by default.

**nginx** does this with `auth_request`, which is the same subrequest mechanism, plus `error_page 401 = @anonymous;` pointing at a named location that proxies to indice with no identity headers. That named location is the equivalent of the shipped `Caddyfile`'s `handle_errors` block. Check `proxy_buffering` against a large ranged read while you are there.

**Apache with [`mod_auth_openidc`](https://github.com/OpenIDC/mod_auth_openidc)** speaks OIDC itself, so it replaces Caddy *and* oauth2-proxy, leaving two containers. It is the same arrangement as the Shibboleth section above: the web server authenticates and sets headers, indice reads them.

**Traefik** has a ForwardAuth middleware that fits job 2.

One arrangement to avoid: running oauth2-proxy as the reverse proxy itself, with `OAUTH2_PROXY_UPSTREAMS` pointed at indice and no Caddy. It is tempting because it drops a container, but in that mode it gates everything by default, so you have to list the public routes in `--skip-auth-regex`. That is a second copy of indice's route policy living somewhere no test can see it, which is the exact arrangement that broke the public annotations API before.

:::caution[These are sketches, not tested configs]
The shipped `Caddyfile` is exercised by [`scripts/smoke-deploy.sh`](https://github.com/edsu/indice/blob/main/scripts/smoke-deploy.sh). The alternatives above are not. Proxy configuration fails quietly: Caddy's documented `handle_errors 502 503 504` form, for one, parses and validates and then never matches, so the fallback it describes silently never runs. Test your own against the same checks that script makes.
:::


## Keeping an instance private

The server shape serves reads to anyone. If your archive holds in-copyright or embargoed material that should stay off the open web, you can gate reads at the proxy as well. indice does not mind: the identity header then arrives on read requests too, and the roster still decides who may write. Nothing changes inside indice.

The shipped `Caddyfile` needs two changes first, and both are easy to miss because the site looks private once the homepage asks for a login.

**Remove the `@bytes` bypass.** That block sends `/files/*`, `/thumb/*`, `/collection-thumb/*`, `/replay/*` and `/assets/*` straight to indice with no auth subrequest, because identity cannot change those responses and replay issues a lot of them. On a public archive that is a performance win. On a private one it is the hole: `/files/{id}` returns the whole WACZ and `/thumb/{id}` returns page images, so the material you are protecting is served to anyone who has an id, while the pages that merely describe it are behind a login. Delete the block and let those routes go through the catch-all.

**Replace the `handle_errors` fallback with a failure.** It exists so a dead login service does not take a public archive offline, which means it serves every request anonymously. On a private archive that turns an oauth2-proxy outage into an open door. Swap the body for `respond "authentication unavailable" 503`.

Once reads are gated, `OIDC_EMAIL_DOMAINS` becomes load-bearing rather than cosmetic: it is what stops anyone with an account at your issuer from reading the archive.
