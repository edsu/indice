---
title: Deploy & run
description: Run indice as a server — the container image, a one-command Caddy stack with automatic HTTPS, and management over the network (Basic auth or SSO).
---

Two options, depending on who is running indice:

- **On your laptop** you can grab a prebuilt binary and run `indice serve`. That's the whole story for local use; see [Install](/docs/install/) and [Try it in a minute](/docs/quickstart/).
- **As a server** — run the container image behind a TLS-terminating proxy. The batteries-included [`compose.yaml`](https://github.com/edsu/indice/blob/main/compose.yaml) does this in one command with [Caddy](https://caddyserver.com), which fetches and renews a Let's Encrypt certificate for you.

## Container image

A multi-arch image (`linux/amd64` + `linux/arm64`) is published to the GitHub Container Registry on every release:

```sh
docker run -p 8080:8080 -v indice-data:/data ghcr.io/edsu/indice:latest
```

`/data` is indice's home (`archive/` + `index/`) — mount a volume so it survives restarts. The image runs the read-only server by default.

## One command with Caddy (recommended)

[`compose.yaml`](https://github.com/edsu/indice/blob/main/compose.yaml) runs indice behind Caddy:

```sh
# local / dev — plain HTTP on :80
docker compose up -d

# production — set your domain and Caddy provisions HTTPS automatically
SITE_ADDRESS=archive.example.org docker compose up -d
```

Caddy passes byte-range requests straight through so ReplayWeb.page's ranged reads of large WACZs replay correctly through the proxy. Named volumes persist indice's `/data` and Caddy's certificates. (`compose.yaml` builds the image from the repo by default; to pull the published image instead, follow the comment in the file.)

Load archives by indexing into the running container:

```sh
docker compose cp your.wacz indice:/data/your.wacz
docker compose exec indice indice index --collection "Your Collection" /data/your.wacz
```

## Management over the network

The simplest management needs none of this: `indice serve --manage` on loopback (the [local case](/docs/guides/manage/#local-use)) trusts every request — it's just you on your machine, no login. `docker compose up` alone is **read-only**. To open the in-browser management surface to authenticated admins *over the network*, add one of two overlays, both built on the same [forward-auth](/docs/guides/manage/#running-as-a-service-forward-auth) mechanism:

- **Basic auth** (below) — a single admin password, nothing else to run. The quickest way to get management over the network.
- **Single sign-on** ([next section](#single-sign-on-oauth2-proxy)) — log in with GitHub / Google / OIDC via oauth2-proxy, with a real logout. The upgrade for multiple admins or existing SSO.

### Basic auth

[`compose.manage.yaml`](https://github.com/edsu/indice/blob/main/compose.manage.yaml) is an overlay that adds the write surface. It keeps the public site read-only and gates `/manage` + the write APIs behind HTTP Basic auth, forwarding the authenticated user to indice via forward-auth. Run it alongside the base file:

```sh
docker compose -f compose.yaml -f compose.manage.yaml up -d
```

It needs three values, e.g. in a `.env` file next to `compose.yaml`:

```sh
ADMIN_USER=you
ADMIN_PASSWORD_HASH='$2a$14$...'          # single-quoted — see the note below
INDICE_AUTH_PROXY_SECRET=a-long-random-string
```

Generate the password hash with Caddy:

```sh
docker run --rm caddy:2 caddy hash-password --plaintext 'yourpassword'
```

:::caution[Quote the bcrypt hash]
The bcrypt hash contains `$`, which docker compose interpolates. In a `.env` file, wrap it in **single quotes** so it's taken literally — bare and double-quoted values get mangled and Caddy then fails to start with a `base64-decoding password` error. (If you can't single-quote, double every `$` instead: `$` → `$$`.) Confirm the container got a valid 60-character hash with `docker compose exec caddy printenv ADMIN_PASSWORD_HASH`.
:::

Basic auth is a simple stopgap for one or a few admins. For real single sign-on, use the SSO overlay below instead.

### Single sign-on (oauth2-proxy)

[`compose.sso.yaml`](https://github.com/edsu/indice/blob/main/compose.sso.yaml) + [`Caddyfile.sso`](https://github.com/edsu/indice/blob/main/Caddyfile.sso) put [oauth2-proxy](https://oauth2-proxy.github.io/oauth2-proxy/) in front of the management surface, so admins log in with GitHub (or any OIDC provider) instead of a shared password. indice's forward-auth is unchanged — oauth2-proxy performs the login and Caddy forwards the identity — and you get a **real logout** (`/logout` clears indice's display cookie *and* oauth2-proxy's session).

The example uses **GitHub**, which is the easiest to try: GitHub allows `http://localhost` callback URLs, so you can test the whole flow locally.

1. Create a **GitHub OAuth App** (Settings → Developer settings → OAuth Apps → New) with **Authorization callback URL** `http://localhost/oauth2/callback` (use your `https://your-domain/oauth2/callback` for production).
2. Put its credentials in `.env`:
   ```sh
   GITHUB_CLIENT_ID=...
   GITHUB_CLIENT_SECRET=...
   OAUTH2_PROXY_COOKIE_SECRET=      # a 32-char secret from: openssl rand -hex 16
   INDICE_AUTH_PROXY_SECRET=a-long-random-string
   # GITHUB_USER=you            # allow-list a single login (defaults to edsu)
   # OAUTH2_PROXY_COOKIE_SECURE=true   # in production (HTTPS)
   # OAUTH2_PROXY_REDIRECT_URL=https://your-domain/oauth2/callback
   ```
3. Run it alongside the base file:
   ```sh
   docker compose -f compose.yaml -f compose.sso.yaml up -d
   ```

Only the allow-listed GitHub user(s) can reach management; everyone else sees the read-only site. Clicking **Log in** sends you to GitHub; after authorizing you land back where you were with the workroom chrome. To widen access beyond one user, set `OAUTH2_PROXY_GITHUB_ORG` / `OAUTH2_PROXY_GITHUB_TEAM` (or switch `OAUTH2_PROXY_PROVIDER` to Google/OIDC/etc.) — see the [oauth2-proxy docs](https://oauth2-proxy.github.io/oauth2-proxy/).
