#!/usr/bin/env bash
#
# Smoke-test the shipped server stack (compose.yaml + Caddyfile).
#
#   ./scripts/smoke-deploy.sh            # build, run the checks, tear down
#   PORT=9100 ./scripts/smoke-deploy.sh  # if 8099 is taken
#   KEEP=1 ./scripts/smoke-deploy.sh     # leave the stack up to poke at it
#
# The deployment story lives in a Caddyfile and a compose file, which `cargo
# test` cannot see. That blind spot let the proxy gate `GET /api/annotations`,
# a public route, which broke the annotations panel for logged-out visitors. The
# fetch failed without logging anything, so it went unnoticed.
#
# Two phases. Phase one runs the real oauth2-proxy image against a static OIDC
# discovery document, so it boots with the shipped configuration and answers
# "not logged in" to everything, covering the anonymous surface that most
# visitors see. Phase two swaps in a stub reporting a signed-in user, the only
# way to exercise the authenticated path without a working identity provider.
#
# Neither phase completes an OAuth handshake: the redirect to the issuer, the
# consent screen, the callback. That needs a real issuer and a person to click
# through, and it is where the last bug lived, so a green run here does not mean
# login works. See docs/guides/deploy.md for the manual steps.
#
# Runs under its own compose project name and its own volumes, so it leaves any
# stack you already have running alone.
set -uo pipefail

PORT="${PORT:-8099}"
PROJECT=indice-smoke
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
B="http://localhost:${PORT}"
SECRET="smoke-secret-$RANDOM$RANDOM"

pass=0
fail=0

cleanup() {
	if [ "${KEEP:-}" = "1" ]; then
		echo
		echo "KEEP=1, leaving the stack up at ${B}"
		echo "  NOTE: phase two left a stub identity service in place. It reports every"
		echo "        caller as signed in, and with no users.yaml that means admin. Bound"
		echo "        to loopback, but do not mistake it for a working login."
		echo "  tear down: docker compose -p ${PROJECT} --env-file ${TMP}/env -f ${ROOT}/compose.yaml -f ${TMP}/port.yaml down -v"
		return
	fi
	echo
	echo "tearing down…"
	dc down -v >/dev/null 2>&1
	rm -rf "$TMP"
}
trap cleanup EXIT

dc() {
	docker compose -p "$PROJECT" --env-file "$TMP/env" \
		-f "$ROOT/compose.yaml" -f "$TMP/port.yaml" "$@"
}

chk() { # chk <want> <got> <description>
	if [ "$2" = "$1" ]; then
		printf '   ok   %s\n' "$3"
		pass=$((pass + 1))
	else
		printf '   FAIL %s: wanted %s, got %s\n' "$3" "$1" "$2"
		fail=$((fail + 1))
	fi
}

code() { curl -s -o /dev/null -w '%{http_code}' --max-time 15 "$@"; }

cat >"$TMP/env" <<EOF
SITE_ADDRESS=:${PORT}
INDICE_AUTH_PROXY_SECRET=${SECRET}
OIDC_ISSUER_URL=http://oidc-stub:5556
OIDC_CLIENT_ID=indice-smoke
OIDC_CLIENT_SECRET=smoke-dummy
OIDC_REDIRECT_URL=http://localhost:${PORT}/oauth2/callback
OAUTH2_PROXY_COOKIE_SECRET=0123456789abcdef0123456789abcdef
OAUTH2_PROXY_COOKIE_SECURE=false
EOF

# oauth2-proxy fetches its issuer's discovery document at startup and exits if
# it cannot, so the real image needs something to talk to. Two static JSON files
# are enough to get it running and answering "not logged in", which is all phase
# one asks of it. Serve them from files rather than Caddy's `respond`, because
# Caddy reads the `{` in JSON as a placeholder delimiter.
mkdir -p "$TMP/oidc/www/.well-known"
cat >"$TMP/oidc/www/.well-known/openid-configuration" <<'EOF'
{"issuer":"http://oidc-stub:5556","authorization_endpoint":"http://oidc-stub:5556/auth","token_endpoint":"http://oidc-stub:5556/token","jwks_uri":"http://oidc-stub:5556/keys","userinfo_endpoint":"http://oidc-stub:5556/userinfo","response_types_supported":["code"],"subject_types_supported":["public"],"id_token_signing_alg_values_supported":["RS256"],"scopes_supported":["openid","email","profile"]}
EOF
printf '{"keys":[]}' >"$TMP/oidc/www/keys"
cat >"$TMP/oidc/Caddyfile" <<'EOF'
:5556 {
	root * /srv
	file_server
	header Content-Type application/json
}
EOF

# Caddy listens on $PORT inside the container (SITE_ADDRESS), so publish that
# rather than the base file's 80/443.
cat >"$TMP/port.yaml" <<EOF
services:
  caddy:
    ports: !override
      # Loopback only. Phase two swaps in a stub that reports everyone as signed
      # in, and with no users.yaml everyone signed in is an admin, so a KEEP=1 run
      # would otherwise publish an open write surface on every interface.
      - "127.0.0.1:${PORT}:${PORT}"
  oidc-stub:
    image: caddy:2
    volumes:
      - $TMP/oidc/Caddyfile:/etc/caddy/Caddyfile:ro
      - $TMP/oidc/www:/srv:ro
  oauth2-proxy:
    depends_on:
      oidc-stub:
        condition: service_started
EOF

echo "building and starting the stack on ${B} (first run compiles indice, so be patient)…"
dc up -d --build >/dev/null 2>&1 || {
	echo "FAILED to start the stack. Output:"
	dc up -d --build 2>&1 | tail -30
	exit 1
}

# Seed one crawl so the replay byte checks hit bytes instead of a 404.
dc cp "$ROOT/apod.wacz" indice:/data/apod.wacz >/dev/null 2>&1
dc exec -T indice indice index --collection APOD --home /data /data/apod.wacz >/dev/null 2>&1
WACZ=$(curl -s --max-time 15 "$B/collection/apod/replay.json" |
	python3 -c 'import json,sys; print(json.load(sys.stdin)["resources"][0]["path"])' 2>/dev/null)
if [ -z "${WACZ:-}" ]; then
	echo "FAILED to seed a crawl; the replay checks cannot run."
	exit 1
fi

echo
echo "── the identity service is up (real oauth2-proxy, nobody logged in) ──"
chk 200 "$(code "$B/")" "an anonymous visitor gets the homepage, not a login redirect"
chk 200 "$(code "$B/api/search?q=apod")" "public search"
chk 200 "$(code "$B/api/annotations?collection=apod")" "public annotations API is NOT behind the login"
chk 200 "$(code "$B/collection/apod")" "public collection page"
chk 200 "$(code "$B/replay/viewer")" "replay viewer"
chk 302 "$(code "$B/manage/login")" "/manage/login starts a login"

echo
echo "── a forged identity must not be believed ──"
chk 403 "$(code -X POST -H 'X-Forwarded-Email: attacker@evil.test' \
	-H 'Content-Type: application/json' -d '{}' "$B/api/collections")" \
	"forged identity, no secret"
chk 403 "$(code -X POST -H 'X-Forwarded-Email: attacker@evil.test' \
	-H "X-Indice-Auth-Secret: ${SECRET}" \
	-H 'Content-Type: application/json' -d '{}' "$B/api/collections")" \
	"forged identity AND the real secret (Caddy must replace both)"

echo
echo "── replay bytes survive the proxy ──"
chk 206 "$(code -H 'Range: bytes=10-19' "$B$WACZ")" "ranged WACZ read"
hdrs=$(curl -s -D- -o /dev/null --max-time 15 -H 'Range: bytes=10-19' "$B$WACZ")
chk 1 "$(grep -ci '^content-range:' <<<"$hdrs")" "Content-Range present"
chk 0 "$(grep -ci '^content-encoding:' <<<"$hdrs")" "not gzipped (gzip on a range breaks replay)"
chk 1 "$(grep -ci 'access-control-expose-headers:.*content-range' <<<"$hdrs")" "range headers exposed to the browser"

echo
echo "── the identity service is down: reads must survive ──"
dc stop oauth2-proxy >/dev/null 2>&1
sleep 1
chk 200 "$(code "$B/")" "homepage degrades to anonymous rather than 502"
chk 200 "$(code "$B/api/search?q=apod")" "search degrades to anonymous"
chk 206 "$(code -H 'Range: bytes=10-19' "$B$WACZ")" "replay bytes unaffected"
chk 403 "$(code -X POST -H 'X-Forwarded-Email: attacker@evil.test' \
	-H "X-Indice-Auth-Secret: ${SECRET}" \
	-H 'Content-Type: application/json' -d '{}' "$B/api/collections")" \
	"writes still refused while degraded (fail closed, not open)"
dc start oauth2-proxy >/dev/null 2>&1

echo
echo "── phase two: a stubbed identity service says we are signed in ──"
# Driving the real oauth2-proxy needs a real OAuth app, so swap it for a stub
# that reports an authenticated user. This checks that the forwarded identity
# reaches indice on ordinary public pages, which the signed display cookie used
# to handle.
mkdir -p "$TMP/stub"
cat >"$TMP/stub/Caddyfile" <<'STUB'
:4180 {
	handle /oauth2/auth {
		header X-Auth-Request-Email "smoketester@example.test"
		respond "" 202
	}
	handle {
		respond "stub" 200
	}
}
STUB
cat >"$TMP/stub.yaml" <<EOF
services:
  oauth2-proxy:
    image: caddy:2
    environment: !override
      STUB: "1"
    volumes:
      - $TMP/stub/Caddyfile:/etc/caddy/Caddyfile:ro
EOF
docker compose -p "$PROJECT" --env-file "$TMP/env" \
	-f "$ROOT/compose.yaml" -f "$TMP/port.yaml" -f "$TMP/stub.yaml" \
	up -d --force-recreate oauth2-proxy >/dev/null 2>&1
sleep 2

home=$(curl -s --max-time 15 "$B/")
chk 1 "$(grep -c 'signed in as' <<<"$home")" "a signed-in identity reaches the PUBLIC homepage"
chk 200 "$(code "$B/manage/add")" "the accession desk opens for a signed-in user"
chk 303 "$(code -H "Referer: $B/collection/apod" "$B/manage/login")" "/manage/login bounces back to where you came from"

echo
if [ "$fail" -eq 0 ]; then
	echo "   ${pass} passed"
else
	echo "   ${pass} passed, ${fail} FAILED"
fi
exit $((fail > 0))
