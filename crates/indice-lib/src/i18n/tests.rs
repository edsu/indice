use super::*;
use std::collections::BTreeSet;

fn en() -> LanguageIdentifier {
    "en".parse().unwrap()
}

fn it() -> LanguageIdentifier {
    "it".parse().unwrap()
}

/// Message and term ids of a catalog, read from the source text: an id starts a
/// line, optionally with `-` for a term, and runs to the `=`.
fn ids(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter(|l| l.starts_with(|c: char| c.is_ascii_alphabetic() || c == '-'))
        .filter_map(|l| l.split_once('=').map(|(id, _)| id.trim().to_string()))
        .collect()
}

/// A bundle list for tests that need catalogs indice does not ship. Leaked so it
/// is `'static` like the real one; a test process is short-lived.
fn test_bundles(catalogs: &[(&str, &str)]) -> &'static Bundles {
    Box::leak(Box::new(build_bundles(catalogs)))
}

#[test]
fn every_catalog_parses() {
    for (tag, source) in CATALOGS {
        assert!(
            FluentResource::try_new(source.to_string()).is_ok(),
            "{tag}.ftl does not parse"
        );
        tag.parse::<LanguageIdentifier>().unwrap();
    }
}

#[test]
fn catalogs_have_the_same_messages() {
    let reference = ids(CATALOGS[0].1);
    assert!(!reference.is_empty());
    for (tag, source) in &CATALOGS[1..] {
        let have = ids(source);
        let missing: Vec<_> = reference.difference(&have).collect();
        let extra: Vec<_> = have.difference(&reference).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{tag}.ftl differs from en.ftl: missing {missing:?}, extra {extra:?}"
        );
    }
}

#[test]
fn negotiate_prefers_the_browser() {
    assert_eq!(
        negotiate(Some("it-IT,it;q=0.9,en;q=0.8"), &en()).lang(),
        "it"
    );
}

#[test]
fn negotiate_falls_back_to_the_instance() {
    assert_eq!(negotiate(Some("ja"), &it()).lang(), "it");
    assert_eq!(negotiate(None, &it()).lang(), "it");
}

#[test]
fn negotiate_honours_q_values() {
    assert_eq!(negotiate(Some("en;q=0.5, it;q=0.9"), &en()).lang(), "it");
    assert_eq!(negotiate(Some("it;q=0, en"), &it()).lang(), "en");
}

#[test]
fn negotiate_survives_junk() {
    let long = "it,".repeat(2000);
    for h in ["*", "IT-ch", ";;;,q=", "\u{fffd}\u{0}", &long] {
        let _ = negotiate(Some(h), &en());
    }
    assert_eq!(negotiate(Some("IT-ch"), &en()).lang(), "it");
    // Over 1 KiB is ignored outright, so the instance language wins.
    assert_eq!(negotiate(Some(&long), &en()).lang(), "en");
}

#[test]
fn parse_language_rejects_unknown() {
    let e = parse_language("xx").unwrap_err().to_string();
    assert!(e.contains("en") && e.contains("it"));
    assert!(parse_language("not a tag!").is_err());
    assert_eq!(parse_language("IT-it").unwrap().language.as_str(), "it");
}

#[test]
fn missing_italian_key_falls_back_to_english() {
    let bundles = test_bundles(&[
        ("en", "test-only-en = English text\nboth = en both"),
        ("it", "both = it both"),
    ]);
    let l = negotiate_in(bundles, Some("it"), &en());
    assert_eq!(l.lang(), "it");
    assert_eq!(l.tr("both"), "it both");
    assert_eq!(l.tr("test-only-en"), "English text");
}

#[test]
fn script_subtags_survive() {
    let bundles = test_bundles(&[("en", ""), ("zh-Hans", ""), ("zh-Hant", "")]);
    assert_eq!(
        negotiate_in(bundles, Some("zh-TW"), &en()).lang(),
        "zh-Hant"
    );
    assert_eq!(
        negotiate_in(bundles, Some("zh-CN"), &en()).lang(),
        "zh-Hans"
    );
}

#[test]
fn isolation_follows_direction() {
    let msg = "greet = Hi { $name }";
    let bundles = test_bundles(&[("en", msg), ("it", msg), ("ar", msg)]);
    let args = [("name", FluentArg::Str("Bob"))];
    let ar = negotiate_in(bundles, Some("ar"), &en());
    assert_eq!(ar.dir(), "rtl");
    assert!(ar.tr_args("greet", &args).contains('\u{2068}'));
    let it = negotiate_in(bundles, Some("it"), &en());
    assert_eq!(it.dir(), "ltr");
    assert_eq!(it.tr_args("greet", &args), "Hi Bob");
}

#[test]
fn plurals_select_on_numbers() {
    let msg = "n = { $count ->\n    [one] { $count } crawl\n   *[other] { $count } crawls\n}";
    let bundles = test_bundles(&[("en", msg)]);
    let l = negotiate_in(bundles, None, &en());
    let n = |c| l.tr_args("n", &[("count", FluentArg::Num(c))]);
    assert_eq!(n(0), "0 crawls");
    assert_eq!(n(1), "1 crawl");
}

#[test]
fn unknown_key_returns_the_key() {
    assert_eq!(L10n::default().tr("no-such-key"), "no-such-key");
}

#[test]
fn default_is_english() {
    assert_eq!(L10n::default().lang(), "en");
}
