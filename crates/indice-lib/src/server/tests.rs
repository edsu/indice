use super::*;
use crate::views;
use axum::http::{HeaderMap, Method, Uri};
// Internals these tests exercise directly, now that they live in sibling modules.
use super::auth::{
    clear_session_cookie, hmac_sha256, local_redirect_target, same_site_request, sign_session,
    verify_session,
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

fn same_site(method: &str, hdrs: &[(&str, &str)], trusted: Option<&str>) -> bool {
    let (m, h, u) = csrf_case(method, hdrs);
    same_site_request(&m, &h, &u, trusted)
}

#[test]
fn same_site_request_never_gates_safe_methods() {
    // Reads change nothing, so a cross-site Origin on one is irrelevant.
    for method in ["GET", "HEAD", "OPTIONS", "TRACE"] {
        assert!(
            same_site(
                method,
                &[("host", "a.example"), ("origin", "https://evil.example")],
                None
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
    assert!(same_site("POST", &[("host", "a.example")], None));
    // Sec-Fetch-Site present but benign still passes.
    assert!(same_site(
        "POST",
        &[("host", "a.example"), ("sec-fetch-site", "same-origin")],
        None
    ));
    assert!(same_site(
        "POST",
        &[("host", "a.example"), ("sec-fetch-site", "none")],
        None
    ));
}

#[test]
fn same_site_request_rejects_foreign_origin_and_null() {
    assert!(!same_site(
        "POST",
        &[("host", "a.example"), ("origin", "https://evil.example")],
        None
    ));
    // A sandboxed or data: initiator sends "null" — no authority, so no match.
    assert!(!same_site(
        "POST",
        &[("host", "a.example"), ("origin", "null")],
        None
    ));
    // Same registrable domain but a different host is still a different origin.
    assert!(!same_site(
        "POST",
        &[("host", "a.example"), ("origin", "https://b.a.example")],
        None
    ));
    // A matching Origin passes, and the scheme is deliberately not compared.
    assert!(same_site(
        "POST",
        &[("host", "a.example"), ("origin", "https://a.example")],
        None
    ));
    assert!(same_site(
        "POST",
        &[("host", "a.example"), ("origin", "http://a.example")],
        None
    ));
}

#[test]
fn same_site_request_falls_back_to_sec_fetch_site_without_origin() {
    // Origin absent but Fetch Metadata says another site started this.
    for site in ["cross-site", "same-site"] {
        assert!(
            !same_site(
                "POST",
                &[("host", "a.example"), ("sec-fetch-site", site)],
                None
            ),
            "sec-fetch-site: {site} should be refused"
        );
    }
}

#[test]
fn same_site_request_matches_host_with_explicit_port() {
    // The loopback workroom: Origin carries the port, so Host must too.
    assert!(same_site(
        "POST",
        &[
            ("host", "127.0.0.1:8080"),
            ("origin", "http://127.0.0.1:8080")
        ],
        None
    ));
    // Port mismatch is a different origin — another service on the same host.
    assert!(!same_site(
        "POST",
        &[
            ("host", "127.0.0.1:8080"),
            ("origin", "http://127.0.0.1:9999")
        ],
        None
    ));
    // Case is insensitive on the host.
    assert!(same_site(
        "POST",
        &[("host", "A.Example"), ("origin", "https://a.example")],
        None
    ));
}

#[test]
fn same_site_request_prefers_forwarded_host_then_site_url() {
    // Behind Caddy: Host is the internal upstream, X-Forwarded-Host is what the
    // browser used — and the browser's Origin matches the latter.
    let behind_proxy = &[
        ("host", "indice:8080"),
        ("x-forwarded-host", "archive.example.org"),
        ("origin", "https://archive.example.org"),
    ];
    assert!(same_site("POST", behind_proxy, None));
    // An explicit --site-url outranks both headers (the nginx case, where Host
    // is rewritten and X-Forwarded-Host is absent).
    assert!(same_site(
        "POST",
        &[
            ("host", "indice:8080"),
            ("origin", "https://archive.example.org")
        ],
        Some("archive.example.org")
    ));
    // ...and it is authoritative, so a mismatch against it still fails.
    assert!(!same_site(
        "POST",
        &[("host", "indice:8080"), ("origin", "https://indice:8080")],
        Some("archive.example.org")
    ));
}

#[test]
fn same_site_request_refuses_when_our_own_authority_is_unknown() {
    // An Origin we can't compare against anything is not provably same-site.
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::ORIGIN,
        axum::http::HeaderValue::from_static("https://evil.example"),
    );
    let uri: Uri = "/api/collections/x/delete".parse().unwrap();
    assert!(!same_site_request(&Method::POST, &headers, &uri, None));
}

#[test]
fn appbar_offers_login_when_anonymous_under_forward_auth() {
    use maud::html;
    // Forward-auth configured, request anonymous → a "Log in" link, no name.
    let anon = views::layout("t", false, None, true, None, html! {}).into_string();
    assert!(
        anon.contains(r#"href="/manage/login""#) && anon.contains("Log in"),
        "anonymous + can_login should show a login link: {anon}"
    );
    assert!(!anon.contains("signed in as"));

    // Signed in → the identity + a logout link, and no login link.
    let authed = views::layout("t", true, Some("ed"), false, None, html! {}).into_string();
    assert!(authed.contains("signed in as") && authed.contains("ed"));
    assert!(authed.contains(r#"href="/logout""#) && authed.contains("Log out"));
    assert!(!authed.contains("/manage/login"));

    // Plain read-only server (no forward-auth): neither affordance.
    let plain = views::layout("t", false, None, false, None, html! {}).into_string();
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
