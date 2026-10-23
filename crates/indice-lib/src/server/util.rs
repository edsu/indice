//! Small shared helpers: formatting, escaping, replay-link building, and the
//! provenance/capture summaries the pages render.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::time::Duration;

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

/// The `source` value to hand ReplayWeb.page for a crawl: always its
/// `/files/{id}`, whatever the crawl's source. A file is served from there; a
/// remote URL or a Browsertrix crawl is redirected from there to where its
/// bytes are (`serve_file`). The replay origin replays only this server's own
/// paths (`static/replay/guard.js`), so a remote URL handed over directly
/// would be refused.
pub(super) fn viewer_source(col: &Wacz) -> String {
    format!("/files/{}", col.id)
}

/// The viewer URL that replays a whole collection (multi-WACZ): wabac loads the
/// collection's `replay.json` manifest (see `collection_replay_json`) as one
/// merged collection, cached under the collection's id, which the frame takes
/// from the manifest's path. The rest are breadcrumb params.
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
        "/replay/viewer?source={}&name={}&collection={}&collection_id={}",
        url_encode(&source),
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

/// The longest search query indice will accept.
///
/// `q` arrives from anonymous callers on `/search`, `/api/search` and a
/// collection's page list, and goes straight to Tantivy's parser. Parsing is
/// cheap per byte but not free per *term*, and every term is a dictionary
/// lookup against the index, so a megabyte of `q` buys a megabyte of work for
/// the cost of typing a URL. Until now the only thing stopping that was
/// hyper's limit on the request line, which is an accident of the HTTP stack
/// rather than a decision indice made.
///
/// 2 KiB is far more than a person types — roughly 300 words, or sixty
/// `field:value` filters — and far less than a weapon.
pub(super) const MAX_QUERY_LEN: usize = 2048;

/// The refusal for an over-length query, or `None` to carry on.
///
/// Returns the response rather than an error, because an over-length query is
/// not a failure to handle: it is a handled request whose answer is no. It also
/// keeps clippy's `result_large_err` quiet honestly, since a `Response` really
/// is a large thing to put in an `Err`.
///
/// The message names the limit, so a client can tell this apart from a query
/// that simply matched nothing.
pub(super) fn query_too_long(q: &str) -> Option<Response> {
    (q.len() > MAX_QUERY_LEN).then(|| {
        (
            StatusCode::BAD_REQUEST,
            format!(
                "search query too long: {} bytes, limit {MAX_QUERY_LEN}",
                q.len()
            ),
        )
            .into_response()
    })
}

/// A random id for one request, so a reader who was told "something went
/// wrong" can be matched to the log lines that say what.
///
/// Random rather than a counter, so it says nothing about how busy the server
/// is. 64 bits, so two requests in the same log do not share one.
#[derive(Clone, Copy, Debug)]
pub(super) struct RequestId(u64);

impl RequestId {
    fn new() -> Self {
        use std::hash::BuildHasher;
        use std::sync::OnceLock;
        // A counter hashed under a key chosen at random once per process:
        // unique because the counter is, unpredictable because the key is.
        // `RandomState` is the standard library's randomly keyed hasher, so this
        // needs no new dependency.
        static KEY: OnceLock<std::hash::RandomState> = OnceLock::new();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        RequestId(KEY.get_or_init(std::hash::RandomState::new).hash_one(n))
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

tokio::task_local! {
    /// The id of the request this task is serving. A task-local rather than a
    /// parameter, so [`error_response`] can name it from any handler without
    /// every handler passing it down.
    static REQUEST_ID: RequestId;
}

/// Give each request an id: in its extensions, where the trace span reads it so
/// every line logged while handling the request carries it, and in
/// [`REQUEST_ID`] for the answer to quote. Runs outside the trace layer, so the
/// id exists by the time the span is made.
pub(super) async fn assign_request_id(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let id = RequestId::new();
    req.extensions_mut().insert(id);
    REQUEST_ID.scope(id, next.run(req)).await
}

/// `message`, with the request's reference when there is one, as a sentence.
pub(super) fn with_reference(message: &str) -> String {
    match REQUEST_ID.try_with(|id| *id) {
        Ok(id) => format!("{message} (reference {id})."),
        Err(_) => format!("{message}."),
    }
}

/// The answer when a request's work failed: a `500` that says so, and the
/// request id, but not the error.
///
/// Errors here carry context such as `reading /srv/indice/index/waczs.json`,
/// which tells a stranger where the server keeps its files. The whole chain goes
/// to the log instead, under the request's id, which the answer quotes so the
/// operator can find it. A handler serving someone who may change the archive
/// uses [`Curator::error_response`] instead, which says what went wrong.
pub(super) fn error_response(e: anyhow::Error) -> Response {
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Something went wrong on the server",
        e,
    )
}

/// As [`error_response`], with another status and what to tell the reader, such
/// as a `502` that says which service failed.
pub(super) fn failure(status: StatusCode, message: &str, e: anyhow::Error) -> Response {
    tracing::error!("{e:#}");
    (status, with_reference(message)).into_response()
}

/// How long a request-path write waits for the cross-process index lock before
/// giving up and telling the client to come back.
///
/// Background jobs and the CLI block instead — queueing is right for them. A
/// request is different: a rebuild can hold the lock for hours, and a browser
/// (and the thread serving it) must not wait that long.
pub(super) const REQUEST_LOCK_WAIT: Duration = Duration::from_secs(10);

/// How long to tell a client to wait before retrying.
///
/// Nothing knows the true remaining time — the holder records when it started,
/// not how long it will take — so this is a guess. It is derived from
/// [`REQUEST_LOCK_WAIT`] rather than written as a literal so the two cannot
/// drift: telling a client to come back sooner than the next attempt's own
/// budget would just build a queue of parked threads.
fn retry_after_secs() -> u64 {
    // `as_secs` floors, so a sub-second budget would send `Retry-After: 0` and
    // invite clients to hammer — the queue of parked threads this is meant to
    // avoid. At least a second, always.
    ((REQUEST_LOCK_WAIT.as_secs_f64() * 3.0).ceil() as u64).max(1)
}

/// 503 for a write that could not get the index lock in time.
pub(super) fn busy_response(holder: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("Retry-After", retry_after_secs().to_string())],
        format!(
            "The search index is busy: {holder} is running. \
             Your change was not saved — try again shortly."
        ),
    )
        .into_response()
}

/// A request-path write's result, or the reason it never ran.
///
/// Lets a handler keep its own outcome type (`UpdateResult`, `EditOutcome`, …)
/// and add "the index was busy" without threading a variant through it.
pub(super) enum Write<T> {
    Done(T),
    Busy(String),
}

/// Take the index lock for a request-path write, or say who has it.
///
/// **Call this before `AppState.write_lock`, never inside it.** Waiting on a
/// cross-process lock while holding the in-process mutex would hang every other
/// workroom write behind this one, and once the library functions this handler
/// calls take the index lock too, the inverted order is an outright deadlock.
/// See `index::lock`'s ordering rule.
pub(super) fn acquire_index_for_request(
    home: &std::path::Path,
    what: &str,
) -> anyhow::Result<Result<crate::index::lock::IndexLock, String>> {
    Ok(
        match crate::index::lock::lock_index_within(home, what, REQUEST_LOCK_WAIT)? {
            crate::index::lock::Acquisition::Held(l) => Ok(l),
            crate::index::lock::Acquisition::Busy { holder } => Err(holder),
        },
    )
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

/// A time axis from `(YYYYMM, count)` pairs in any order: every month from the
/// first to the last, empty months included, so gaps show. `href` gives a
/// month's link and `active` whether it is the filter in force. At most the
/// last ten years, so a stray old capture cannot squeeze the rest to nothing.
pub(super) fn month_axis(
    counts: &[(u64, u64)],
    href: impl Fn(u64) -> String,
    active: impl Fn(u64) -> bool,
) -> Vec<crate::views::TimelineBar> {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let valid = |ym: u64| (1..=12).contains(&(ym % 100));
    let mut by_month: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
    for &(ym, n) in counts.iter().filter(|(ym, _)| valid(*ym)) {
        *by_month.entry(ym).or_default() += n;
    }
    let (Some(&first), Some(&last)) = (by_month.keys().next(), by_month.keys().next_back()) else {
        return Vec::new();
    };
    let index = |ym: u64| (ym / 100) * 12 + (ym % 100 - 1);
    let start = index(first).max(index(last).saturating_sub(119));
    // Scaled to the busiest month shown, not one the window cut off.
    let in_view = |ym: u64| index(ym) >= start;
    let max = by_month
        .iter()
        .filter(|(ym, _)| in_view(**ym))
        .map(|(_, n)| *n)
        .max()
        .unwrap_or(1)
        .max(1);
    (start..=index(last))
        .map(|i| {
            let ym = (i / 12) * 100 + i % 12 + 1;
            let count = by_month.get(&ym).copied().unwrap_or(0);
            let month = (ym % 100) as usize;
            crate::views::TimelineBar {
                label: format!("{} {}", MONTHS[month - 1], ym / 100),
                year: (i == start || month == 1).then(|| (ym / 100).to_string()),
                count,
                step: if count == 0 {
                    0
                } else {
                    (count * 10).div_ceil(max).clamp(1, 10) as u8
                },
                href: (count > 0).then(|| href(ym)),
                active: active(ym),
            }
        })
        .collect()
}

/// Two timestamps as a span of dates in words: "7 January 2025 to 3 August
/// 2026", or one date when they fall on the same day. A timestamp that is not
/// a date is shown as it is.
fn date_span(start: Option<&str>, end: Option<&str>) -> Option<String> {
    let words = |ts: &str| crate::collections::long_date(ts).unwrap_or_else(|| ymd(ts));
    match (start, end) {
        (Some(s), Some(e)) => {
            let (sd, ed) = (words(s), words(e));
            Some(if sd == ed {
                sd
            } else {
                format!("{sd} to {ed}")
            })
        }
        (Some(t), None) | (None, Some(t)) => Some(words(t)),
        (None, None) => None,
    }
}

/// When a crawl's captures were made, or `None` when none were recorded.
pub(super) fn capture_range(c: &Wacz) -> Option<String> {
    date_span(c.capture_start.as_deref(), c.capture_end.as_deref())
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
    date_span(start.as_deref(), end.as_deref())
}

/// A one-sentence provenance summary for a collection's list of crawls: "38
/// pages, by Browsertrix-Crawler 1.4.2." The list shows each crawl's date
/// above it, so the sentence leaves it out. `None` when nothing is known. The
/// view escapes it.
pub(super) fn provenance_summary(c: &Wacz) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(n) = c.page_count {
        parts.push(format!("{n} page{}", if n == 1 { "" } else { "s" }));
    }
    if !c.software.is_empty() {
        parts.push(format!("by {}", c.software.join(" and ")));
    }
    let mut sentence = parts.join(", ");
    let first = sentence.chars().next()?;
    sentence.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
    sentence.push('.');
    Some(sentence)
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
