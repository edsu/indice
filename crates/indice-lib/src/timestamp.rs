//! Timestamps as indice writes them: RFC 3339 in UTC, to the second, as in
//! `2026-11-18T16:45:15Z`. Manifests, notes, the event log and the index lock
//! all keep this form, and the tests that read them back expect it.

use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

/// The current time, as indice writes it.
pub(crate) fn now() -> String {
    format(OffsetDateTime::now_utc())
}

/// `at` as indice writes a time: in UTC, with any fraction of a second
/// dropped.
pub(crate) fn format(at: OffsetDateTime) -> String {
    let at = at.to_offset(UtcOffset::UTC);
    // Formatting fails only for a year outside 0 to 9999, which no clock,
    // WARC timestamp or WACZ `mtime` indice reads gives.
    at.replace_nanosecond(0)
        .unwrap_or(at)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// An RFC 3339 time, with any offset, or `None` if `text` isn't one.
pub(crate) fn parse(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_is_written_in_utc_to_the_second() {
        let at = parse("2026-11-18T11:45:15.678-05:00").unwrap();
        assert_eq!(format(at), "2026-11-18T16:45:15Z");
        assert_eq!(
            format(parse("2026-11-18T16:45:15Z").unwrap()),
            "2026-11-18T16:45:15Z"
        );
    }

    #[test]
    fn now_has_the_written_form() {
        let now = now();
        assert_eq!(now.len(), "2026-11-18T16:45:15Z".len(), "{now}");
        assert!(now.ends_with('Z'), "{now}");
        assert_eq!(parse(&now).map(format).as_deref(), Some(now.as_str()));
    }

    #[test]
    fn what_is_not_rfc_3339_does_not_parse() {
        assert_eq!(parse("2026-11-18 16:45:15"), None);
        assert_eq!(parse("not a time"), None);
    }
}
