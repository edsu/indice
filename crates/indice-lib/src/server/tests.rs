use super::*;
use crate::views;
use axum::http::{HeaderMap, Method, Uri};
// Internals these tests exercise directly, now that they live in sibling modules.
use super::auth::{
    clear_session_cookie, hmac_sha256, local_redirect_target, same_site_request, sign_session,
    verify_session, CsrfPolicy,
};
use super::imports::imported_browsertrix_ids;
use super::pages::{active_filters, query_with_filter, query_without_filter};

#[test]
fn imported_browsertrix_ids_from_source_and_provenance() {
    use crate::collections::{BrowsertrixRef, Source};
    // Streamed import: the item id lives in the Browsertrix source.
    let streamed = Source::Browsertrix {
        host: "h".into(),
        org: "o".into(),
        item: "streamed-1".into(),
        resource: "a.wacz".into(),
    };
    // Downloaded import: a local file source + a BrowsertrixRef provenance.
    let downloaded = Source::File(std::path::PathBuf::from("archive/x/y.wacz"));
    let downloaded_ref = BrowsertrixRef {
        host: "h".into(),
        item_id: "downloaded-1".into(),
        resource_hash: String::new(),
        review_status: Some(4),
    };
    // A hand-indexed URL crawl contributes nothing.
    let unrelated = Source::Url("https://ex.org/w.wacz".into());

    let ids = imported_browsertrix_ids(
        [
            (&streamed, None),
            (&downloaded, Some(&downloaded_ref)),
            (&unrelated, None),
        ]
        .into_iter(),
    );

    assert!(ids.contains("streamed-1"), "detects streamed source id");
    assert!(ids.contains("downloaded-1"), "detects provenance item id");
    assert_eq!(ids.len(), 2, "the plain URL crawl adds nothing");
}

#[test]
fn hmac_sha256_matches_rfc4231_vector() {
    // RFC 4231, Test Case 2.
    let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

#[test]
fn session_cookie_roundtrips_and_rejects_tampering() {
    let secret = "shared-proxy-secret";
    let now = 1_000_000u64;
    let cookie = sign_session(secret, "ed@example.org", now + 100);

    // Valid + unexpired → the identity comes back.
    assert_eq!(
        verify_session(secret, &cookie, now).as_deref(),
        Some("ed@example.org")
    );
    // Expired → rejected.
    assert_eq!(verify_session(secret, &cookie, now + 200), None);
    // Wrong secret (forged by someone without it) → rejected.
    assert_eq!(verify_session("other-secret", &cookie, now), None);
    // Tampered signature → rejected.
    let mut bad = cookie.clone();
    bad.pop();
    bad.push(if cookie.ends_with('A') { 'B' } else { 'A' });
    assert_eq!(verify_session(secret, &bad, now), None);
    // Tampered identity (re-encode a different user, keep the old sig) → rejected.
    let forged = {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let sig = cookie.rsplit_once('|').unwrap().1;
        format!("{}|{}|{}", b64.encode("root"), now + 100, sig)
    };
    assert_eq!(verify_session(secret, &forged, now), None);
    // Garbage → None, not a panic.
    assert_eq!(verify_session(secret, "nonsense", now), None);
}

#[test]
fn clear_session_cookie_expires_the_cookie() {
    let mut res = axum::response::Response::new(axum::body::Body::empty());
    clear_session_cookie(&mut res, true);
    let sc = res
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(sc.starts_with("indice_session=;"), "{sc}");
    assert!(sc.contains("Max-Age=0") && sc.contains("HttpOnly") && sc.contains("Secure"));
}

#[test]
fn local_redirect_target_keeps_same_site_paths_only() {
    // A normal same-site Referer → its path (+query) is kept.
    assert_eq!(
        local_redirect_target("http://localhost/collection/x?y=1").as_deref(),
        Some("/collection/x?y=1")
    );
    assert_eq!(
        local_redirect_target("https://archive.example.org/crawl/abc").as_deref(),
        Some("/crawl/abc")
    );
    // Root path.
    assert_eq!(local_redirect_target("http://host/").as_deref(), Some("/"));
    // A Referer from another host still only yields a *path* (never off-site),
    // so this can't be an open redirect.
    assert_eq!(
        local_redirect_target("https://evil.example/collection/x").as_deref(),
        Some("/collection/x")
    );
    // No path component, or something we can't parse → None (caller uses "/").
    assert_eq!(local_redirect_target("http://host"), None);
    assert_eq!(local_redirect_target("not a url"), None);
    // Protocol-relative smuggling is rejected (would navigate off-site),
    // including the backslash variant some browsers normalize to `//`.
    assert_eq!(local_redirect_target("http://host//evil.example"), None);
    assert_eq!(local_redirect_target("http://host/\\evil.example"), None);
    assert_eq!(local_redirect_target("http://host/\\/evil.example"), None);
}

// ── Cross-site (CSRF) guard ─────────────────────────────────────────────────

/// Build the (method, headers, uri) triple `same_site_request` judges.
/// `hdrs` are plain `(name, value)` pairs so each case reads as the request a
/// browser would actually send.
fn csrf_case(method: &str, hdrs: &[(&str, &str)]) -> (Method, HeaderMap, Uri) {
    let mut headers = HeaderMap::new();
    for (k, v) in hdrs {
        headers.insert(
            axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            axum::http::HeaderValue::from_str(v).unwrap(),
        );
    }
    (
        Method::from_bytes(method.as_bytes()).unwrap(),
        headers,
        "/api/collections/x/delete".parse().unwrap(),
    )
}

/// Judge a request as a server whose `--site-url` has the origin `site`.
fn same_site(method: &str, hdrs: &[(&str, &str)], site: &str) -> bool {
    let (m, h, u) = csrf_case(method, hdrs);
    same_site_request(&m, &h, &u, &CsrfPolicy::Site(site.to_string()))
}

/// Judge a request as a workstation, which compares with the request's own
/// `Host` and requires it to name this machine.
fn same_site_local(method: &str, hdrs: &[(&str, &str)]) -> bool {
    let (m, h, u) = csrf_case(method, hdrs);
    same_site_request(&m, &h, &u, &CsrfPolicy::Loopback)
}

#[test]
fn same_site_request_never_gates_safe_methods() {
    // Reads change nothing, so a cross-site Origin on one is irrelevant.
    for method in ["GET", "HEAD", "OPTIONS", "TRACE"] {
        assert!(
            same_site(
                method,
                &[("origin", "https://evil.example")],
                "https://a.example"
            ),
            "{method} should never be gated"
        );
    }
}

#[test]
fn same_site_request_allows_absent_origin_for_non_browsers() {
    // No Origin at all: curl, our own ureq tests, a health checker. Browsers
    // always send Origin on a cross-origin POST, so absence means "not a
    // browser" — and a non-browser has no ambient credentials to ride.
    assert!(same_site("POST", &[], "https://a.example"));
    // Sec-Fetch-Site present but benign still passes.
    assert!(same_site(
        "POST",
        &[("sec-fetch-site", "same-origin")],
        "https://a.example"
    ));
    assert!(same_site(
        "POST",
        &[("sec-fetch-site", "none")],
        "https://a.example"
    ));
}

#[test]
fn same_site_request_rejects_foreign_origin_and_null() {
    let site = "https://a.example";
    assert!(!same_site(
        "POST",
        &[("origin", "https://evil.example")],
        site
    ));
    // A sandboxed or data: initiator sends "null" — no authority, so no match.
    assert!(!same_site("POST", &[("origin", "null")], site));
    // Same registrable domain but a different host is still a different origin.
    assert!(!same_site(
        "POST",
        &[("origin", "https://b.a.example")],
        site
    ));
    // A matching Origin passes, the host compared without regard to case.
    assert!(same_site("POST", &[("origin", "https://a.example")], site));
    assert!(same_site("POST", &[("origin", "https://A.Example")], site));
    // The scheme is part of the origin: a page on plain http, say one an
    // on-path attacker served before HSTS took hold, may not write to the
    // https site.
    assert!(!same_site("POST", &[("origin", "http://a.example")], site));
}

#[test]
fn same_site_request_falls_back_to_sec_fetch_site_without_origin() {
    // Origin absent but Fetch Metadata says another site started this.
    for fetch_site in ["cross-site", "same-site"] {
        assert!(
            !same_site(
                "POST",
                &[("sec-fetch-site", fetch_site)],
                "https://a.example"
            ),
            "sec-fetch-site: {fetch_site} should be refused"
        );
    }
}

#[test]
fn the_port_is_part_of_the_match() {
    // The loopback workroom: Origin carries the port, so Host must too.
    assert!(same_site_local(
        "POST",
        &[
            ("host", "127.0.0.1:8080"),
            ("origin", "http://127.0.0.1:8080")
        ]
    ));
    // Port mismatch is a different origin — another service on the same host.
    assert!(!same_site_local(
        "POST",
        &[
            ("host", "127.0.0.1:8080"),
            ("origin", "http://127.0.0.1:9999")
        ]
    ));
    // A server on a non-default port: its site URL says so, and so does Origin.
    assert!(same_site(
        "POST",
        &[("origin", "http://archive.internal:8080")],
        "http://archive.internal:8080"
    ));
    assert!(!same_site(
        "POST",
        &[("origin", "http://archive.internal")],
        "http://archive.internal:8080"
    ));
}

#[test]
fn a_server_compares_with_its_site_url_whatever_the_headers_say() {
    // Behind a proxy, Host is the internal upstream and may or may not come
    // with X-Forwarded-Host. Neither is consulted: a browser on the site sends
    // the site's Origin, and that is what matches.
    assert!(same_site(
        "POST",
        &[
            ("host", "indice:8080"),
            ("origin", "https://archive.example.org")
        ],
        "https://archive.example.org"
    ));
    // A request whose headers agree with each other but not with the site is
    // refused, however it got here.
    assert!(!same_site(
        "POST",
        &[
            ("host", "evil.example"),
            ("x-forwarded-host", "evil.example"),
            ("origin", "https://evil.example"),
        ],
        "https://archive.example.org"
    ));
    assert!(!same_site(
        "POST",
        &[("host", "indice:8080"), ("origin", "https://indice:8080")],
        "https://archive.example.org"
    ));
}

#[test]
fn local_mode_refuses_a_rebound_non_loopback_authority() {
    // DNS rebinding: the attacker points evil.example at 127.0.0.1, so the
    // browser sends a *self-consistent* pair — Host and Origin both name
    // evil.example — at the loopback workroom. Origin==Host alone would let
    // this through; requiring a loopback authority is what stops it.
    assert!(!same_site_local(
        "POST",
        &[
            ("host", "evil.example:8080"),
            ("origin", "http://evil.example:8080")
        ]
    ));
    // The genuine workroom, under every spelling of "this machine".
    for host in ["127.0.0.1:8080", "localhost:8080", "[::1]:8080"] {
        assert!(
            same_site_local(
                "POST",
                &[("host", host), ("origin", &format!("http://{host}"))]
            ),
            "{host} is this machine"
        );
    }
}

#[test]
fn a_workstation_refuses_when_the_request_names_no_host() {
    // An Origin we can't compare against anything is not provably same-site.
    assert!(!same_site_local(
        "POST",
        &[("origin", "http://127.0.0.1:8080")]
    ));
}

#[test]
fn appbar_offers_login_when_anonymous_under_forward_auth() {
    use maud::html;
    // Forward-auth configured, request anonymous → a "Log in" link, no name.
    let anon = views::layout(
        "t",
        &views::Chrome {
            can_login: true,
            ..Default::default()
        },
        None,
        html! {},
    )
    .into_string();
    assert!(
        anon.contains(r#"href="/manage/login""#) && anon.contains("Log in"),
        "anonymous + can_login should show a login link: {anon}"
    );
    assert!(!anon.contains("signed in as"));

    // Signed in → the identity + a logout control, and no login link.
    let authed = views::layout(
        "t",
        &views::Chrome {
            manage: true,
            signed_in: Some("ed".into()),
            can_login: false,
        },
        None,
        html! {},
    )
    .into_string();
    assert!(authed.contains("signed in as") && authed.contains("ed"));
    assert!(authed.contains("Log out"));
    assert!(!authed.contains("/manage/login"));
    // A POST form, not a link (see `server::auth::logout`). Asserted as two
    // properties rather than one literal: maud's attribute order is not the
    // behaviour under test, and coupling to it would send the next reader
    // hunting for a routing bug that does not exist.
    assert!(
        authed.contains(r#"action="/logout""#) && authed.contains(r#"method="post""#),
        "logout must be a POST form: {authed}"
    );
    assert!(
        !authed.contains(r#"href="/logout""#),
        "and must not still be a link: {authed}"
    );

    // Nobody signed in and no login to offer: neither affordance.
    let plain = views::layout("t", &views::Chrome::default(), None, html! {}).into_string();
    assert!(!plain.contains("/manage/login") && !plain.contains("signed in as"));
}

#[test]
fn capture_quality_summarizes_status_histogram() {
    use std::collections::BTreeMap;
    let mut c = BTreeMap::new();
    c.insert(200u16, 96u64);
    c.insert(301, 2);
    c.insert(404, 1);
    c.insert(504, 1);
    let s = capture_quality(&c).unwrap();
    // 2xx+3xx are "ok": (96+2)/100 = 98%.
    assert!(s.starts_with("100 captures, 98% ok"), "{s}");
    // Failing codes surfaced, most frequent first.
    assert!(s.contains("404×1") && s.contains("504×1"), "{s}");
    // Empty histogram → nothing to show.
    assert!(capture_quality(&BTreeMap::new()).is_none());
}

#[test]
fn active_filters_extracts_facet_tokens_only() {
    // Free text and non-facet `field:` tokens are ignored.
    let f = active_filters("climate type:pdf domain:example.com title:foo");
    assert_eq!(
        f,
        vec![
            ("type".to_string(), "pdf".to_string()),
            ("domain".to_string(), "example.com".to_string()),
        ]
    );
    assert!(active_filters("just some words").is_empty());
}

#[test]
fn query_with_filter_appends_once() {
    assert_eq!(
        query_with_filter("climate", "type", "pdf"),
        "climate type:pdf"
    );
    // Idempotent: already-present filter is not duplicated.
    assert_eq!(
        query_with_filter("climate type:pdf", "type", "pdf"),
        "climate type:pdf"
    );
    // Empty base query yields just the filter.
    assert_eq!(query_with_filter("  ", "year", "2021"), "year:2021");
}

#[test]
fn query_without_filter_removes_that_token() {
    assert_eq!(
        query_without_filter("climate type:pdf", "type", "pdf"),
        "climate"
    );
    // Leaves other filters and free text intact.
    assert_eq!(
        query_without_filter("climate type:pdf domain:ex.com", "type", "pdf"),
        "climate domain:ex.com"
    );
    // Removing an absent filter is a no-op (modulo whitespace normalization).
    assert_eq!(query_without_filter("climate", "type", "pdf"), "climate");
}

#[test]
fn toggling_a_filter_round_trips() {
    let q = "coral reef";
    let added = query_with_filter(q, "collection", "coralreef-gov");
    assert_eq!(added, "coral reef collection:coralreef-gov");
    assert_eq!(
        query_without_filter(&added, "collection", "coralreef-gov"),
        "coral reef"
    );
}

/// `Access::local` refuses a public bind.
///
/// This invariant is what separates the two deployment shapes, and until now
/// nothing checked it. It also used to live only in `serve_on_listeners`, so a
/// caller building a router directly walked straight past it; making it a
/// constructor means the type carries the rule.
#[test]
fn local_access_refuses_a_public_bind() {
    use crate::server::Access;

    for ok in ["127.0.0.1:8080", "127.0.0.53:80", "[::1]:8080"] {
        assert!(
            Access::local(ok.parse().unwrap()).is_ok(),
            "{ok} is loopback and should be allowed"
        );
    }
    for bad in ["0.0.0.0:8080", "192.168.1.10:8080", "[::]:8080"] {
        let err = Access::local(bad.parse().unwrap())
            .expect_err("a public bind must not get local trust");
        let msg = format!("{err}");
        assert!(
            msg.contains(bad),
            "the refusal should name the offending address, got: {msg}"
        );
    }
}

/// A request that outruns the clock answers 503, not 408.
///
/// tower-http's default is `408 Request Timeout`, which says the *client* was
/// too slow to send its request. Here the opposite happened: indice took too
/// long to answer. 503 says that, matches what saturation should return, and
/// is what a monitor will read as "this server is unwell" rather than "that
/// client is broken". The distinction is invisible until someone reads a
/// dashboard, which is exactly when it matters, so pin the choice.
///
/// This exercises the layer in isolation with a short budget. It does not
/// prove the router puts it on the right routes — [`REQUEST_TIMEOUT`] is 30
/// seconds, so no test is going to wait for the real one — and that half is
/// covered structurally instead: the timed and the streaming routes are
/// separate `Router`s, not a list of paths to keep in step.
#[tokio::test]
async fn a_timed_out_request_answers_503() {
    use tower::ServiceExt;

    let slow = Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            "finished"
        }),
    );
    let app = slow.layer(TimeoutLayer::with_status_code(
        StatusCode::SERVICE_UNAVAILABLE,
        std::time::Duration::from_millis(20),
    ));

    let res = app
        .oneshot(
            axum::http::Request::get("/slow")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Every crawl replays through its own `/files/{id}`, whatever its source,
/// because the replay origin replays nothing else (`static/replay/guard.js`).
/// A remote URL handed to the viewer directly would be refused there.
#[test]
fn every_crawl_replays_through_its_own_files_path() {
    let crawl = |source: serde_json::Value| -> crate::collections::Wacz {
        serde_json::from_value(serde_json::json!({
            "id": "abc123",
            "collection": "c",
            "source": source,
            "name": "n",
            "date_indexed": "2026-01-01T00:00:00Z",
            "file_size": 1,
            "sha256": "0",
        }))
        .unwrap()
    };
    let file = crawl(serde_json::json!("archive/c/a.wacz"));
    let remote = crawl(serde_json::json!("https://example.org/a.wacz"));
    assert!(matches!(file.source, crate::collections::Source::File(_)));
    assert!(matches!(remote.source, crate::collections::Source::Url(_)));
    assert_eq!(viewer_source(&file), "/files/abc123");
    assert_eq!(viewer_source(&remote), "/files/abc123");
}

/// A time axis covers every month in its span, so the gaps in an archive show:
/// empty months get a slot and no link, years are marked where they start, and
/// a small month is never too short to see.
#[test]
fn a_time_axis_keeps_empty_months_and_marks_years() {
    let bars = month_axis(
        &[(202105, 200), (202207, 1)],
        |ym| format!("/m/{ym}"),
        |ym| ym == 202207,
    );
    assert_eq!(bars.len(), 15, "May 2021 to July 2022 inclusive");
    assert_eq!(bars[0].label, "May 2021");
    assert_eq!(bars[0].year.as_deref(), Some("2021"));
    assert_eq!(bars[0].step, 10);
    assert_eq!(bars[0].href.as_deref(), Some("/m/202105"));
    // June 2021 to June 2022: nothing crawled, so a slot with no link.
    assert!(bars[1..14]
        .iter()
        .all(|b| b.count == 0 && b.href.is_none() && b.step == 0));
    assert_eq!(bars[8].label, "January 2022");
    assert_eq!(bars[8].year.as_deref(), Some("2022"));
    assert!(bars[1].year.is_none());
    // One page against two hundred still shows.
    assert_eq!(bars[14].step, 1);
    assert!(bars[14].active);
    assert!(month_axis(&[], |_| String::new(), |_| false).is_empty());
}
