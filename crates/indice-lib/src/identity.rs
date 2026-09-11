//! Who wrote something: a stable identity, kept distinct from a display name.
//!
//! indice authenticates nobody. A front proxy performs the login (SSO, OIDC,
//! HTTP Basic) and forwards whatever it considers the user's identity in a
//! header — an email under oauth2-proxy, a bare username under Basic auth. That
//! raw string used to be written straight into an annotation as *both* the
//! private edit key and the public display name, which conflated two values
//! with opposite requirements:
//!
//! - the **subject id** is compared, never shown. It must be stable and
//!   normalized, because it is the only thing standing between an author and
//!   someone else's note.
//! - the **display name** is shown, never compared. It must be safe to publish,
//!   because annotations are world-readable and committed to git.
//!
//! [`SubjectId`] is the first of those. It follows the `CollectionId` precedent
//! in [`crate::collections`]: a private field and a validating `parse`, so an
//! un-normalized string cannot become one by accident.
//!
//! # Canonicalize on *comparison*, not only on write
//!
//! [`SubjectId::matches`] canonicalizes the stored key before comparing it. That
//! one decision is what lets this ship without a migration: a note written
//! before normalization carries `"alice@x.edu"`, which canonicalizes to exactly
//! what the live identity canonicalizes to, so its author keeps their note. The
//! same holds for the `"local"` key every loopback instance wrote. Rewriting the
//! stored files is therefore cosmetic, never load-bearing for correctness.

use serde::{Deserialize, Serialize};

/// Prefix for an identity that isn't an email — a bare username, or the local
/// operator. A URN keeps the value an IRI, which is what the W3C Web Annotation
/// model wants in `creator.id`.
const USER_URN: &str = "urn:indice:user:";

/// RFC 5321's maximum email length. A header value is attacker-influenced even
/// behind a trusted proxy, and this string ends up in committed JSONL.
const MAX_LEN: usize = 320;

/// A normalized, stable identity for someone who writes to this archive.
///
/// Always an IRI:
/// - `alice@x.edu` → `mailto:alice@x.edu`
/// - `alice` → `urn:indice:user:alice`
/// - an IRI already → left alone
///
/// Construct one only through [`SubjectId::parse`] or [`SubjectId::local`];
/// there is deliberately no `From<&str>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubjectId(String);

impl SubjectId {
    /// Canonicalize whatever the proxy forwarded.
    ///
    /// `None` for an identity we refuse to record: empty, absurdly long, or
    /// containing control characters (a newline would smuggle a second record
    /// into a log line or a JSONL file).
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw.len() > MAX_LEN || raw.chars().any(char::is_control) {
            return None;
        }
        // Already an IRI. `mailto:` and our own URN are case-folded so they
        // canonicalize the same as their bare forms; any other scheme is left
        // exactly as given, since lowercasing a URL path can change what it
        // points at.
        for scheme in ["mailto:", USER_URN] {
            if let Some(rest) = raw.strip_prefix(scheme) {
                return Some(SubjectId(format!("{scheme}{}", rest.to_ascii_lowercase())));
            }
        }
        if raw.contains("://") {
            return Some(SubjectId(raw.to_string()));
        }
        // A bare identity. Emails and usernames are case-insensitive in every
        // IdP we sit behind, so `Alice@x.edu` and `alice@x.edu` are one person.
        let lower = raw.to_ascii_lowercase();
        Some(SubjectId(match lower.contains('@') {
            true => format!("mailto:{lower}"),
            false => format!("{USER_URN}{lower}"),
        }))
    }

    /// The single trusted operator of a loopback `--manage` instance, which has
    /// no authentication and therefore no distinct identities.
    pub fn local() -> Self {
        SubjectId(format!("{USER_URN}local"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this identity wrote the record carrying `stored` as its author
    /// key. The stored value is canonicalized first, so keys written before
    /// normalization (`alice@x.edu`, `Alice@X.edu`, `local`) still match.
    pub fn matches(&self, stored: Option<&str>) -> bool {
        stored
            .and_then(SubjectId::parse)
            .is_some_and(|s| s == *self)
    }

    /// A name safe to publish: the email local part, or the bare username —
    /// never the domain, and never a mailto-able address.
    ///
    /// Used verbatim, never title-cased: turning `ed.summers` into "Ed Summers"
    /// would be inventing the spelling of someone's name. A real display name
    /// comes from the person choosing one.
    pub fn display_name(&self) -> &str {
        let bare = self
            .0
            .strip_prefix("mailto:")
            .or_else(|| self.0.strip_prefix(USER_URN))
            .unwrap_or(&self.0);
        match bare.split_once('@') {
            Some((local, _)) if !local.is_empty() => local,
            _ => bare,
        }
    }
}

impl std::fmt::Display for SubjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_emails_and_usernames() {
        assert_eq!(
            SubjectId::parse("Alice@X.edu").unwrap().as_str(),
            "mailto:alice@x.edu"
        );
        assert_eq!(
            SubjectId::parse("  alice  ").unwrap().as_str(),
            "urn:indice:user:alice"
        );
        assert_eq!(SubjectId::parse("local").unwrap(), SubjectId::local());
    }

    #[test]
    fn canonicalization_is_idempotent() {
        // Required, or re-canonicalizing a stored key would drift away from the
        // live identity and silently orphan notes.
        for raw in ["Alice@X.edu", "alice", "local", "https://x.edu/u/1"] {
            let once = SubjectId::parse(raw).unwrap();
            let twice = SubjectId::parse(once.as_str()).unwrap();
            assert_eq!(once, twice, "{raw} should be stable under re-parsing");
        }
    }

    #[test]
    fn rejects_identities_we_refuse_to_record() {
        assert_eq!(SubjectId::parse(""), None);
        assert_eq!(SubjectId::parse("   "), None);
        // A newline would smuggle a second line into the JSONL store or a log.
        assert_eq!(SubjectId::parse("alice\nroot"), None);
        assert_eq!(SubjectId::parse("a\tb"), None);
        assert_eq!(SubjectId::parse(&"a".repeat(MAX_LEN + 1)), None);
    }

    #[test]
    fn leaves_a_foreign_iri_alone() {
        // Lowercasing a URL path could change what it identifies.
        let iri = "https://orcid.org/0000-0002-ABCD";
        assert_eq!(SubjectId::parse(iri).unwrap().as_str(), iri);
    }

    #[test]
    fn matches_legacy_author_keys_without_a_migration() {
        // This is the property that makes the change safe on existing data.
        let alice = SubjectId::parse("alice@x.edu").unwrap();
        assert!(alice.matches(Some("alice@x.edu")), "pre-normalization key");
        assert!(alice.matches(Some("Alice@X.edu")), "differing case");
        assert!(alice.matches(Some("mailto:alice@x.edu")), "canonical key");
        assert!(!alice.matches(Some("bob@x.edu")), "someone else's note");
        assert!(!alice.matches(None), "an unattributed note is nobody's");

        // Every note a loopback instance ever wrote used the key "local".
        assert!(SubjectId::local().matches(Some("local")));
    }

    #[test]
    fn display_name_never_carries_a_domain() {
        assert_eq!(
            SubjectId::parse("alice@x.edu").unwrap().display_name(),
            "alice"
        );
        assert_eq!(
            SubjectId::parse("ed.summers").unwrap().display_name(),
            "ed.summers"
        );
        assert_eq!(SubjectId::local().display_name(), "local");
    }
}
