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
use std::time::{Duration, SystemTime};

use anyhow::Result;

use crate::collections::{Source, Wacz};
use crate::index::{Resolved, SourceResolver};

/// How long before its expiry a URL stops being handed out, at most, so that
/// a reader given one has that long to use it. A reader's next visit fetches
/// the manifest again and takes the fresh URLs from it, but wabac does not
/// recover from a URL that stops working during a visit, before it has opened
/// the file: it then gives up on the file rather than fetch the manifest
/// again. So this is as long as a reader may keep a replay open. With
/// Browsertrix's 48 hours, a URL is replaced after 36. A URL with less time
/// left keeps a quarter of it as its margin, so that one issued for a few
/// hours is still cached for most of them.
const MARGIN: Duration = Duration::from_secs(12 * 3600);

/// How long to keep a URL whose expiry the service didn't give, comfortably
/// under Browsertrix's ~48 hours.
const UNDATED: Duration = Duration::from_secs(24 * 3600);

/// Resolved URLs by source location, each with when to stop handing it out.
///
/// The deadlines are wall-clock times, as the expiries they come from are: a
/// monotonic clock stops while the machine sleeps, so a workstation that slept
/// through a URL's expiry would go on handing it out.
#[derive(Default)]
pub(super) struct PresignedUrls {
    urls: Mutex<HashMap<String, (String, SystemTime)>>,
}

impl PresignedUrls {
    /// `source`'s URL, if one is cached and not near its expiry.
    fn get(&self, source: &Source) -> Option<String> {
        let urls = self.urls.lock().unwrap();
        let (url, until) = urls.get(&source.location())?;
        (SystemTime::now() < *until).then(|| url.clone())
    }

    /// Keep every URL a resolve turned up, and drop those past their time, so
    /// the cache holds no more than the URLs still worth handing out.
    fn insert_all(&self, resolved: &[Resolved], now: SystemTime) {
        let mut urls = self.urls.lock().unwrap();
        urls.retain(|_, (_, until)| *until > now);
        for r in resolved {
            urls.insert(
                r.source.location(),
                (r.url.clone(), good_until(r.expires, now)),
            );
        }
    }
}

/// When to stop handing out a URL that expires at `expires`: its margin
/// before it ([`MARGIN`], or a quarter of the time it has left if less), `now`
/// if it has expired, or [`UNDATED`] from `now` with no expiry given.
fn good_until(expires: Option<SystemTime>, now: SystemTime) -> SystemTime {
    match expires {
        Some(at) => {
            let left = at.duration_since(now).unwrap_or(Duration::ZERO);
            now + left - MARGIN.min(left / 4)
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

/// What one lookup covers: a source's location but for its file name, which
/// is the Browsertrix item or public collection the file is in.
fn lookup_of(location: &str) -> &str {
    location.rsplit_once('|').map_or(location, |(l, _)| l)
}

/// Resolve `source` and every URL its lookup turns up, caching them all. The
/// resolve is a blocking HTTP call, so it runs on a blocking thread, where a
/// slow Browsertrix can't hold up the server's other requests. The URLs are
/// cached on that thread too, so they are kept even if the request that asked
/// for them is gone by the time they arrive.
async fn resolve(
    cache: &Arc<PresignedUrls>,
    resolver: &Arc<dyn SourceResolver>,
    source: &Source,
) -> Result<Vec<Resolved>> {
    let (cache, resolver, s) = (cache.clone(), resolver.clone(), source.clone());
    tokio::task::spawn_blocking(move || {
        let resolved = resolver.resolve_all(&s)?;
        cache.insert_all(&resolved, SystemTime::now());
        Ok(resolved)
    })
    .await?
}

/// A presigned URL for `w`, from the cache or, failing that, the resolver.
///
/// A URL just resolved is returned even if it is already within its margin of
/// expiring, since there is none fresher; it is only not served again from
/// the cache.
pub(super) async fn url_for(
    cache: &Arc<PresignedUrls>,
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

/// The cached presigned URLs of those of `files` that have one, by source
/// location, resolving nothing. For a collection's manifest, which may span
/// many Browsertrix items, one lookup each: rather than make them all before
/// answering, for files wabac mostly never opens, a file with no URL cached
/// goes by `/files/{id}`, which resolves it when wabac does open it, and
/// caches the rest of its item with it.
pub(super) fn cached_urls(cache: &PresignedUrls, files: &[&Wacz]) -> HashMap<String, String> {
    files
        .iter()
        .filter(|w| is_presigned(w))
        .filter_map(|w| Some((w.source.location(), cache.get(&w.source)?)))
        .collect()
}

/// Presigned URLs for those of `files` that have one, by source location, in
/// as few resolves as they need. For a crawl's manifest, whose files are in one
/// Browsertrix item or collection, so one lookup. What a lookup turns up
/// serves every file it covers, however near expiry, and a lookup is made once
/// at most, so files it didn't turn up, or a Browsertrix that is down, don't
/// cost a call each. A file missing from the result has no URL to be had.
pub(super) async fn urls_for(
    cache: &Arc<PresignedUrls>,
    resolver: &Arc<dyn SourceResolver>,
    files: &[&Wacz],
) -> HashMap<String, String> {
    let mut urls = cached_urls(cache, files);
    let mut tried = HashSet::new();
    for w in files.iter().filter(|w| is_presigned(w)) {
        let location = w.source.location();
        if urls.contains_key(&location) || !tried.insert(lookup_of(&location).to_string()) {
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
            }
        }
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn a_url_is_handed_out_until_its_margin_before_it_expires() {
        let now = SystemTime::now();
        // Browsertrix's 48 hours: twelve hours' margin.
        assert_eq!(good_until(Some(now + 48 * HOUR), now), now + 36 * HOUR);
        // Six hours: a quarter of them, so still cached for most.
        assert_eq!(
            good_until(Some(now + 6 * HOUR), now),
            now + 6 * HOUR - 6 * HOUR / 4
        );
        // Past expiry: not handed out again.
        assert_eq!(good_until(Some(now - HOUR), now), now);
        // No expiry given: a day.
        assert_eq!(good_until(None, now), now + UNDATED);
    }

    fn source(resource: &str) -> Source {
        Source::BrowsertrixPublic {
            host: "https://bt.example".into(),
            org: "o".into(),
            collection: "c".into(),
            resource: resource.into(),
        }
    }

    fn resolved(resource: &str, expires: SystemTime) -> Resolved {
        Resolved {
            source: source(resource),
            url: format!("https://s3.example/{resource}?sig"),
            expires: Some(expires),
        }
    }

    #[test]
    fn one_resolve_caches_every_url_it_turns_up() {
        let cache = PresignedUrls::default();
        let now = SystemTime::now();
        cache.insert_all(
            &[resolved("a.wacz", now + 48 * HOUR), resolved("b.wacz", now)],
            now,
        );
        assert_eq!(
            cache.get(&source("a.wacz")).as_deref(),
            Some("https://s3.example/a.wacz?sig")
        );
        // Expired: not handed out.
        assert_eq!(cache.get(&source("b.wacz")), None);
        assert_eq!(cache.get(&source("c.wacz")), None);
    }

    #[test]
    fn urls_past_their_time_are_dropped() {
        let cache = PresignedUrls::default();
        let then = SystemTime::now() - 48 * HOUR;
        cache.insert_all(&[resolved("old.wacz", then + HOUR)], then);
        cache.insert_all(
            &[resolved("new.wacz", SystemTime::now() + 48 * HOUR)],
            SystemTime::now(),
        );
        let urls = cache.urls.lock().unwrap();
        assert!(urls.contains_key(&source("new.wacz").location()));
        assert!(!urls.contains_key(&source("old.wacz").location()));
    }
}
