//! Interface translations: compiled-in Fluent catalogs and per-request language
//! negotiation. See DESIGN.md, "Interface languages".
//!
//! This module only turns keys into strings; rendering them stays with the views,
//! so the library keeps its "reports data, binary/views render" split.

use std::sync::LazyLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use unic_langid::LanguageIdentifier;

/// Every language indice ships, as (tag, catalog text). The tag is the whole
/// catalog tag (`zh-Hant`, not `zh`) and matches the file name. Adding a language
/// is one `.ftl` file plus one line here; English comes first and is the reference.
pub const CATALOGS: &[(&str, &str)] = &[
    ("en", include_str!("../../locales/en.ftl")),
    ("it", include_str!("../../locales/it.ftl")),
];

/// The language every other catalog falls back to.
const REFERENCE: &str = "en";

/// A header longer than this is ignored: real browsers send well under 200 bytes,
/// and negotiation cost grows with the number of tags.
const MAX_ACCEPT_LANGUAGE: usize = 1024;

type Bundle = FluentBundle<FluentResource>;
type Bundles = Vec<(LanguageIdentifier, Bundle)>;

/// Language tags in `CATALOGS` order.
pub fn supported() -> Vec<&'static str> {
    CATALOGS.iter().map(|(tag, _)| *tag).collect()
}

/// Right-to-left languages. Their bundles keep Fluent's isolation marks.
fn is_rtl(lang: &LanguageIdentifier) -> bool {
    matches!(lang.language.as_str(), "ar" | "fa" | "he" | "ur")
}

fn build_bundle(tag: &str, source: &str) -> (LanguageIdentifier, Bundle) {
    let lang: LanguageIdentifier = tag
        .parse()
        .unwrap_or_else(|e| panic!("catalog tag {tag:?} is not a language tag: {e}"));
    // A catalog that fails to parse is a bug in a compiled-in file, caught by the
    // `every_catalog_parses` test, so panicking at first use cannot reach a user.
    let resource = FluentResource::try_new(source.to_owned())
        .unwrap_or_else(|(_, errors)| panic!("catalog {tag} has syntax errors: {errors:?}"));
    let mut bundle = FluentBundle::new_concurrent(vec![lang.clone()]);
    // Fluent wraps every argument in U+2068/U+2069 so a name in the opposite
    // direction cannot reorder the sentence around it. That matters in
    // right-to-left languages (an LTR collection name or URL inside Arabic text)
    // but in LTR languages the marks only leak into `title` attributes and JSON,
    // so they are off there.
    bundle.set_use_isolating(is_rtl(&lang));
    bundle
        .add_resource(resource)
        .unwrap_or_else(|errors| panic!("catalog {tag} has duplicate ids: {errors:?}"));
    (lang, bundle)
}

fn build_bundles(catalogs: &[(&str, &str)]) -> Bundles {
    catalogs
        .iter()
        .map(|(tag, source)| build_bundle(tag, source))
        .collect()
}

static BUNDLES: LazyLock<Bundles> = LazyLock::new(|| build_bundles(CATALOGS));

/// Pick the catalog in `bundles` that the requested languages (best first) match.
/// Filtering with likely subtags means `zh-TW` finds a `zh-Hant` catalog and
/// `it-CH` finds `it`.
fn best_match(bundles: &Bundles, requested: &[LanguageIdentifier]) -> Option<usize> {
    let available: Vec<LanguageIdentifier> = bundles.iter().map(|(l, _)| l.clone()).collect();
    let matched = fluent_langneg::negotiate_languages(
        requested,
        &available,
        None,
        fluent_langneg::NegotiationStrategy::Filtering,
    );
    let first = matched.first()?;
    available.iter().position(|l| l == *first)
}

/// Parse an `Accept-Language` value into tags ordered by q-value, best first.
/// `fluent_langneg::accepted_languages::parse` keeps header order and drops the
/// weights, so the ordering is done here. Entries that are not language tags
/// (`*`, garbage) and `q=0` ("not acceptable") are dropped.
fn requested_languages(header: &str) -> Vec<LanguageIdentifier> {
    let mut entries: Vec<(f32, LanguageIdentifier)> = header
        .split(',')
        .filter_map(|part| {
            let mut pieces = part.split(';');
            let lang: LanguageIdentifier = pieces.next()?.trim().parse().ok()?;
            let q = pieces
                .find_map(|p| p.trim().strip_prefix("q="))
                .map_or(1.0, |v| v.trim().parse::<f32>().unwrap_or(0.0));
            (q > 0.0).then_some((q, lang))
        })
        .collect();
    // Stable sort: equal weights keep the browser's own order.
    entries.sort_by(|a, b| b.0.total_cmp(&a.0));
    entries.into_iter().map(|(_, lang)| lang).collect()
}

/// The language `tag` negotiates to among the shipped catalogs (`it-IT` is `it`),
/// for validating `site.language`. The error lists what is available.
pub fn parse_language(tag: &str) -> anyhow::Result<LanguageIdentifier> {
    let unsupported = || {
        anyhow::anyhow!(
            "unsupported language {tag:?}; supported languages: {}",
            supported().join(", ")
        )
    };
    let requested: LanguageIdentifier = tag.trim().parse().map_err(|_| unsupported())?;
    let idx = best_match(&BUNDLES, &[requested]).ok_or_else(unsupported)?;
    Ok(BUNDLES[idx].0.clone())
}

/// A plural-aware argument for a message. `Num` selects plural forms.
#[derive(Clone, Copy, Debug)]
pub enum FluentArg<'a> {
    Str(&'a str),
    Num(i64),
}

/// A chosen interface language: an index into the compiled-in bundles, so it is
/// cheap to clone and to put in request state.
#[derive(Clone)]
pub struct L10n {
    bundles: &'static Bundles,
    idx: usize,
}

// Bundles are not `Debug`, and their contents would be noise anyway.
impl std::fmt::Debug for L10n {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "L10n({})", self.lang())
    }
}

impl Default for L10n {
    /// English.
    fn default() -> Self {
        L10n::in_bundles(&BUNDLES, &REFERENCE.parse().expect("reference tag parses"))
    }
}

/// The language for one request: the first `Accept-Language` entry that has a
/// catalog, else `fallback` (the instance's `site.language`).
pub fn negotiate(accept_language: Option<&str>, fallback: &LanguageIdentifier) -> L10n {
    negotiate_in(&BUNDLES, accept_language, fallback)
}

fn negotiate_in(
    bundles: &'static Bundles,
    accept_language: Option<&str>,
    fallback: &LanguageIdentifier,
) -> L10n {
    let header = accept_language.filter(|h| h.len() <= MAX_ACCEPT_LANGUAGE);
    let idx = header
        .and_then(|h| best_match(bundles, &requested_languages(h)))
        .or_else(|| best_match(bundles, std::slice::from_ref(fallback)));
    match idx {
        Some(idx) => L10n { bundles, idx },
        None => L10n::in_bundles(bundles, &REFERENCE.parse().expect("reference tag parses")),
    }
}

impl L10n {
    /// The bundle for `lang` in `bundles`, else the first one.
    fn in_bundles(bundles: &'static Bundles, lang: &LanguageIdentifier) -> L10n {
        let idx = bundles.iter().position(|(l, _)| l == lang).unwrap_or(0);
        L10n { bundles, idx }
    }

    fn bundle(&self) -> &'static (LanguageIdentifier, Bundle) {
        &self.bundles[self.idx]
    }

    /// The catalog's full tag, for `<html lang>`.
    pub fn lang(&self) -> String {
        self.bundle().0.to_string()
    }

    /// `rtl` or `ltr`, for `<html dir>`.
    pub fn dir(&self) -> &'static str {
        if is_rtl(&self.bundle().0) {
            "rtl"
        } else {
            "ltr"
        }
    }

    /// Whether the chosen language or English has `key`; lets a caller choose
    /// between two phrasings without `tr_args` logging a missing message.
    pub fn has_message(&self, key: &str) -> bool {
        self.bundle().1.has_message(key)
            || self
                .bundles
                .iter()
                .any(|(l, b)| l.language.as_str() == REFERENCE && b.has_message(key))
    }

    pub fn tr(&self, key: &str) -> String {
        self.tr_args(key, &[])
    }

    /// Look `key` up in the chosen language, then English, then give back the key
    /// itself: a page with a missing translation stays usable. The last two steps
    /// log, because they mean a catalog needs fixing.
    pub fn tr_args(&self, key: &str, args: &[(&str, FluentArg)]) -> String {
        let mut fluent_args = FluentArgs::new();
        for (name, value) in args {
            match value {
                FluentArg::Str(s) => fluent_args.set(*name, *s),
                FluentArg::Num(n) => fluent_args.set(*name, *n),
            }
        }
        let chosen = self.bundle();
        if let Some(text) = format(&chosen.1, key, &fluent_args) {
            return text;
        }
        let english = self
            .bundles
            .iter()
            .find(|(l, _)| l.language.as_str() == REFERENCE);
        if let Some((_, bundle)) = english {
            if let Some(text) = format(bundle, key, &fluent_args) {
                tracing::warn!(key, lang = %chosen.0, "message missing from catalog; used English");
                return text;
            }
        }
        tracing::warn!(key, "message missing from every catalog; showing the key");
        key.to_string()
    }
}

fn format(bundle: &Bundle, key: &str, args: &FluentArgs) -> Option<String> {
    let pattern = bundle.get_message(key)?.value()?;
    let mut errors = vec![];
    let text = bundle.format_pattern(pattern, Some(args), &mut errors);
    if !errors.is_empty() {
        tracing::warn!(key, ?errors, "message did not format cleanly");
    }
    Some(text.into_owned())
}

mod format;

#[cfg(test)]
mod tests;
