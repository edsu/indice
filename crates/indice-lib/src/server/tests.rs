use super::*;
use crate::views;
use axum::http::{HeaderMap, Method, Uri};
// Internals these tests exercise directly, now that they live in sibling modules.
use super::auth::{
    clear_session_cookie, hmac_sha256, local_redirect_target, rebound_request, same_site_request,
    sign_session, verify_session, CsrfPolicy,
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

/// Whether the workstation guard treats a request as DNS rebinding. The
/// method is not an argument: the guard refuses reads and writes alike.
fn rebound_local(hdrs: &[(&str, &str)]) -> bool {
    let (_, h, u) = csrf_case("GET", hdrs);
    rebound_request(&h, &u)
}

#[test]
fn local_mode_refuses_a_rebound_non_loopback_authority() {
    // DNS rebinding: the attacker points evil.example at 127.0.0.1, so the
    // browser sends a *self-consistent* pair — Host and Origin both name
    // evil.example — at the loopback workroom. Origin==Host alone would let
    // this through; requiring a loopback authority is what stops it, both in
    // the rebinding guard and, on its own, in the write check.
    let pair = [
        ("host", "evil.example:8080"),
        ("origin", "http://evil.example:8080"),
    ];
    assert!(rebound_local(&pair));
    assert!(!same_site_local("POST", &pair));
    // A read is refused too, by the rebinding guard: reading a private archive
    // is what a rebinding page is after, and a same-origin GET has no Origin.
    assert!(rebound_local(&[("host", "evil.example:8080")]));
    // The genuine workroom, under every spelling of "this machine".
    for host in [
        "127.0.0.1:8080",
        "127.1.2.3:8080",
        "localhost:8080",
        "LOCALHOST",
        "localhost.:8080",
        "indice.localhost:8080",
        "[::1]:8080",
        "[::ffff:127.0.0.1]:8080",
    ] {
        assert!(!rebound_local(&[("host", host)]), "{host} is this machine");
        assert!(
            same_site_local(
                "POST",
                &[("host", host), ("origin", &format!("http://{host}"))]
            ),
            "{host} is this machine"
        );
    }
    // Names that only look like it.
    for host in [
        "localhost.evil.example",
        "evillocalhost:8080",
        "127.0.0.1.evil.example",
        "[::ffff:10.0.0.1]:8080",
    ] {
        assert!(
            rebound_local(&[("host", host)]),
            "{host} is not this machine"
        );
    }
}

#[test]
fn the_rebinding_guard_reads_the_authority_a_browser_sent() {
    // No authority at all is not a browser, which always sends Host, so it is
    // not the case the guard is for.
    assert!(!rebound_local(&[]));
    // A Host that is there but names nothing is refused, not treated as absent.
    assert!(rebound_local(&[("host", "")]));
    let mut h = HeaderMap::new();
    h.insert(
        axum::http::header::HOST,
        axum::http::HeaderValue::from_bytes(b"evil\xff.example").unwrap(),
    );
    assert!(rebound_request(&h, &"/".parse().unwrap()));
    // Under HTTP/2 the authority is the URI's, not a header.
    let h2 = |uri: &str| rebound_request(&HeaderMap::new(), &uri.parse().unwrap());
    assert!(h2("http://evil.example:8080/search"));
    assert!(!h2("http://127.0.0.1:8080/search"));
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
        Some("t"),
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
        Some("t"),
        &views::Chrome {
            manage: true,
            signed_in: Some("ed".into()),
            can_login: false,
            ..Default::default()
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
    let plain = views::layout(Some("t"), &views::Chrome::default(), None, html! {}).into_string();
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
    let s = capture_quality(&l10n("en"), &c).unwrap();
    // 2xx+3xx are "ok": (96+2)/100 = 98%.
    assert!(s.starts_with("100 captures, 98% ok"), "{s}");
    // Failing codes surfaced, most frequent first.
    assert!(s.contains("404×1") && s.contains("504×1"), "{s}");
    // The codes follow the summary in parentheses (no em dash: house style).
    assert!(s.contains("98% ok (404×1"), "{s}");
    // Empty histogram → nothing to show.
    assert!(capture_quality(&l10n("en"), &BTreeMap::new()).is_none());
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
/// A crawl's files are named by their place in it, padded so that the names
/// sort in that order (`10-` after `09-`, not after `1-`), and none looks like
/// an integer, which JavaScript would put first.
#[test]
fn a_crawls_files_are_named_to_sort_in_the_crawls_order() {
    let file = |id: &str| -> crate::collections::Wacz {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "collection": "c",
            "source": "archive/c/a.wacz",
            "name": "n",
            "date_indexed": "2026-01-01T00:00:00Z",
            "file_size": 1,
            "sha256": "0",
        }))
        .unwrap()
    };
    // In the crawl's order, with ids that sort another way, some all digits.
    let ids = [
        "ffffffff", "23469625", "0d9b54be", "88634329", "a0000000", "b0000000", "c0000000",
        "d0000000", "e0000000", "f0000000", "00000001",
    ];
    let files: Vec<_> = ids.iter().map(|id| file(id)).collect();
    let refs: Vec<&crate::collections::Wacz> = files.iter().collect();
    let names: Vec<String> = crawl_file_names(&refs)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names[0], "00-ffffffff");
    assert_eq!(names[10], "10-00000001");
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(sorted, names, "sorting keeps the crawl's order");
    assert!(names.iter().all(|n| n.parse::<u64>().is_err()));
    // One file needs no padding.
    assert_eq!(crawl_file_names(&refs[..1])[0].0, "0-ffffffff");
}

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
        &crate::i18n::L10n::default(),
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
    assert!(month_axis(
        &crate::i18n::L10n::default(),
        &[],
        |_| String::new(),
        |_| false
    )
    .is_empty());
    // The labels follow the reader's language.
    let it = crate::i18n::negotiate(Some("it"), &"en".parse().unwrap());
    let bars = month_axis(&it, &[(202105, 1)], |ym| ym.to_string(), |_| false);
    assert_eq!(bars[0].label, "maggio 2021");
}

/// The axis shows at most ten years, and its heights are scaled to the busiest
/// month it shows: one huge month the window cuts off must not flatten the rest.
#[test]
fn a_time_axis_is_scaled_to_the_months_it_shows() {
    let bars = month_axis(
        &crate::i18n::L10n::default(),
        &[(200801, 10_000), (202501, 50), (202502, 25)],
        |ym| ym.to_string(),
        |_| false,
    );
    assert_eq!(bars.len(), 120, "ten years, ending at the last month");
    assert!(bars.iter().all(|b| !b.label.contains("2008")));
    let jan = bars.iter().find(|b| b.label == "January 2025").unwrap();
    let feb = bars.iter().find(|b| b.label == "February 2025").unwrap();
    assert_eq!(jan.step, 10);
    assert_eq!(feb.step, 5);
}

/// A blank `site:` value is unset, so the header always has a name to link home
/// with, and the homepage a line under it.
#[test]
fn blank_site_text_falls_back_to_the_defaults() {
    let cfg = crate::config::SiteConfig {
        name: Some("  ".into()),
        tagline: Some("\n".into()),
        ..Default::default()
    };
    let site = views::Site::new(&cfg);
    assert_eq!(site.name, "indice");
    assert!(site.tagline.is_none(), "a blank tagline is unset");

    let named = views::Site::new(&crate::config::SiteConfig {
        name: Some(" Ada's archive ".into()),
        ..Default::default()
    });
    assert_eq!(named.name, "Ada's archive", "surrounding space is trimmed");
}

/// A replay address names what is read and nothing else, and reads back as
/// the same page: the archived URL's scheme, query and own escapes survive
/// the round trip.
#[test]
fn a_replay_address_round_trips_the_archived_url() {
    let cases = [
        (
            "https://www.climate.gov/",
            "/crawl/aeea520d/20250421164752/https://www.climate.gov/",
        ),
        (
            "https://ex.org/search?q=sea+ice&page=2",
            "/crawl/aeea520d/20250421164752/https://ex.org/search?q=sea+ice&page=2",
        ),
        (
            "https://ex.org/caf%C3%A9%20x",
            "/crawl/aeea520d/20250421164752/https://ex.org/caf%C3%A9%20x",
        ),
    ];
    for (url, href) in cases {
        let built =
            replay_href(ReplayScope::Crawl("aeea520d"), "2025-04-21T16:47:52Z", url).unwrap();
        assert_eq!(built, href);
        let (path, query) = match built.split_once('?') {
            Some((p, q)) => (p.to_string(), Some(q.to_string())),
            None => (built.clone(), None),
        };
        let t = parse_replay_path(&path, query.as_deref()).unwrap();
        assert_eq!(
            (t.kind, t.id.as_str(), t.ts.as_str(), t.url.as_str()),
            (ReplayKind::Crawl, "aeea520d", "20250421164752", url)
        );
    }
}

/// The archived URL goes in as a browser would request it: what a link
/// can't hold is encoded, the fragment dropped, and the scheme and host
/// lowercased. What replay can't open gets no address.
#[test]
fn a_replay_address_holds_the_url_as_a_browser_requests_it() {
    let href = |url| replay_href(ReplayScope::Crawl("c"), "", url);
    assert_eq!(
        href("https://ex.org/a%20b/caf\u{e9} x\"<>#top").as_deref(),
        Some("/crawl/c/https://ex.org/a%20b/caf%C3%A9%20x%22%3C%3E")
    );
    assert_eq!(
        href("HTTP://Example.ORG/").as_deref(),
        Some("/crawl/c/http://example.org/")
    );
    assert_eq!(href("ftp://ex.org/f"), None);
    assert_eq!(href("not a url"), None);
}

/// A browser resolves `.` and `..` in a link before it asks for it, `%2e%2e`
/// included, so a dot segment left in an archived URL would climb out of the
/// replay path. They are resolved within the archived URL, as the browser
/// that captured it would have, and can't reach above its host.
#[test]
fn an_archived_url_cannot_climb_out_of_its_replay_address() {
    for hostile in [
        "https://a/../../../../manage/add",
        "https://a/%2e%2e/%2E%2E/../manage/add",
        "https://a/x/./../../manage/add",
    ] {
        let built = replay_href(ReplayScope::Crawl("c"), "2025", hostile).unwrap();
        assert_eq!(built, "/crawl/c/2025/https://a/manage/add", "{hostile}");
        assert!(!built.contains(".."));
    }
    let t = parse_replay_path("/crawl/c/2025/https://a/../../manage/add", None).unwrap();
    assert_eq!(t.url, "https://a/manage/add");
}

#[test]
fn a_replay_path_takes_a_partial_time_or_none_and_mends_a_folded_scheme() {
    let t =
        parse_replay_path("/collection/climate-gov/2025/https:/www.climate.gov/", None).unwrap();
    assert_eq!(t.kind, ReplayKind::Collection);
    assert_eq!(t.ts, "2025");
    assert_eq!(t.url, "https://www.climate.gov/");
    let t = parse_replay_path("/crawl/abc/https://ex.org/", None).unwrap();
    assert_eq!((t.ts.as_str(), t.url.as_str()), ("", "https://ex.org/"));
    // A Wayback-style 17-digit time, of which replay reads 14.
    let t = parse_replay_path("/crawl/abc/20250421164752123/https://ex.org/", None).unwrap();
    assert_eq!(t.ts, "20250421164752");
    assert_eq!(
        replay_href(ReplayScope::Collection("c"), "", "https://ex.org/").as_deref(),
        Some("/collection/c/https://ex.org/")
    );
    assert!(parse_replay_path("/crawl/abc", None).is_none());
    assert!(parse_replay_path("/crawl/abc/", None).is_none());
    assert!(parse_replay_path("/search/x/y", None).is_none());
    assert!(
        parse_replay_path("/collection/c/pages", None).is_none(),
        "a collection's own page"
    );
    assert!(parse_replay_path("/collection/c/2025/not-a-web-address", None).is_none());
}

/// The log keeps which crawl or collection was replayed, never the page, even
/// for a path that won't replay.
#[test]
fn a_replay_path_is_logged_without_the_page() {
    for (path, logged) in [
        (
            "/crawl/aeea520d/20250421164752/https://www.climate.gov/x",
            "/crawl/aeea520d/…",
        ),
        ("/collection/c/2025/https://ex.org/", "/collection/c/…"),
        ("/crawl/x/2025/ftp://secret.example/page", "/crawl/x/…"),
        (
            "/collection/c/123456789012345678/https://ex.org/",
            "/collection/c/…",
        ),
        ("/crawl/aeea520d", "/crawl/aeea520d"),
        ("/collection/c/pages", "/collection/c/pages"),
        ("/collection/c/replay.json", "/collection/c/replay.json"),
        ("/collection/c/annotations", "/collection/c/annotations"),
        // A note's own page is cut like a replayed page: it is what someone
        // read.
        (
            "/collection/c/annotations/0123456789abcdef0123456789abcdef",
            "/collection/c/…",
        ),
        ("/crawl/aeea520d/pages", "/crawl/aeea520d/pages"),
        ("/crawl/aeea520d/replay.json", "/crawl/aeea520d/replay.json"),
    ] {
        assert_eq!(loggable_path(path), logged, "{path}");
    }
}

/// A note's JSON-LD goes to whoever ranks JSON above HTML, by q-value; a
/// browser, curl's `*/*`, a tie and a refusal all get the page.
#[test]
fn a_note_is_json_ld_only_for_a_client_that_ranks_it_first() {
    let asks = |accept: &str| {
        let mut h = HeaderMap::new();
        if !accept.is_empty() {
            h.insert(axum::http::header::ACCEPT, accept.parse().unwrap());
        }
        super::collection::wants_json_ld(&h)
    };
    for json in [
        "application/ld+json",
        "application/json",
        r#"application/ld+json; profile="http://www.w3.org/ns/anno.jsonld""#,
        "application/json, text/html;q=0.1",
        "text/html;q=0.5, APPLICATION/LD+JSON",
    ] {
        assert!(asks(json), "{json:?} asks for JSON-LD");
    }
    for page in [
        "",
        "*/*",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        "application/json, text/html",
        "application/ld+json;q=0, text/html",
        "application/ld+json;q=0",
    ] {
        assert!(!asks(page), "{page:?} gets the page");
    }
}

/// A note indice didn't mint, in a hand-edited store say, has no address of its
/// own, since that address couldn't find it: no link rather than a dead one.
#[test]
fn only_a_minted_note_has_an_address() {
    use super::collection::annotation_path;
    assert_eq!(
        annotation_path(
            "c",
            "urn:indice:annotation:0123456789abcdef0123456789abcdef"
        )
        .as_deref(),
        Some("/collection/c/annotations/0123456789abcdef0123456789abcdef")
    );
    for id in ["urn:uuid:7b1f", "urn:indice:annotation:0123ABCD", "x"] {
        assert_eq!(annotation_path("c", id), None, "{id:?}");
    }
}

/// An app router over an empty home, whose `site.language` is `language`.
fn app_with_language(language: Option<&str>) -> (tempfile::TempDir, Router) {
    let tmp = tempfile::TempDir::new().unwrap();
    if let Some(tag) = language {
        std::fs::write(
            crate::config::Config::path(tmp.path()),
            format!("site:\n  language: {tag}\n"),
        )
        .unwrap();
    }
    let config = ServerConfig::new(Access::local("127.0.0.1:0".parse().unwrap()).unwrap());
    let app = router(tmp.path(), config).unwrap();
    (tmp, app)
}

async fn get_with(
    app: &Router,
    path: &str,
    accept_language: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    use tower::ServiceExt;

    let mut req = axum::http::Request::get(path).header("host", "localhost");
    if let Some(v) = accept_language {
        req = req.header("accept-language", v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn home_follows_the_browser_language() {
    let (_home, app) = app_with_language(None);
    let (status, _, body) = get_with(&app, "/", Some("it")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#"lang="it""#), "{body}");
    let (_, _, body) = get_with(&app, "/", None).await;
    assert!(body.contains(r#"lang="en""#), "{body}");
    assert!(body.contains(r#"dir="ltr""#), "{body}");
}

#[tokio::test]
async fn home_falls_back_to_site_language() {
    let (_home, app) = app_with_language(Some("it"));
    let (_, _, body) = get_with(&app, "/", Some("ja")).await;
    assert!(body.contains(r#"lang="it""#), "{body}");
    // Asking for a language indice has wins over the fallback.
    let (_, _, body) = get_with(&app, "/", Some("en")).await;
    assert!(body.contains(r#"lang="en""#), "{body}");
}

/// Whatever a browser or a hostile client sends, the answer is a page in a
/// language indice has.
#[tokio::test]
async fn a_messy_accept_language_never_breaks_a_page() {
    let (_home, app) = app_with_language(None);
    let long = format!("{}it", "xx,".repeat(5000));
    let cases = [
        ("en;q=0.5, it;q=0.9", "it"),
        ("*", "en"),
        ("it-CH", "it"),
        ("IT", "it"),
        ("fr, it;q=0.1", "it"),
        ("\u{fffd}\u{fffd};;;q=,,,", "en"),
        ("it;q=0", "en"),
        (long.as_str(), "en"),
    ];
    for (header, lang) in cases {
        let (status, _, body) = get_with(&app, "/", Some(header)).await;
        assert_eq!(status, StatusCode::OK, "{header:?}");
        assert!(body.contains(&format!(r#"lang="{lang}""#)), "{header:?}");
    }
    // A header that is not text at all.
    let res = tower::ServiceExt::oneshot(
        app.clone(),
        axum::http::Request::get("/")
            .header("host", "localhost")
            .header("accept-language", &[0xffu8, 0xfe][..])
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

fn vary_values(headers: &HeaderMap) -> String {
    headers
        .get_all(axum::http::header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(", ")
        .to_ascii_lowercase()
}

#[tokio::test]
async fn pages_vary_on_accept_language() {
    let (_home, app) = app_with_language(None);
    for path in ["/", "/search?q=x"] {
        let (_, headers, _) = get_with(&app, path, None).await;
        assert!(vary_values(&headers).contains("accept-language"), "{path}");
    }
    let (status, headers, _) = get_with(&app, "/assets/app.css", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!vary_values(&headers).contains("accept-language"));
}

/// A handler that already varies on `Accept` (a note's page and JSON-LD share a
/// URL) keeps that, and gains `Accept-Language`.
#[tokio::test]
async fn vary_is_appended_not_replaced() {
    use tower::ServiceExt;

    let app = Router::new()
        .route(
            "/note",
            get(|| async { ([(axum::http::header::VARY, "Accept")], "note") }),
        )
        .layer(axum::middleware::from_fn(i18n::vary_accept_language));
    let res = app
        .oneshot(
            axum::http::Request::get("/note")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let vary = vary_values(res.headers());
    assert!(
        vary.contains("accept") && vary.contains("accept-language"),
        "{vary}"
    );
}

#[test]
fn an_unsupported_site_language_stops_startup() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        crate::config::Config::path(tmp.path()),
        "site:\n  language: xx\n",
    )
    .unwrap();
    let config = ServerConfig::new(Access::local("127.0.0.1:0".parse().unwrap()).unwrap());
    let err = format!("{:#}", router(tmp.path(), config).unwrap_err());
    assert!(err.contains("site.language") && err.contains("it"), "{err}");
}

fn l10n(tag: &str) -> crate::i18n::L10n {
    crate::i18n::negotiate(Some(tag), &"en".parse().unwrap())
}

#[tokio::test]
async fn search_page_in_italian() {
    let (_home, app) = app_with_language(None);
    let (status, _, body) = get_with(&app, "/search?q=x", Some("it")).await;
    assert_eq!(status, StatusCode::OK);
    let placeholder = l10n("it").tr("search-placeholder-all");
    assert!(body.contains(&placeholder), "{body}");
    assert!(!body.contains("Search all collections"), "{body}");
    let (_, _, body) = get_with(&app, "/search?q=x", Some("en")).await;
    assert!(body.contains("Search all collections"), "{body}");
}

#[test]
fn result_count_plurals() {
    for (tag, expected) in [
        ("en", ["0 results", "1 result", "3 results"]),
        ("it", ["0 risultati", "1 risultato", "3 risultati"]),
    ] {
        let l = l10n(tag);
        for (n, want) in [0, 1, 3].into_iter().zip(expected) {
            assert_eq!(views::results_count(&l, n, false), want, "{tag} {n}");
        }
    }
    assert_eq!(
        views::results_count(&l10n("en"), 1000, true),
        "1,000+ results"
    );
    assert_eq!(
        views::results_count(&l10n("it"), 1000, true),
        "1.000+ risultati"
    );
}

#[tokio::test]
async fn default_tagline_follows_language() {
    let (_home, app) = app_with_language(None);
    let (_, _, en) = get_with(&app, "/", Some("en")).await;
    assert!(en.contains("Web archive search and replay"), "{en}");
    let (_, _, it) = get_with(&app, "/", Some("it")).await;
    let italian = l10n("it").tr("site-tagline-default");
    assert!(it.contains(&italian), "{it}");
    assert!(!it.contains("Web archive search and replay"), "{it}");
}

#[tokio::test]
async fn custom_tagline_is_not_translated() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        crate::config::Config::path(tmp.path()),
        "site:\n  tagline: Our own words\n",
    )
    .unwrap();
    let config = ServerConfig::new(Access::local("127.0.0.1:0".parse().unwrap()).unwrap());
    let app = router(tmp.path(), config).unwrap();
    for lang in ["en", "it"] {
        let (_, _, body) = get_with(&app, "/", Some(lang)).await;
        assert!(body.contains("Our own words"), "{lang}: {body}");
        assert!(
            !body.contains(&l10n("it").tr("site-tagline-default")),
            "{lang}"
        );
        assert!(!body.contains("Web archive search and replay"), "{lang}");
    }
}

/// A home with the fixture WACZ indexed into `collection`, and a router over it.
fn app_with_crawl(collection: &str) -> (tempfile::TempDir, Router, String) {
    app_with_named_crawl(collection, "Simple")
}

/// As [`app_with_crawl`], with the crawl called `name`.
fn app_with_named_crawl(collection: &str, name: &str) -> (tempfile::TempDir, Router, String) {
    let tmp = tempfile::TempDir::new().unwrap();
    let input = tmp.path().join("input.wacz");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/simple.wacz"),
        &input,
    )
    .unwrap();
    crate::index::Ingest::new(tmp.path())
        .name(Some(name))
        .index_location(&input.to_string_lossy(), collection)
        .unwrap();
    let manifest =
        crate::collections::Manifest::open(&crate::index::index_dir(tmp.path())).unwrap();
    let id = manifest.waczs[0].id.clone();
    let config = ServerConfig::new(Access::local("127.0.0.1:0".parse().unwrap()).unwrap());
    let app = router(tmp.path(), config).unwrap();
    (tmp, app, id)
}

#[tokio::test]
async fn collection_page_in_italian() {
    let (_home, app, _) = app_with_crawl("c");
    let it = l10n("it");
    let (status, _, body) = get_with(&app, "/collection/c", Some("it")).await;
    assert_eq!(status, StatusCode::OK);
    for key in [
        "collection-about",
        "collection-crawls-heading",
        "meta-capture-dates",
        "meta-size",
    ] {
        let word = it.tr(key);
        assert!(body.contains(&word), "{key}: {body}");
    }
    for english in [
        "About this collection",
        "Capture dates",
        ">Crawls<",
        "Stored here",
    ] {
        assert!(!body.contains(english), "{english}: {body}");
    }
    let (_, _, body) = get_with(&app, "/collection/c", Some("en")).await;
    assert!(body.contains("About this collection"), "{body}");
    assert!(body.contains("Capture dates"), "{body}");
}

#[tokio::test]
async fn crawl_page_in_italian() {
    let (_home, app, id) = app_with_crawl("c");
    let it = l10n("it");
    let (status, _, body) = get_with(&app, &format!("/crawl/{id}"), Some("it")).await;
    assert_eq!(status, StatusCode::OK);
    for key in ["crawl-file", "crawl-pages-heading", "meta-indexed"] {
        let word = it.tr(key);
        assert!(body.contains(&word), "{key}: {body}");
    }
    for english in ["Indexed", "No pages are listed", ">✓ present<", "Status"] {
        assert!(!body.contains(english), "{english}: {body}");
    }
}

/// A crawl named only by its tool's identifier is called by its date, and the
/// words around the date are in the page's language wherever its name shows.
#[tokio::test]
async fn a_machine_named_crawl_is_named_in_italian() {
    let (_home, app, id) = app_with_named_crawl("c", "20250107171404015-af3df7fa-3fc-0");
    let pages = [
        format!("/crawl/{id}"),
        "/collection/c".to_string(),
        format!("/search?q=crawl:{id}"),
    ];
    for path in &pages {
        let (status, _, body) = get_with(&app, path, Some("it")).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains("Scansione del "), "{path}: {body}");
        assert!(!body.contains("Crawl of "), "{path}: {body}");
        let (_, _, body) = get_with(&app, path, Some("en")).await;
        assert!(body.contains("Crawl of "), "{path}: {body}");
    }
}

#[tokio::test]
async fn curator_text_stays_escaped_in_italian() {
    let hostile = "<img src=x onerror=alert(1)>";
    let (_home, app, id) = app_with_crawl(hostile);
    let slug = crate::collections::slugify(hostile);
    for path in [format!("/collection/{slug}"), format!("/crawl/{id}")] {
        let (status, _, body) = get_with(&app, &path, Some("it")).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains("&lt;img"), "{path}: {body}");
        assert!(!body.contains("<img src=x"), "{path}: {body}");
    }
}

/// A collection page with `n` member crawls, for the delete confirmation.
fn collection_with_members(n: usize, tag: &str) -> String {
    let members = (0..n)
        .map(|i| views::MemberItem {
            id: format!("m{i}"),
            name: format!("Crawl {i}"),
            when: None,
            present: true,
            remote: false,
            provenance: None,
            thumb: None,
        })
        .collect();
    let page = views::CollectionPage {
        name: "C".into(),
        description: None,
        narrative: None,
        creator: None,
        dates: None,
        rights: None,
        subjects: vec![],
        meta: vec![],
        facets: vec![],
        members,
        crawl_months: vec![],
        replay_href: None,
        id: "c".into(),
        can_delete: true,
        chrome: views::Chrome {
            l10n: l10n(tag),
            ..Default::default()
        },
        annotation_count: 0,
        drafted_note: None,
    };
    views::collection(&page).into_string()
}

#[test]
fn delete_confirmation_plurals() {
    for (tag, one, three, all_three) in [
        (
            "en",
            "has 1 crawl.",
            "has 3 crawls.",
            "also delete all 3 member crawls",
        ),
        (
            "it",
            "ha 1 scansione.",
            "ha 3 scansioni.",
            "tutte le 3 scansioni",
        ),
    ] {
        let body = collection_with_members(1, tag);
        assert!(body.contains(one), "{tag}: {body}");
        assert!(!body.contains("(s)"), "{tag}: {body}");
        let body = collection_with_members(3, tag);
        assert!(body.contains(three), "{tag}: {body}");
        assert!(body.contains(all_three), "{tag}: {body}");
        assert!(!body.contains("(s)"), "{tag}: {body}");
    }
}

#[test]
fn date_span_in_both_languages() {
    let (start, end) = (Some("20250107000000"), Some("20260803120000"));
    assert_eq!(
        date_span(&l10n("en"), start, end).unwrap(),
        "7 January 2025 to 3 August 2026"
    );
    assert_eq!(
        date_span(&l10n("it"), start, end).unwrap(),
        "dal 7 gennaio 2025 al 3 agosto 2026"
    );
    // The same day on both ends is one date, not "from X to X".
    let (a, b) = (Some("20250107000000"), Some("20250107235959"));
    assert_eq!(date_span(&l10n("en"), a, b).unwrap(), "7 January 2025");
    assert_eq!(date_span(&l10n("it"), a, b).unwrap(), "7 gennaio 2025");
    assert_eq!(date_span(&l10n("it"), None, None), None);
}

async fn post_with(
    app: &Router,
    path: &str,
    headers: &[(&str, &str)],
    body: Body,
) -> (StatusCode, HeaderMap, String) {
    use tower::ServiceExt;

    let mut req = axum::http::Request::post(path).header("host", "localhost");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn manage_page_in_italian() {
    let (_home, app) = app_with_language(None);
    let it = l10n("it");
    for (path, keys, english) in [
        (
            "/manage/add",
            vec![
                "desk-heading",
                "desk-eyebrow",
                "desk-tab-upload",
                "desk-submit",
            ],
            vec!["Accession", "Display name", "Hide already-imported"],
        ),
        (
            "/manage/collections/new",
            vec!["form-new-heading", "form-label-narrative", "form-create"],
            vec!["Finding aid", "Create collection", "comma-separated"],
        ),
    ] {
        let (status, _, body) = get_with(&app, path, Some("it")).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        for key in keys {
            let word = it.tr(key);
            assert!(body.contains(&word), "{path} {key}: {body}");
        }
        for word in english {
            assert!(!body.contains(word), "{path} {word}: {body}");
        }
    }
}

/// The page carries the refusal the server would send, so a refusal says the
/// same thing whichever of the two catches it, in the reader's language.
#[tokio::test]
async fn refused_upload_in_italian() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        crate::config::Config::path(tmp.path()),
        "uploads:\n  max_mb: 1\n",
    )
    .unwrap();
    let config = ServerConfig::new(Access::local("127.0.0.1:0".parse().unwrap()).unwrap());
    let app = router(tmp.path(), config).unwrap();
    let declared = (2 * 1024 * 1024).to_string();
    let headers = [
        ("accept-language", "it"),
        ("content-type", "multipart/form-data; boundary=x"),
        ("content-length", declared.as_str()),
    ];
    let (status, resp_headers, text) =
        post_with(&app, "/api/archives/upload", &headers, Body::empty()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let it = l10n("it");
    let sentence = it.tr_args(
        "refused-too-large",
        &[("cap", crate::i18n::FluentArg::Str(&it.size(1024 * 1024)))],
    );
    assert_eq!(text, sentence);
    assert!(text.contains(&it.size(1024 * 1024)), "{text}");
    assert!(text.contains("uploads.max_mb"), "{text}");
    assert!(!text.contains("larger than"), "{text}");
    assert!(vary_values(&resp_headers).contains("accept-language"));

    // The accession desk says exactly the same thing, escaped into its attribute.
    let (_, _, desk) = get_with(&app, "/manage/add", Some("it")).await;
    let attr = maud::html! { (text) }.into_string();
    assert!(desk.contains(&attr), "{desk}");
}

#[tokio::test]
async fn curator_error_keeps_chain() {
    let (home, app, _) = app_with_crawl("c");
    // Make the manifest unreadable, so a curator's page fails with a chain.
    std::fs::write(
        crate::index::index_dir(home.path()).join("waczs.json"),
        "{ not json",
    )
    .unwrap();
    let it = l10n("it");
    let outer = it.tr_args(
        "error-request-failed",
        &[("detail", crate::i18n::FluentArg::Str(""))],
    );
    let prefix = outer.trim_end();
    for (lang, expect_prefix) in [
        ("it", prefix.to_string()),
        ("en", "The request failed".into()),
    ] {
        let (status, _, body) = get_with(&app, "/manage/edit/c", Some(lang)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(body.starts_with(&expect_prefix), "{lang}: {body}");
        // The chain is indice's English error text, passed through unchanged.
        assert!(body.contains("waczs.json"), "{lang}: {body}");
        let reference = if lang == "it" {
            "riferimento"
        } else {
            "reference"
        };
        assert!(body.contains(reference), "{lang}: {body}");
    }
}

#[tokio::test]
async fn public_search_api_stays_english() {
    let (_home, app) = app_with_language(None);
    let long = "a".repeat(5000);
    let (status, _, body) = get_with(&app, &format!("/api/search?q={long}"), Some("it")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.starts_with("search query too long"), "{body}");
    // The same refusal on the search page is the reader's language.
    let (status, _, body) = get_with(&app, &format!("/search?q={long}"), Some("it")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!body.starts_with("search query too long"), "{body}");
    assert!(body.contains("5000"), "{body}");
}

#[tokio::test]
async fn page_not_found_in_italian() {
    let (_home, app) = app_with_language(None);
    for path in ["/collection/nope", "/crawl/nope"] {
        let (status, _, it) = get_with(&app, path, Some("it")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        let (_, _, en) = get_with(&app, path, Some("en")).await;
        assert_ne!(it, en, "{path}");
    }
}

#[tokio::test]
async fn a_refusal_says_why_in_the_readers_language() {
    use super::auth::{Denied, Need};
    use axum::response::IntoResponse;
    for (tag, forbidden) in [
        (
            "en",
            "forbidden: your account is not authorized for curating",
        ),
        ("it", "vietato: il tuo account non è autorizzato a curare"),
    ] {
        let res = Denied::Insufficient(Need::Curating, l10n(tag)).into_response();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let text = String::from_utf8_lossy(
            &axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .into_owned();
        assert!(text.starts_with(forbidden), "{tag}: {text}");
    }
    let res = Denied::Unauthenticated(l10n("it")).into_response();
    let text = String::from_utf8_lossy(
        &axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .into_owned();
    assert_eq!(text, l10n("it").tr("denied-unauthenticated"));

    // The page a browser gets says it in the same words.
    let chrome = views::Chrome {
        l10n: l10n("it"),
        ..Default::default()
    };
    let page = views::access_denied(&chrome, Need::DeletingOthersCrawl.key()).into_string();
    assert!(page.contains(&l10n("it").tr("denied-heading")), "{page}");
    assert!(
        page.contains("aggiunta da un&#39;altra persona")
            || page.contains("aggiunta da un'altra persona"),
        "{page}"
    );
    assert!(!page.contains("You can't change"), "{page}");
}

#[tokio::test]
async fn desk_keeps_the_wacz_code_and_a_path_is_refused_in_italian() {
    let (_home, app) = app_with_language(None);
    for lang in ["en", "it"] {
        let (_, _, desk) = get_with(&app, "/manage/add", Some(lang)).await;
        assert!(desk.contains("<code>.wacz</code>"), "{lang}: {desk}");
    }
    let body = r#"{"path":"/etc/x.wacz","collection":"c"}"#;
    let (status, _, text) = post_with(
        &app,
        "/api/archives",
        &[
            ("accept-language", "it"),
            ("content-type", "application/json"),
        ],
        Body::from(body),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(text, l10n("it").tr("error-location-not-url"));
    assert!(!text.contains("add an archive"), "{text}");
}

/// The workroom's scripts get their sentences from the page, in the reader's
/// language, so the browser never needs a catalog of its own.
#[tokio::test]
async fn workroom_ships_italian_js_strings() {
    let (_home, app) = app_with_language(None);
    for (lang, loading) in
        [("it", l10n("it")), ("en", l10n("en"))].map(|(lang, l)| (lang, l.tr("js-loading-crawls")))
    {
        let (_, _, page) = get_with(&app, "/manage/add", Some(lang)).await;
        let marker = r#"<script id="i18n" type="application/json">"#;
        let start = page
            .find(marker)
            .unwrap_or_else(|| panic!("{lang}: no i18n element"))
            + marker.len();
        let end = start + page[start..].find("</script>").unwrap();
        let strings: serde_json::Value = serde_json::from_str(&page[start..end]).unwrap();
        assert_eq!(strings["js-loading-crawls"], loading.as_str(), "{lang}");
    }
    assert_ne!(
        l10n("it").tr("js-loading-crawls"),
        l10n("en").tr("js-loading-crawls")
    );
}

/// The quoted string literals of a line of JavaScript (single, double or
/// template), with the part after a `//` comment dropped. A tokenizer rather
/// than a regex so an apostrophe inside a comment or another quote's text does
/// not start a literal.
fn js_string_literals(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut found = vec![];
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '/' if chars.get(i + 1) == Some(&'/') => break,
            quote @ ('\'' | '"' | '`') => {
                let mut text = String::new();
                i += 1;
                while i < chars.len() && chars[i] != quote {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    if let Some(c) = chars.get(i) {
                        text.push(*c);
                    }
                    i += 1;
                }
                found.push(text);
            }
            _ => {}
        }
        i += 1;
    }
    found
}

/// Every sentence the workroom's and the replay viewer's scripts show comes
/// from `t(...)`, so none can be left in English on an Italian page. A literal that starts with a letter
/// and holds a space reads as prose; selectors, ids, class names, event names and
/// headers have no space, or start with a symbol.
#[test]
fn no_english_literals_in_scripts() {
    // Literals that look like prose but are not shown to a reader. Keep this
    // empty unless a literal truly is not UI text, and say why beside it.
    const NOT_UI: &[&str] = &[
        // The iframe's Permissions Policy value (viewer.js), read by the browser.
        "autoplay; fullscreen",
        // The directive at the top of annotations.js.
        "use strict",
        // A selector (annotations.js).
        "button, a",
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static/assets");
    let replay = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static/replay");
    // Not the vendored `ui.js` and `sw.js`, which are ReplayWeb.page's own.
    let files = [
        (&dir, "manage.js"),
        (&dir, "editor.js"),
        (&dir, "i18n.js"),
        (&replay, "viewer.js"),
        (&replay, "annotations.js"),
    ];
    for (dir, file) in files {
        let src = std::fs::read_to_string(dir.join(file)).unwrap();
        for (n, line) in src.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("/*") || code.starts_with('*') {
                continue;
            }
            for lit in js_string_literals(line) {
                let prose = lit.starts_with(|c: char| c.is_ascii_alphabetic()) && lit.contains(' ');
                assert!(
                    !prose || NOT_UI.contains(&lit.as_str()),
                    "{file}:{}: English literal {lit:?}: {line}",
                    n + 1
                );
            }
        }
    }
}

/// The scan itself: it flags the select's placeholder that once slipped through,
/// and passes selectors and `t(...)` calls.
#[test]
fn the_js_literal_scan_catches_prose() {
    let prose = |line: &str| {
        js_string_literals(line)
            .iter()
            .any(|l| l.starts_with(|c: char| c.is_ascii_alphabetic()) && l.contains(' '))
    };
    assert!(prose("fillSelect(sel, colls, 'All crawls');"));
    assert!(prose("x.title = `Waiting for ${s}`;"));
    assert!(!prose(
        "fillSelect(sel, colls, t('js-all-crawls')); // can't be 'All crawls'"
    ));
    assert!(!prose(
        "document.querySelector('#bx-items input:checked').dataset.id;"
    ));
}

/// A translation is text the viewer's template is filled with, so it must not
/// be able to fill a slot or close an element: a catalog edited by hand, or a
/// translator's slip, cannot turn the page's own data into someone else's.
#[test]
fn viewer_strings_cannot_fill_slots() {
    // Fluent source: `{"{{"}` is a literal `{{`, so the translation really
    // contains the text of the slots (a bare `{{x}}` would be a placeable).
    let hostile = r#"{"{{"}replay_context{"}}"}</script><b>{"{{"}analytics_head{"}}"}"#;
    let catalog = format!(
        "viewer-notes = {hostile}\nviewer-loading = {hostile}\nviewer-frame-title = {hostile}\nreplay-title = {hostile}\n"
    );
    let l10n = crate::i18n::l10n_from_catalog("en", &catalog);
    let template = std::str::from_utf8(&assets::ReplayAssets::get("viewer.html").unwrap().data)
        .unwrap()
        .to_string();
    let html = assets::viewer_html(
        &template,
        &l10n,
        "<!-- head -->",
        "https://replay.test",
        "Site",
        r#"{"url":"https://example.org/{{site_name}}"}"#,
    );
    assert_eq!(
        html.matches(r#"id="replay-context""#).count(),
        1,
        "one replay-context element: {html}"
    );
    assert_eq!(html.matches(r#"id="i18n""#).count(), 1, "{html}");
    // The slots' text survives as written: in the button (as text), in the
    // loading notice, and in the JSON, and nothing was filled into it.
    assert!(
        html.contains(r#"aria-expanded="true">{{replay_context}}&lt;/script&gt;&lt;b&gt;{{analytics_head}}</button>"#),
        "{html}"
    );
    assert!(
        html.contains(
            r#""viewer-notes":"{{replay_context}}\u003c/script>\u003cb>{{analytics_head}}""#
        ),
        "{html}"
    );
    assert_eq!(html.matches("<!-- head -->").count(), 1, "{html}");
    // The page's own elements and nothing a string opened.
    assert_eq!(html.matches("</script>").count(), 5, "{html}");
    assert!(!html.contains("<b>"), "{html}");
    assert!(
        html.contains("&lt;b&gt;"),
        "escaped where it is text: {html}"
    );
    assert!(
        html.contains("\\u003c/script>"),
        "escaped in the JSON: {html}"
    );
    // The archived URL names a slot, and the slot stays as written.
    assert!(html.contains("example.org/{{site_name}}"), "{html}");
    assert!(!html.contains("{{lang}}"), "{html}");
}

/// The drafted note names its fields inside an Italian sentence, so the words
/// stay lower case and a field whose name has its own "e" ("ambito e contenuto")
/// stays readable in a list joined by "e".
#[test]
fn drafted_note_reads_as_a_sentence_in_italian() {
    use super::collection::drafted_note;
    let drafted = |fields: &[&str]| crate::collections::Drafted {
        model: "m".into(),
        service: String::new(),
        fields: fields.iter().map(|f| f.to_string()).collect(),
        at: String::new(),
    };
    let it = l10n("it");
    assert_eq!(
        drafted_note(&it, &drafted(&[]), None),
        "Bozza di alcuni campi generata con m."
    );
    assert_eq!(
        drafted_note(&it, &drafted(&["description", "narrative", "dates"]), None),
        "Bozza di “descrizione”, “ambito e contenuto” e “date” generata con m."
    );
    // English opens with the list, capitalized.
    assert_eq!(
        drafted_note(&l10n("en"), &drafted(&["narrative"]), None),
        "Narrative drafted with m."
    );
}

/// The search sidebar and filter chips name each filter in the page's language;
/// the library's English label is for the JSON API. Every filter the library
/// knows has a message, so a new facet cannot appear in English on an Italian page.
#[test]
fn filter_names_follow_the_page_language() {
    use super::pages::filter_name;
    let it = l10n("it");
    assert_eq!(filter_name(&it, "year"), "Anno");
    assert_eq!(filter_name(&it, "collection"), "Raccolta");
    assert_eq!(filter_name(&l10n("en"), "type"), "Type");
    for field in crate::search::filter_fields() {
        let key = format!("filter-{field}");
        assert!(it.has_message(&key), "no {key} message");
    }
}
