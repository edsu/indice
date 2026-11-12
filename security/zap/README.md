# ZAP scan of a running server

[OWASP ZAP](https://www.zaproxy.org/) scans indice as a server: the stack in the repository root's `compose.yaml`, with oauth2-proxy replaced by a fixed identity per role, so the scanner can be an anonymous visitor, a signed-in reader (not in `users.yaml`), a curator or an admin.

## Running it

Docker is all it needs. From the repository root:

```sh
security/zap/scan.sh baseline   # spider, then passive checks only: about 5 minutes a role
security/zap/scan.sh full       # also attacks every request it found: about 9 minutes a role
```

Each role gets a fresh archive of the test fixtures, since an active scan as a curator or admin changes it. `ROLES="curator admin"` runs only those. The image is built from the checkout first; to scan a release instead, build `indice:zap` from it and pass `BUILD=0`:

```sh
git worktree add /tmp/indice-v0.6.2 v0.6.2
docker build -t indice:zap /tmp/indice-v0.6.2
BUILD=0 security/zap/scan.sh full
```

Reports land in `security/zap/reports/` (ignored by git): `<mode>-<role>.html`, `.json` and `.md` from ZAP, and `<mode>-<role>-indice.log`, indice's log, which says why a request failed when ZAP only saw the status. `rules.conf` holds the alerts that were triaged as expected, each with its reason, so a rerun shows only what is new.

## How the roles work

`Caddyfile` here maps an `X-Zap-Role` header to an identity, and ZAP's replacer adds that header to every request. Like the real Caddyfile, it forwards an identity only with the shared secret and strips both from anything a client sent, so a forged `X-Forwarded-Email`, with or without the secret, reaches indice as anonymous. The site has one origin (`http://site`, replay on `http://replay`), so indice's `Origin` check stays on. ZAP sends no `Origin`, which indice accepts as a non-browser client, so the attacks reach the write routes.

Anyone who can reach this stack can be admin. It publishes no ports, and only containers on its own network can reach it.

## Results: 0.6.2, 2026-11-12

Both scans, all four roles. ZAP raised no alert for cross-site scripting, and none for injection or path traversal that held up. What it found, and what became of it:

| Alert | Roles | Outcome |
|---|---|---|
| 500 for deleting a crawl that is already gone or never was (Application Error Disclosure, and behind SQL Injection and Integer Overflow) | admin | **Fixed**: 404. It also recorded a deletion in the event log for a crawl that was not there; now nothing is recorded. `manage_delete_crawl_removes_it_from_index_and_disk` |
| 500 for deleting a collection that is already gone | admin | **Fixed**: 404. `deleting_a_collection_that_is_gone_is_not_found` |
| 500 on public pages read while a collection was being deleted: the finding aid went between the check that it exists and the read | anyone, during a delete | **Fixed**: a finding aid that goes between the two is a deleted collection |
| 500 for a collection name whose id is too long to be a directory | curator, admin | **Fixed**: 400. `a_collection_name_too_long_for_a_directory_is_refused` |
| 500 for the notes of a collection named in a request with an id too long to be a directory | anyone | **Fixed**: no notes, as for any collection that does not exist. `a_collection_too_long_to_exist_has_no_notes` |
| Cross-Origin-Opener-Policy missing | all | **Fixed**: `same-origin` on the site's pages |
| Cross-Origin-Embedder-Policy missing | all | **Explained**: `require-corp` would stop the viewer framing replay |
| Cross-Origin-Resource-Policy missing | all | **Explained**: replay on its own origin reads `/files/{id}`; CORS already decides who may |
| Permissions-Policy missing | all | **Explained**: the CSP allows only indice's own script, and that asks for no camera, location or the like |
| SQL Injection, Path Traversal, Source Code Disclosure, Format String (low or medium confidence) | curator, admin | **False positive**: indice has no SQL, no file include and no format strings; the differences ZAP measured were the 500s above and ordinary page content |
| Rules ignored in `rules.conf` | | **Explained**, each with its reason there |

The informational alerts (comments in scripts, cache headers, the session cookie, modern web application) need nothing.

Rerun with the fixes, the full scan raised three warnings in each role: the Cross-Origin-Resource-Policy and Permissions-Policy headers explained above, and SQL Injection, now on a 404. indice's logs from the rerun hold one server error, the annotations one in the table, found by that rerun and fixed after it.

## What it does not cover

ZAP sends what its spider finds. It did not upload a WACZ, import from Browsertrix or Archive-It, or follow a replayed page's own requests, and it knows nothing of indice's threat model. Testing against that is [#228](https://github.com/edsu/indice/issues/228).
