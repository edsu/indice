//! Presigned URLs for streamed Browsertrix crawls, which replay reads WACZ
//! bytes from directly.
//!
//! A Browsertrix WACZ has no lasting URL: Browsertrix hands out presigned S3
//! URLs that work for about 48 hours. The server resolves them through the
//! binary's [`SourceResolver`] and caches them, keyed by source, until
//! [`MARGIN`] before they expire. One resolve returns the URLs of a whole
//! collection or item (`SourceResolver::resolve_all`), and all of them are
//! kept, so replaying a crawl of many files costs one call to Browsertrix, not
//! one per file.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;

use crate::collections::{Source, Wacz};
use crate::index::{Resolved, SourceResolver};

/// How long before its expiry a URL stops being handed out, so that a reader
/// given one has that long to use it. A reader's next visit fetches the
/// manifest again and takes the fresh URLs from it, but wabac does not
/// recover from a URL that stops working during a visit, before it has opened
/// the file: it then gives up on the file rather than fetch the manifest
/// again. So this is as long as a reader may keep a replay open. With
/// Browsertrix's 48 hours, a URL is replaced after 36.
const MARGIN: Duration = Duration::from_secs(12 * 3600);

/// How long to keep a URL whose expiry the service didn't give, comfortably
/// under Browsertrix's ~48 hours.
const UNDATED: Duration = Duration::from_secs(24 * 3600);

/// Resolved URLs by source location, each with when to stop handing it out.
#[derive(Default)]
pub(super) struct PresignedUrls {
    urls: Mutex<HashMap<String, (String, Instant)>>,
}

impl PresignedUrls {
    /// `source`'s URL, if one is cached and not near its expiry.
    fn get(&self, source: &Source) -> Option<String> {
        let urls = self.urls.lock().unwrap();
        let (url, until) = urls.get(&source.location())?;
        (Instant::now() < *until).then(|| url.clone())
    }

    /// Keep every URL a resolve turned up.
    fn insert_all(&self, resolved: &[Resolved], now: Instant, wall: SystemTime) {
        let mut urls = self.urls.lock().unwrap();
        for r in resolved {
            urls.insert(
                r.source.location(),
                (r.url.clone(), good_until(r.expires, now, wall)),
            );
        }
    }
}

/// When to stop handing out a URL that expires at `expires`, as an instant:
/// [`MARGIN`] before it, `now` if that has passed, or [`UNDATED`] from `now`
/// with no expiry given. `wall` is the system time at `now`.
fn good_until(expires: Option<SystemTime>, now: Instant, wall: SystemTime) -> Instant {
    match expires {
        Some(at) => {
            let left = at.duration_since(wall).unwrap_or(Duration::ZERO);
            now + left.saturating_sub(MARGIN)
        }
        None => now + UNDATED,
    }
}

/// Whether replay reads `w`'s bytes from a presigned URL, rather than from
/// indice's `/files/{id}`.
pub(super) fn is_presigned(w: &Wacz) -> bool {
    matches!(
        w.source,
        Source::Browsertrix { .. } | Source::BrowsertrixPublic { .. }
    )
}

/// Resolve `source` and every URL its lookup turns up, caching them all. The
/// resolve is a blocking HTTP call, so it runs on a blocking thread, where a
/// slow Browsertrix can't hold up the server's other requests.
async fn resolve(
    cache: &PresignedUrls,
    resolver: &Arc<dyn SourceResolver>,
    source: &Source,
) -> Result<Vec<Resolved>> {
    let (resolver, s) = (resolver.clone(), source.clone());
    let resolved = tokio::task::spawn_blocking(move || resolver.resolve_all(&s)).await??;
    cache.insert_all(&resolved, Instant::now(), SystemTime::now());
    Ok(resolved)
}

/// A presigned URL for `w`, from the cache or, failing that, the resolver.
///
/// A URL just resolved is returned even if it is already within [`MARGIN`] of
/// expiring, since there is none fresher; it is only not served again from
/// the cache.
pub(super) async fn url_for(
    cache: &PresignedUrls,
    resolver: &Arc<dyn SourceResolver>,
    w: &Wacz,
) -> Result<String> {
    if let Some(url) = cache.get(&w.source) {
        return Ok(url);
    }
    resolve(cache, resolver, &w.source)
        .await?
        .into_iter()
        .find(|r| r.source == w.source)
        .map(|r| r.url)
        .ok_or_else(|| anyhow::anyhow!("{} not among the URLs resolved", w.source.location()))
}

/// Presigned URLs for those of `files` that have one, by source location, in
/// as few resolves as they need: what one resolve turns up serves every file
/// it covers, however near expiry, so a manifest of a whole collection costs
/// one lookup, not one per file. A failed lookup is not repeated for the other
/// files it would have covered, so a Browsertrix that is down costs one call.
/// A file missing from the result has no URL to be had.
pub(super) async fn urls_for(
    cache: &PresignedUrls,
    resolver: &Arc<dyn SourceResolver>,
    files: &[&Wacz],
) -> HashMap<String, String> {
    let mut urls = HashMap::new();
    let mut failed = HashSet::new();
    for w in files.iter().filter(|w| is_presigned(w)) {
        let location = w.source.location();
        if urls.contains_key(&location) {
            continue;
        }
        if let Some(url) = cache.get(&w.source) {
            urls.insert(location, url);
            continue;
        }
        // What one lookup covers: the source but for its file name.
        let lookup = location
            .rsplit_once('|')
            .map_or(location.as_str(), |(l, _)| l);
        if failed.contains(lookup) {
            continue;
        }
        match resolve(cache, resolver, &w.source).await {
            Ok(resolved) => {
                for r in resolved {
                    urls.entry(r.source.location()).or_insert(r.url);
                }
            }
            Err(e) => {
                tracing::warn!(crawl = %w.id, error = %format!("{e:#}"), "no presigned URL for replay; falling back to /files");
                failed.insert(lookup.to_string());
            }
        }
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_is_handed_out_until_twelve_hours_before_it_expires() {
        let (now, wall) = (Instant::now(), SystemTime::now());
        let in_48h = wall + Duration::from_secs(48 * 3600);
        assert_eq!(
            good_until(Some(in_48h), now, wall),
            now + Duration::from_secs(36 * 3600)
        );
        // Within the margin, or past expiry: not handed out again.
        let soon = wall + Duration::from_secs(11 * 3600);
        assert_eq!(good_until(Some(soon), now, wall), now);
        let gone = wall - Duration::from_secs(60);
        assert_eq!(good_until(Some(gone), now, wall), now);
        // No expiry given: a day.
        assert_eq!(good_until(None, now, wall), now + UNDATED);
    }

    #[test]
    fn one_resolve_caches_every_url_it_turns_up() {
        let source = |resource: &str| Source::BrowsertrixPublic {
            host: "https://bt.example".into(),
            org: "o".into(),
            collection: "c".into(),
            resource: resource.into(),
        };
        let cache = PresignedUrls::default();
        let wall = SystemTime::now();
        let resolved = [
            Resolved {
                source: source("a.wacz"),
                url: "https://s3.example/a?sig".into(),
                expires: Some(wall + Duration::from_secs(48 * 3600)),
            },
            Resolved {
                source: source("b.wacz"),
                url: "https://s3.example/b?sig".into(),
                expires: Some(wall + Duration::from_secs(60)),
            },
        ];
        cache.insert_all(&resolved, Instant::now(), wall);
        assert_eq!(
            cache.get(&source("a.wacz")).as_deref(),
            Some("https://s3.example/a?sig")
        );
        // Kept, but too close to expiry to hand out.
        assert_eq!(cache.get(&source("b.wacz")), None);
        assert_eq!(cache.get(&source("c.wacz")), None);
    }
}
