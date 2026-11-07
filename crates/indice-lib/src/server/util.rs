//! Small shared helpers: formatting, escaping, replay-link building, and the
//! provenance/capture summaries the pages render.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::time::Duration;

use crate::collections::{Manifest, Wacz};
use crate::i18n::{FluentArg, L10n};

use super::*;

/// Format a byte count for display (e.g. `48.2 MB`) in English, for the CLI and
/// log lines. Pages use [`crate::i18n::L10n::size`] with the reader's language;
/// this is the same code with the English catalog, so the two cannot drift.
pub fn human_size(bytes: u64) -> String {
    crate::i18n::L10n::default().size(bytes)
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

/// What a replay link replays: one crawl's WACZ, or a whole collection's
/// crawls merged into one (see `collection_replay_json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReplayScope<'a> {
    Crawl(&'a str),
    Collection(&'a str),
}

impl ReplayScope<'_> {
    /// The page the replay links sit under: `/crawl/{id}` or `/collection/{id}`.
    pub(super) fn base(&self) -> String {
        match self {
            ReplayScope::Crawl(id) => format!("/crawl/{}", url_encode(id)),
            ReplayScope::Collection(id) => format!("/collection/{}", url_encode(id)),
        }
    }
}

/// The address that replays `url` as captured at `ts` within `scope`:
/// `/crawl/{id}/{ts}/{url}` or `/collection/{id}/{ts}/{url}`, the archived URL
/// written out as a browser requests it (`url_in_path`), scheme and query
/// included, in the manner of Wayback-style archives. The server works out
/// everything else the viewer needs (`replay_page`), so the link says only
/// what is being read. A `ts` with no digits is left out and replay picks the
/// capture. `None` for a URL replay can't open.
pub(super) fn replay_href(scope: ReplayScope, ts: &str, url: &str) -> Option<String> {
    let ts = ts_to_14digit(ts);
    let url = url_in_path(url)?;
    Some(if ts.is_empty() {
        format!("{}/{url}", scope.base())
    } else {
        format!("{}/{ts}/{url}", scope.base())
    })
}

/// An archived URL as the tail of a replay path, as a browser would request
/// it: parsed and serialized by the URL standard, which lowercases the scheme
/// and host, percent-encodes what a link can't hold (spaces, quotes,
/// non-ASCII), and resolves `.` and `..` segments. That last one matters: a
/// browser resolves dot segments in a link before requesting it, `%2e%2e`
/// included, so a `..` left in the tail would climb out of the replay path to
/// any address on the site. The fragment is dropped, since it never reaches a
/// server. A `?` stays, and the server takes the query back as part of the
/// URL. `None` for what isn't an http(s) URL, which replay can't open.
pub(super) fn url_in_path(url: &str) -> Option<String> {
    let mut u = url::Url::parse(url.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    u.set_fragment(None);
    Some(u.into())
}

/// What a replay path names: one crawl, or a whole collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReplayKind {
    Crawl,
    Collection,
}

/// A replay path taken apart: what it replays, the capture time, and the
/// archived URL. `path` is the request's raw path and `query` its raw query,
/// which belongs to the archived URL. `None` when it isn't a replay path.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ReplayTarget {
    pub kind: ReplayKind,
    pub id: String,
    pub ts: String,
    pub url: String,
}

/// The sub-pages of a collection that sit where a replay path's time would.
const COLLECTION_PAGES: &[&str] = &["replay.json", "pages", "annotations"];

/// The same for a crawl.
const CRAWL_PAGES: &[&str] = &["replay.json", "pages"];

/// The kind, id and the rest of a path under `/crawl/{id}/` or
/// `/collection/{id}/` that could be a replay path: anything beyond the
/// crawl's or collection's own pages. Ids are slugs or hex, so they are used
/// as they come.
fn replay_shaped(path: &str) -> Option<(ReplayKind, &str, &str)> {
    let rest = path.strip_prefix('/')?;
    let (kind, rest) = rest.split_once('/')?;
    let kind = match kind {
        "crawl" => ReplayKind::Crawl,
        "collection" => ReplayKind::Collection,
        _ => return None,
    };
    let (id, rest) = rest.split_once('/')?;
    let own_pages = match kind {
        ReplayKind::Crawl => CRAWL_PAGES,
        ReplayKind::Collection => COLLECTION_PAGES,
    };
    if own_pages.contains(&rest) {
        return None;
    }
    (!id.is_empty()).then_some((kind, id, rest))
}

pub(super) fn parse_replay_path(path: &str, query: Option<&str>) -> Option<ReplayTarget> {
    let (kind, id, rest) = replay_shaped(path)?;
    // A capture time is all digits, as many as the reader knows (`2025` is
    // fine: replay picks the nearest capture), and up to the 17 of a
    // Wayback-style link, of which replay reads 14. Anything else starts the
    // URL.
    let (ts, url) = match rest.split_once('/') {
        Some((ts, url))
            if (1..=17).contains(&ts.len()) && ts.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (ts[..ts.len().min(14)].to_string(), url)
        }
        _ => (String::new(), rest),
    };
    // Something on the way may fold the `//` after the scheme to one slash.
    let mut url = url.to_string();
    for scheme in ["http:/", "https:/"] {
        if url.len() > scheme.len()
            && url[..scheme.len()].eq_ignore_ascii_case(scheme)
            && !url[scheme.len()..].starts_with('/')
        {
            url.insert(scheme.len(), '/');
        }
    }
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }
    // As a link would carry it (`url_in_path`): the tail's escapes are the
    // URL's own, and replay treats `%20` and a space alike.
    let url = url_in_path(&url)?;
    Some(ReplayTarget {
        kind,
        id: id.to_string(),
        ts,
        url,
    })
}

/// A request path as the log records it. A replay path names the page a
/// reader is reading, so the log keeps only which crawl or collection, as it
/// keeps a search's path and not its query. Anything shaped like one is cut,
/// whether or not it would replay.
pub(super) fn loggable_path(path: &str) -> std::borrow::Cow<'_, str> {
    match replay_shaped(path) {
        Some((ReplayKind::Crawl, id, _)) => format!("/crawl/{id}/…").into(),
        Some((ReplayKind::Collection, id, _)) => format!("/collection/{id}/…").into(),
        None => path.into(),
    }
}

/// The address that replays a whole collection, opening on `start` (a page's
/// url and timestamp, usually `collection_default_page`). `None` without a
/// start page: replay with no page to open shows nothing, so there is
/// nothing to link to.
pub(super) fn collection_replay_href(id: &str, start: Option<(String, String)>) -> Option<String> {
    start.and_then(|(url, ts)| replay_href(ReplayScope::Collection(id), &ts, &url))
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
/// Labels are in the reader's language.
pub(super) fn month_axis(
    l10n: &crate::i18n::L10n,
    counts: &[(u64, u64)],
    href: impl Fn(u64) -> String,
    active: impl Fn(u64) -> bool,
) -> Vec<crate::views::TimelineBar> {
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
                label: l10n.month_year(month as u32, (ym / 100) as i32),
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
pub(super) fn date_span(l10n: &L10n, start: Option<&str>, end: Option<&str>) -> Option<String> {
    let words = |ts: &str| l10n.long_date(ts).unwrap_or_else(|| ymd(ts));
    match (start, end) {
        (Some(s), Some(e)) => {
            let (sd, ed) = (words(s), words(e));
            Some(if sd == ed {
                sd
            } else {
                l10n.tr_args(
                    "date-span",
                    &[("start", FluentArg::Str(&sd)), ("end", FluentArg::Str(&ed))],
                )
            })
        }
        (Some(t), None) | (None, Some(t)) => Some(words(t)),
        (None, None) => None,
    }
}

/// When a crawl's captures were made, or `None` when none were recorded.
pub(super) fn capture_range(l10n: &L10n, c: &Wacz) -> Option<String> {
    date_span(l10n, c.capture_start.as_deref(), c.capture_end.as_deref())
}

/// A compact "capture quality" summary of an HTTP status histogram: total
/// captures, the share that succeeded (2xx/3xx), and the notable failing codes.
/// The derived DACS Appraisal signal — surfaces the 404/403/504 "absences" that
/// a clean-looking crawl can hide.
pub(super) fn capture_quality(
    l10n: &L10n,
    counts: &std::collections::BTreeMap<u16, u64>,
) -> Option<String> {
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
    let summary = l10n.tr_args(
        "capture-quality",
        &[
            (
                "count",
                FluentArg::Num(total.try_into().unwrap_or(i64::MAX)),
            ),
            ("shown", FluentArg::Str(&l10n.number(total))),
            ("pct", FluentArg::Num(ok_pct as i64)),
        ],
    );
    // Notable failing codes (>= 400), most frequent first.
    let mut bad: Vec<(u16, u64)> = counts
        .iter()
        .filter(|(c, _)| **c >= 400)
        .map(|(c, n)| (*c, *n))
        .collect();
    bad.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if bad.is_empty() {
        return Some(summary);
    }
    // Codes and counts are digits, written the way the status line writes them.
    let codes: Vec<String> = bad
        .iter()
        .take(4)
        .map(|(c, n)| format!("{c}×{n}"))
        .collect();
    Some(l10n.tr_args(
        "capture-quality-failures",
        &[
            ("summary", FluentArg::Str(&summary)),
            ("codes", FluentArg::Str(&codes.join(", "))),
        ],
    ))
}

/// Map a Browsertrix QA review rating (1–5) to its label, with the raw value —
/// a DACS Appraisal signal ("this crawl was reviewed and judged …").
pub(super) fn review_label(l10n: &L10n, rating: u8) -> String {
    let word = match rating {
        1..=5 => l10n.tr(&format!("review-{rating}")),
        _ => l10n.tr("review-other"),
    };
    l10n.tr_args(
        "review-label",
        &[
            ("word", FluentArg::Str(&word)),
            ("rating", FluentArg::Str(&rating.to_string())),
        ],
    )
}

/// Names as a list in words: "A", "A and B", "A, B and C".
pub(super) fn list_and(l10n: &L10n, items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [head @ .., last] => l10n.tr_args(
            "list-and",
            &[
                ("first", FluentArg::Str(&head.join(", "))),
                ("last", FluentArg::Str(last)),
            ],
        ),
    }
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
pub(super) fn members_capture_range(l10n: &L10n, members: &[&Wacz]) -> Option<String> {
    let start = members.iter().filter_map(|w| w.capture_start.clone()).min();
    let end = members.iter().filter_map(|w| w.capture_end.clone()).max();
    date_span(l10n, start.as_deref(), end.as_deref())
}

/// A one-sentence provenance summary for a collection's list of crawls: "38
/// pages, by Browsertrix-Crawler 1.4.2." The list shows each crawl's date
/// above it, so the sentence leaves it out. `None` when nothing is known. The
/// view escapes it.
pub(super) fn provenance_summary(l10n: &L10n, c: &Wacz) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(n) = c.page_count {
        parts.push(counted_message(l10n, "crawl-summary-pages", n));
    }
    if !c.software.is_empty() {
        parts.push(l10n.tr_args(
            "crawl-summary-by",
            &[("software", FluentArg::Str(&list_and(l10n, &c.software)))],
        ));
    }
    let mut sentence = parts.join(", ");
    let first = sentence.chars().next()?;
    sentence.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
    sentence.push('.');
    Some(sentence)
}

/// A plural message (`$count`, `$shown`) for `n`, with the number in the
/// language's own grouping.
fn counted_message(l10n: &L10n, key: &str, n: u64) -> String {
    l10n.tr_args(
        key,
        &[
            ("count", FluentArg::Num(n.try_into().unwrap_or(i64::MAX))),
            ("shown", FluentArg::Str(&l10n.number(n))),
        ],
    )
}

/// A date for a facts table: in words when the timestamp is a date, as given
/// otherwise.
pub(super) fn day_in_words(l10n: &L10n, ts: &str) -> String {
    l10n.long_date(ts)
        .unwrap_or_else(|| ts.get(..10).unwrap_or(ts).to_string())
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
    ts_iso(ts).unwrap_or_else(|| ts.to_string())
}

/// A 14-digit timestamp as ISO 8601, or `None` for any other form.
pub(super) fn ts_iso(ts: &str) -> Option<String> {
    (ts.len() == 14 && ts.bytes().all(|b| b.is_ascii_digit())).then(|| {
        format!(
            "{}-{}-{}T{}:{}:{}Z",
            &ts[0..4],
            &ts[4..6],
            &ts[6..8],
            &ts[8..10],
            &ts[10..12],
            &ts[12..14],
        )
    })
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
