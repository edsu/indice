//! Deriving indexable values from raw input: language detection, and the
//! date/host/URL-token fields the schema stores.

/// The primary language subtag of an HTML `lang` attribute, lowercased
/// (`en-US` -> `en`). Empty when there's no usable value.
pub(super) fn primary_lang(lang: &str) -> String {
    lang.split(['-', '_'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Below this many bytes of body text, language detection is too unreliable to
/// trust.
const MIN_DETECT_BYTES: usize = 40;

/// whatlang only needs a modest sample to identify the dominant language, so we
/// cap the input to keep detection cheap on long pages (it runs per page that
/// lacks `<html lang>`, so cost matters at scale). A couple of KB is plenty.
pub(super) const DETECT_SAMPLE_BYTES: usize = 2048;

/// Detect a page's language from its body text as a fallback when `<html lang>`
/// is absent. Returns the ISO 639-1 subtag (e.g. `en`) to match the codes used
/// by declared `lang`, or `None` when the text is too short, detection is not
/// reliable, or whatlang's language has no 639-1 code. whatlang reports a single
/// dominant language, which fits our single-valued `lang` field.
pub(super) fn detect_lang(body: &str) -> Option<String> {
    let body = body.trim();
    if body.len() < MIN_DETECT_BYTES {
        return None;
    }
    // Detect on a bounded prefix rather than the whole body; truncate on a char
    // boundary so we never slice mid-UTF-8.
    let sample = if body.len() > DETECT_SAMPLE_BYTES {
        let mut end = DETECT_SAMPLE_BYTES;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        &body[..end]
    } else {
        body
    };
    let info = whatlang::detect(sample)?;
    if !info.is_reliable() {
        return None;
    }
    lang3_to_lang1(info.lang().code()).map(String::from)
}

/// Map an ISO 639-3 code (as whatlang reports) to its ISO 639-1 two-letter
/// subtag. The arms mirror whatlang's supported language set; a language with no
/// 639-1 code (or a new whatlang language not listed here) returns `None`, so we
/// never store a code inconsistent with the declared `lang` values (better a gap
/// than a bucket that won't unify).
fn lang3_to_lang1(code3: &str) -> Option<&'static str> {
    Some(match code3 {
        "eng" => "en",
        "spa" => "es",
        "por" => "pt",
        "fra" => "fr",
        "deu" => "de",
        "ita" => "it",
        "nld" => "nl",
        "rus" => "ru",
        "ukr" => "uk",
        "bel" => "be",
        "bul" => "bg",
        "ces" => "cs",
        "pol" => "pl",
        "hrv" => "hr",
        "srp" => "sr",
        "mkd" => "mk",
        "slv" => "sl",
        "ron" => "ro",
        "ell" => "el",
        "dan" => "da",
        "swe" => "sv",
        "nob" => "nb",
        "fin" => "fi",
        "hun" => "hu",
        "est" => "et",
        "lit" => "lt",
        "lav" => "lv",
        "tur" => "tr",
        "aze" => "az",
        "uzb" => "uz",
        "tuk" => "tk",
        "cat" => "ca",
        "epo" => "eo",
        "cmn" => "zh",
        "jpn" => "ja",
        "kor" => "ko",
        "vie" => "vi",
        "tha" => "th",
        "ind" => "id",
        "tgl" => "tl",
        "jav" => "jv",
        "mya" => "my",
        "khm" => "km",
        "ara" => "ar",
        "heb" => "he",
        "yid" => "yi",
        "pes" => "fa",
        "urd" => "ur",
        "hin" => "hi",
        "ben" => "bn",
        "guj" => "gu",
        "pan" => "pa",
        "mar" => "mr",
        "kan" => "kn",
        "tam" => "ta",
        "tel" => "te",
        "mal" => "ml",
        "ori" => "or",
        "nep" => "ne",
        "sin" => "si",
        "kat" => "ka",
        "hye" => "hy",
        "amh" => "am",
        "zul" => "zu",
        "aka" => "ak",
        _ => return None,
    })
}

/// The four-digit crawl year parsed from a 14-digit page timestamp
/// (`20210417...` -> `2021`). `None` when the timestamp is missing or does not
/// start with a plausible year.
pub(super) fn year_of(timestamp: &str) -> Option<u64> {
    let year: u64 = timestamp.get(..4)?.parse().ok()?;
    (1000..=9999).contains(&year).then_some(year)
}

/// The six-digit crawl month `YYYYMM` parsed from a 14-digit page timestamp
/// (`20210417...` -> `202104`). `None` when the year or month is implausible.
pub(super) fn month_of(timestamp: &str) -> Option<u64> {
    let s = timestamp.get(..6)?;
    let year: u64 = s.get(..4)?.parse().ok()?;
    let month: u64 = s.get(4..6)?.parse().ok()?;
    ((1000..=9999).contains(&year) && (1..=12).contains(&month)).then_some(year * 100 + month)
}

/// The exact host of a URL, lowercased (e.g. `https://Example.com/a` -> `example.com`).
/// Empty when the URL has no host (relative paths, `urn:`, unparseable input).
pub(super) fn domain_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .unwrap_or_default()
}

/// The registrable domain (eTLD+1) of a URL, via the Public Suffix List, so a
/// whole site unifies across subdomains and multi-level suffixes are handled
/// correctly (`www.example.co.uk` -> `example.co.uk`, `a.github.io` ->
/// `a.github.io` since `github.io` is a private suffix). Empty when there's no
/// host or no registrable domain (e.g. a bare public suffix, `urn:`).
pub(crate) fn site_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .and_then(|host| psl::domain_str(&host).map(|d| d.to_string()))
        .unwrap_or_default()
}

/// A searchable text rendering of a URL: the host and path with separators
/// turned into spaces, so the default tokenizer indexes each word. For example
/// `https://github.com/DocNow/hydrator` yields `github.com DocNow hydrator`,
/// making a search for `hydrator` match the page.
pub(super) fn url_search_text(url: &str) -> String {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return String::new(),
    };
    let mut parts: Vec<&str> = Vec::new();
    if let Some(host) = parsed.host_str() {
        parts.push(host);
    }
    // Split the path on `/` and keep non-empty segments (drops the leading `/`).
    parts.extend(parsed.path().split('/').filter(|s| !s.is_empty()));
    parts.join(" ")
}
