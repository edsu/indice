//! Small shared helpers: formatting, escaping, replay-link building, and the
//! provenance/capture summaries the pages render.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::collections::{Manifest, Wacz};

use super::*;

/// Format a byte count as a short human-readable size.
/// Format a byte count for display (e.g. `48.2 MB`). Shared by the web UI and
/// the CLI so both show sizes the same way.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut b = bytes as f64;
    let mut i = 0;
    while b >= 1024.0 && i < UNITS.len() - 1 {
        b /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{b:.1} {}", UNITS[i])
    }
}

pub(super) fn load_waczs(state: &AppState) -> Vec<Wacz> {
    Manifest::open(&state.index_dir)
        .map(|m| m.waczs)
        .unwrap_or_default()
}

/// The `source` value to hand wabac.js for a collection: our local byte-range
/// endpoint for a file, or the remote URL directly (read client-side) for a URL.
pub(super) fn viewer_source(col: &Wacz) -> String {
    match &col.source {
        // A Browsertrix source is served through /files/{id}, which re-resolves a
        // fresh presigned URL and 302-redirects to it (its stored URL expires).
        crate::collections::Source::File(_)
        | crate::collections::Source::Browsertrix { .. }
        | crate::collections::Source::BrowsertrixPublic { .. } => {
            format!("/files/{}", col.id)
        }
        crate::collections::Source::Url(u) => u.clone(),
    }
}

/// The viewer URL that replays a whole collection (multi-WACZ): wabac loads the
/// collection's `replay.json` manifest (see `collection_replay_json`) as one
/// merged collection. Passes an explicit `coll` namespace plus breadcrumb params.
///
/// A whole-collection entry has no page in mind, so it opens on a sensible
/// default landing page (`default_page`) when one is known — otherwise wabac
/// lands on its collection root. (Specific-context replay — a crawl's Replay
/// button, a search result — carries its own `url`/`ts` and doesn't come through
/// here.)
pub(super) fn collection_replay_href(
    id: &str,
    name: &str,
    default_page: Option<(String, String)>,
) -> String {
    let source = format!("/collection/{id}/replay.json");
    let mut href = format!(
        "/replay/viewer?source={}&coll={}&name={}&collection={}&collection_id={}",
        url_encode(&source),
        url_encode(id),
        url_encode(name),
        url_encode(name),
        url_encode(id),
    );
    if let Some((url, ts)) = default_page {
        href.push_str(&format!("&url={}&ts={}", url_encode(&url), url_encode(&ts)));
    }
    href
}

/// A sensible landing page for whole-collection replay: the first member (in
/// manifest order) that has a seed page, with that page's url and wabac
/// timestamp. `None` when no member has a seed page.
pub(super) fn collection_default_page(members: &[&Wacz]) -> Option<(String, String)> {
    members
        .iter()
        .find_map(|w| w.seed_pages.first())
        .map(|p| (p.url.clone(), ts_to_14digit(&p.ts)))
}

// ── Page annotations API (gnqf.3) ───────────────────────────────────────────
//
// Display is public, authoring is gated — the same pattern as finding aids and
// crawl notes. The public `GET /api/annotations` returns a capture's notes (or
// a whole collection's); create/update/delete live in the management block and
// so inherit its auth gate. Unlike the other write handlers, these also read
// the author identity (via `admin_ctx`) to attribute notes and gate edits.

pub(super) fn error_response(e: anyhow::Error) -> Response {
    tracing::error!("{e:#}");
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

/// `YYYY-MM-DD` from the first 8 digits of a 14-digit timestamp; the input as-is
/// if it is too short.
fn ymd(ts: &str) -> String {
    if ts.len() >= 8 && ts[..8].bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{}", &ts[0..4], &ts[4..6], &ts[6..8])
    } else {
        ts.to_string()
    }
}

/// The capture date range of a collection as a display string (`start → end`, or
/// a single date when they coincide), or `None` when no range was recorded.
pub(super) fn capture_range(c: &Wacz) -> Option<String> {
    match (c.capture_start.as_deref(), c.capture_end.as_deref()) {
        (Some(s), Some(e)) => {
            let (sd, ed) = (ymd(s), ymd(e));
            Some(if sd == ed {
                sd
            } else {
                format!("{sd} → {ed}")
            })
        }
        (Some(s), None) => Some(ymd(s)),
        (None, Some(e)) => Some(ymd(e)),
        (None, None) => None,
    }
}

/// A compact "capture quality" summary of an HTTP status histogram: total
/// captures, the share that succeeded (2xx/3xx), and the notable failing codes.
/// The derived DACS Appraisal signal — surfaces the 404/403/504 "absences" that
/// a clean-looking crawl can hide.
pub(super) fn capture_quality(counts: &std::collections::BTreeMap<u16, u64>) -> Option<String> {
    let total: u64 = counts.values().sum();
    if total == 0 {
        return None;
    }
    let ok: u64 = counts
        .iter()
        .filter(|(c, _)| (200..400).contains(*c))
        .map(|(_, n)| n)
        .sum();
    let ok_pct = (ok as f64 / total as f64 * 100.0).round() as u64;
    let mut s = format!("{total} captures, {ok_pct}% ok");
    // Notable failing codes (>= 400), most frequent first.
    let mut bad: Vec<(u16, u64)> = counts
        .iter()
        .filter(|(c, _)| **c >= 400)
        .map(|(c, n)| (*c, *n))
        .collect();
    bad.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if !bad.is_empty() {
        let parts: Vec<String> = bad
            .iter()
            .take(4)
            .map(|(c, n)| format!("{c}×{n}"))
            .collect();
        s.push_str(" — ");
        s.push_str(&parts.join(", "));
    }
    Some(s)
}

/// Map a Browsertrix QA review rating (1–5) to its label, with the raw value —
/// a DACS Appraisal signal ("this crawl was reviewed and judged …").
pub(super) fn review_label(rating: u8) -> String {
    let word = match rating {
        5 => "Excellent",
        4 => "Good",
        3 => "Fair",
        2 => "Poor",
        1 => "Bad",
        _ => "Reviewed",
    };
    format!("{word} ({rating}/5)")
}

/// Merge the per-crawl status histograms across a collection's members.
pub(super) fn merged_status_counts(members: &[&Wacz]) -> std::collections::BTreeMap<u16, u64> {
    let mut agg = std::collections::BTreeMap::new();
    for w in members {
        for (code, n) in &w.status_counts {
            *agg.entry(*code).or_insert(0) += *n;
        }
    }
    agg
}

/// The deduped union of software across a collection's member WACZs.
pub(super) fn collection_software(members: &[&Wacz]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in members {
        for s in &w.software {
            if !out.contains(s) {
                out.push(s.clone());
            }
        }
    }
    out
}

/// The capture date range spanning a collection's member WACZs.
pub(super) fn members_capture_range(members: &[&Wacz]) -> Option<String> {
    let start = members.iter().filter_map(|w| w.capture_start.clone()).min();
    let end = members.iter().filter_map(|w| w.capture_end.clone()).max();
    match (start, end) {
        (Some(s), Some(e)) => {
            let (sd, ed) = (ymd(&s), ymd(&e));
            Some(if sd == ed {
                sd
            } else {
                format!("{sd} → {ed}")
            })
        }
        (Some(s), None) => Some(ymd(&s)),
        (None, Some(e)) => Some(ymd(&e)),
        (None, None) => None,
    }
}

/// A compact one-line provenance summary (`Software: X · N pages · dates`) as
/// plain text for collection member listings. `None` when nothing is known.
/// The view wraps it in a `.prov` element and handles escaping.
pub(super) fn provenance_summary(c: &Wacz) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if !c.software.is_empty() {
        parts.push(format!("Software: {}", c.software.join(", ")));
    }
    if let Some(n) = c.page_count {
        parts.push(format!("{n} page{}", if n == 1 { "" } else { "s" }));
    }
    if let Some(range) = capture_range(c) {
        parts.push(range);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

pub(super) fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(super) fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Normalize a timestamp to the 14-digit form wabac.js expects. Seed pages in
/// `pages.jsonl` carry ISO 8601 timestamps (`2026-06-09T21:34:06.891Z`); wabac
/// wants `20260609213406`. Extract the digits and take the first 14.
pub(super) fn ts_to_14digit(ts: &str) -> String {
    ts.chars().filter(|c| c.is_ascii_digit()).take(14).collect()
}

/// Convert a 14-digit capture timestamp (`YYYYMMDDHHMMSS`) to ISO 8601
/// (`YYYY-MM-DDTHH:MM:SSZ`) so wabac's `new Date(ts)` in the pages list parses
/// it (the index stores the 14-digit form). Anything not 14 digits is returned
/// unchanged (already ISO, or empty).
pub(super) fn ts_to_iso(ts: &str) -> String {
    if ts.len() == 14 && ts.bytes().all(|b| b.is_ascii_digit()) {
        format!(
            "{}-{}-{}T{}:{}:{}Z",
            &ts[0..4],
            &ts[4..6],
            &ts[6..8],
            &ts[8..10],
            &ts[10..12],
            &ts[12..14],
        )
    } else {
        ts.to_string()
    }
}

pub(super) fn format_timestamp(ts: &str) -> String {
    if ts.len() >= 14 {
        format!(
            "{}-{}-{} {}:{}",
            &ts[0..4],
            &ts[4..6],
            &ts[6..8],
            &ts[8..10],
            &ts[10..12]
        )
    } else {
        ts.to_string()
    }
}
