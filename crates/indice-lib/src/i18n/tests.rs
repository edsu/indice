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

fn italian() -> L10n {
    negotiate(Some("it"), &en())
}

#[test]
fn long_date_in_italian() {
    assert_eq!(
        italian().long_date("20250107").as_deref(),
        Some("7 gennaio 2025")
    );
    assert_eq!(
        L10n::default().long_date("20250107").as_deref(),
        Some("7 January 2025")
    );
    assert_eq!(
        L10n::default().long_date("2021-05-25T00:00:00Z").as_deref(),
        Some("25 May 2021")
    );
    assert_eq!(L10n::default().long_date("2025"), None);
    assert_eq!(L10n::default().long_date("20251301"), None);
    assert_eq!(L10n::default().long_date("20250132"), None);
}

#[test]
fn month_year_in_italian() {
    assert_eq!(italian().month_year(5, 2021), "maggio 2021");
    assert_eq!(L10n::default().month_year(5, 2021), "May 2021");
}

#[test]
fn number_grouping() {
    assert_eq!(L10n::default().number(23_800), "23,800");
    assert_eq!(italian().number(23_800), "23.800");
    assert_eq!(L10n::default().number(999), "999");
    assert_eq!(italian().number(1_234_567), "1.234.567");
    assert_eq!(italian().number(0), "0");
}

#[test]
fn size_in_italian() {
    assert_eq!(italian().size(2_621_440), "2,5 MB");
    assert_eq!(L10n::default().size(2_621_440), "2.5 MB");
    assert_eq!(L10n::default().size(512), "512 B");
    assert_eq!(italian().size(1024), "1,0 KB");
}

/// A language indice does not ship, with all six of Arabic's plural categories.
const ARABIC_COUNT: &str = "
js-n-count = { $count ->
    [zero] zero { $shown }
    [one] one { $shown }
    [two] two { $shown }
    [few] few { $shown }
    [many] many { $shown }
   *[other] other { $shown }
}
js-plain = plain
";

fn forms_of(json: &str, key: &str) -> BTreeSet<String> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    v[key].as_object().unwrap().keys().cloned().collect()
}

#[test]
fn plural_strings_carry_every_category() {
    // Italian has `one` and `other`, no more.
    let it = crate::views::strings_json(
        &L10n::in_bundles(&BUNDLES, &it()),
        &["js-added-crawls-count"],
    );
    assert_eq!(
        forms_of(&it, "js-added-crawls-count"),
        BTreeSet::from(["one".to_string(), "other".to_string()])
    );
    let v: serde_json::Value = serde_json::from_str(&it).unwrap();
    // The number stays a placeholder for the page's script to fill in.
    assert!(v["js-added-crawls-count"]["other"]
        .as_str()
        .unwrap()
        .contains("{count}"));

    // Arabic has six.
    let ar = test_bundles(&[("ar", ARABIC_COUNT)]);
    let l10n = negotiate_in(ar, Some("ar"), &"ar".parse().unwrap());
    let json = crate::views::strings_json(&l10n, &["js-n-count", "js-plain"]);
    let expected: BTreeSet<String> = ["zero", "one", "two", "few", "many", "other"]
        .map(String::from)
        .into();
    assert_eq!(forms_of(&json, "js-n-count"), expected);
    // A string without a count stays a plain string.
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["js-plain"], "plain");
    // Each form was picked by its own category.
    assert!(v["js-n-count"]["few"].as_str().unwrap().starts_with("few"));
    assert!(v["js-n-count"]["many"]
        .as_str()
        .unwrap()
        .starts_with("many"));
}

#[test]
fn script_strings_escape_lt() {
    let b = test_bundles(&[("en", "x = </script><b>\n")]);
    let l10n = negotiate_in(b, Some("en"), &en());
    let json = crate::views::strings_json(&l10n, &["x"]);
    assert!(!json.contains('<'), "{json}");
    assert!(json.contains("\\u003c/script>"), "{json}");
    let html = crate::views::script_strings(&l10n, &["x"]).into_string();
    assert!(
        html.starts_with(r#"<script id="i18n" type="application/json">"#),
        "{html}"
    );
    assert_eq!(html.matches("</script>").count(), 1, "{html}");
}

#[test]
fn js_strings_use_known_arguments() {
    use crate::views::{strings_json, DESK_STRINGS, EDITOR_STRINGS, VIEWER_STRINGS};
    let keys: Vec<&str> = DESK_STRINGS
        .iter()
        .chain(EDITOR_STRINGS)
        .chain(VIEWER_STRINGS)
        .copied()
        .collect();
    // Every script message (`js-` for the workroom, `viewer-` and `anno-` for the
    // viewer) is sent to some page.
    let for_scripts = |id: &&String| {
        ["js-", "viewer-", "anno-"]
            .iter()
            .any(|p| id.starts_with(p))
    };
    for id in ids(CATALOGS[0].1).iter().filter(for_scripts) {
        assert!(keys.contains(&id.as_str()), "{id} is in no page's list");
    }
    for (tag, _) in CATALOGS {
        let l10n = L10n::in_bundles(&BUNDLES, &tag.parse().unwrap());
        let json = strings_json(&l10n, &keys);
        // An argument the page does not pass shows up as `{$name}`, a missing
        // message as its own key.
        assert!(!json.contains("{$"), "{tag}: {json}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        for key in &keys {
            assert_ne!(v[key], *key, "{tag}: {key} is missing");
        }
    }
}

/// The interface's prose uses colons, semicolons and parentheses, not em dashes:
/// the house style for everything indice says, in every language.
#[test]
fn catalogs_have_no_em_dashes() {
    for (tag, source) in CATALOGS {
        let lines: Vec<_> = source
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains('\u{2014}'))
            .map(|(n, l)| format!("{}: {l}", n + 1))
            .collect();
        assert!(
            lines.is_empty(),
            "{tag}.ftl has em dashes:\n{}",
            lines.join("\n")
        );
    }
}
