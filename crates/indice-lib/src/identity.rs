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

/// Reduce an identity-ish string to something safe to publish: an email's local
/// part, or the value unchanged if there's no domain in it.
///
/// This is the sanitizer every *public* surface goes through, and it runs on
/// read rather than on write so it covers data already on disk — annotations
/// written before identities were separated, and `author` values already baked
/// into the search index, which is only rebuilt by an explicit `reindex`.
///
/// Used verbatim, never title-cased: turning `ed.summers` into "Ed Summers"
/// would be inventing the spelling of someone's name.
pub fn public_display_name(raw: &str) -> &str {
    match raw.split_once('@') {
        // A leading `@` would otherwise yield an empty label.
        Some((local, _)) if !local.is_empty() => local,
        _ => raw,
    }
}

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
    pub fn display_name(&self) -> &str {
        let bare = self
            .0
            .strip_prefix("mailto:")
            .or_else(|| self.0.strip_prefix(USER_URN))
            .unwrap_or(&self.0);
        public_display_name(bare)
    }

    /// Parse an identity forwarded by a *proxy*, which must never be able to
    /// name the loopback operator.
    ///
    /// [`SubjectId::local`] is the author key every loopback `--manage` session
    /// writes, and [`SubjectId::matches`] deliberately treats the bare string
    /// `"local"` as that same identity so those notes stay editable. The flip
    /// side is that a remote user whose proxy identity happened to be `local`
    /// would inherit every note the machine's operator ever wrote. Rare, but
    /// free to close: reject it, rather than silently conflating two people.
    pub fn parse_remote(raw: &str) -> Option<Self> {
        SubjectId::parse(raw).filter(|s| *s != SubjectId::local())
    }
}

impl std::fmt::Display for SubjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What someone is allowed to do.
///
/// Deliberately **ordered** — `derive(Ord)` on a fieldless enum orders by
/// declaration order, so the whole permission check is `role >= Role::Curator`.
/// Each role contains the one below it, which is what makes the model
/// explainable in a sentence: *curators add, admins remove.*
///
/// There is no annotate-only tier yet. If a deployment ever wants one (invite
/// researchers to annotate without letting them accession), it slots in between
/// `Reader` and `Curator` as one variant plus a roster value — the ordering
/// keeps every existing comparison correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Anonymous, or authenticated but on nobody's roster: public read only.
    Reader,
    /// Accession and describe: create collections, add and upload crawls, edit
    /// finding aids, run imports, annotate. May edit and delete their own notes.
    Curator,
    /// Everything a curator may do, plus the irreversible and shared acts:
    /// delete a crawl, delete a collection, and moderate anyone's notes.
    Admin,
}

impl Role {
    /// May accession and describe.
    pub fn can_curate(self) -> bool {
        self >= Role::Curator
    }
    /// May write annotations.
    pub fn can_annotate(self) -> bool {
        self >= Role::Curator
    }
    /// May deaccession, and moderate other people's notes.
    pub fn can_administer(self) -> bool {
        self >= Role::Admin
    }
}

/// Who is making a request, and what they may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    id: SubjectId,
    display_name: String,
    role: Role,
    /// Whether this principal may act on records *other people* authored.
    ///
    /// Separate from `role` rather than derived from it, because the two come
    /// apart in the default deployment: with no `users.yaml` everyone
    /// authenticated is an `Admin`, and deriving moderation from that would
    /// silently let any signed-in user delete anyone's notes — which was
    /// strictly author-only before roles existed. Moderating someone else's
    /// work is a real decision, so it is something an operator opts into by
    /// naming admins in a roster.
    can_moderate: bool,
}

impl Principal {
    pub fn new(
        id: SubjectId,
        display_name: impl Into<String>,
        role: Role,
        can_moderate: bool,
    ) -> Self {
        let display_name = display_name.into();
        let display_name = match display_name.trim().is_empty() {
            true => id.display_name().to_string(),
            false => display_name,
        };
        Principal {
            id,
            display_name,
            role,
            can_moderate,
        }
    }

    /// The single trusted operator of a loopback `--manage` instance. Always an
    /// admin: there is no authentication to filter, and the startup guard
    /// already refuses to run local mode anywhere but loopback. (Moderation is
    /// moot there — every note carries the same author key, so `owns` already
    /// covers all of them.)
    pub fn local_operator() -> Self {
        let id = SubjectId::local();
        Principal::new(id, "", Role::Admin, true)
    }

    pub fn id(&self) -> &SubjectId {
        &self.id
    }
    /// Safe to publish — derived from the identity, never the raw login value.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
    pub fn role(&self) -> Role {
        self.role
    }
    /// Whether this principal wrote the record carrying `stored` as its author
    /// key (canonicalizing `stored` first, so legacy keys still match).
    pub fn owns(&self, stored: Option<&str>) -> bool {
        self.id.matches(stored)
    }
    /// Whether this principal may moderate records other people authored.
    pub fn can_moderate(&self) -> bool {
        self.can_moderate
    }

    /// Whether this principal may deaccession a crawl accessioned by
    /// `added_by`: an admin may remove any, a curator only their own.
    ///
    /// This completes the model's sentence — *curators add and can undo their
    /// own additions; only admins remove a collection.* Ownership governs
    /// crawls because they are leaves; a collection is a shared container, so
    /// deleting one stays an admin act however it was created.
    ///
    /// Note this uses the **role**, not `can_moderate`. The two answer
    /// different questions: `can_moderate` is "may act on what someone else
    /// *authored*", which nobody could before roles existed and so is opt-in;
    /// deaccession is an operational act on the archive, which any
    /// authenticated user could, so gating it on the role keeps the no-roster
    /// default behaving as it always has. An unattributed crawl (indexed from
    /// the CLI, or before custody was recorded) is nobody's, so only an admin
    /// may remove it.
    pub fn may_delete_crawl(&self, added_by: Option<&str>) -> bool {
        self.role.can_administer() || self.owns(added_by)
    }

    /// Whether this principal may edit the record authored under `stored`:
    /// their own, or anyone's if they can moderate.
    pub fn may_edit(&self, stored: Option<&str>) -> bool {
        self.owns(stored) || self.can_moderate
    }
}

/// The file naming who may do what, relative to a indice home.
pub const USERS_FILE: &str = "users.yaml";

/// One person in the roster.
///
/// `deny_unknown_fields` because this is a permissions file: a typo'd `Role:`
/// or `rol:` would otherwise be ignored and silently fall back to the
/// `curator` default, granting more than the operator wrote down.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserEntry {
    /// Their identity as the proxy forwards it (`alice@x.edu`) or canonically
    /// (`mailto:alice@x.edu`) — both are accepted and normalized on load.
    pub id: String,
    /// The name to show publicly. Omit to derive one from the id.
    #[serde(default)]
    pub name: Option<String>,
    /// Defaults to `curator`: someone written down here is presumed to be
    /// staff, and `reader` is what you get by *not* being listed.
    #[serde(default = "default_role")]
    pub role: Role,
    /// Prior identities — a previous email, or a username from an IdP you've
    /// since migrated off. Lets someone keep the notes they wrote under an old
    /// address instead of being permanently locked out of them.
    #[serde(default)]
    pub aliases: Vec<String>,
}

fn default_role() -> Role {
    Role::Curator
}

/// The roster read from `<home>/users.yaml`.
///
/// indice still authenticates nobody — the proxy owns login, this file owns
/// *privilege*. Keeping it a small hand-editable file rather than a user table
/// matches the rest of the home directory (finding aids, notes): committable,
/// diffable, and legible without indice running.
#[derive(Debug, Clone, Default)]
pub struct Users {
    /// `None` when there is no `users.yaml` at all, which is the common case
    /// and must behave exactly as indice did before roles existed: everyone the
    /// proxy authenticates is an admin. An empty-but-present file is *not* the
    /// same thing — that's an operator saying "nobody", and is honored.
    roster: Option<Vec<ResolvedEntry>>,
}

#[derive(Debug, Clone)]
struct ResolvedEntry {
    id: SubjectId,
    name: Option<String>,
    role: Role,
    aliases: Vec<SubjectId>,
}

/// The on-disk shape.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UsersFile {
    users: Vec<UserEntry>,
}

impl Users {
    pub fn path(home: &std::path::Path) -> std::path::PathBuf {
        home.join(USERS_FILE)
    }

    /// Load `<home>/users.yaml`, or the permissive default if absent. Errors
    /// only on a present-but-malformed file — silently ignoring a typo in a
    /// permissions file is how someone ends up with more access than intended.
    pub fn load(home: &std::path::Path) -> anyhow::Result<Users> {
        use anyhow::Context;
        let path = Self::path(home);
        if !path.exists() {
            return Ok(Users { roster: None });
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed: UsersFile = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        let mut roster = Vec::new();
        for entry in parsed.users {
            let id = SubjectId::parse(&entry.id)
                .with_context(|| format!("{}: unusable user id {:?}", path.display(), entry.id))?;
            roster.push(ResolvedEntry {
                id,
                name: entry.name.filter(|n| !n.trim().is_empty()),
                role: entry.role,
                // An unparseable alias is an error, not something to drop:
                // silently discarding one would quietly lose that person
                // access to every note they wrote under their old address.
                aliases: entry
                    .aliases
                    .iter()
                    .map(|a| {
                        SubjectId::parse(a)
                            .with_context(|| format!("{}: unusable alias {a:?}", path.display()))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?,
            });
        }
        Ok(Users {
            roster: Some(roster),
        })
    }

    /// Whether a roster is configured at all (vs. the permissive default).
    pub fn is_configured(&self) -> bool {
        self.roster.is_some()
    }

    /// A one-line summary for the startup log, so an operator can see which
    /// regime they're in without guessing.
    pub fn summary(&self) -> String {
        match &self.roster {
            None => "no users.yaml: every authenticated user is an admin".to_string(),
            Some(r) => {
                let admins = r.iter().filter(|e| e.role == Role::Admin).count();
                let curators = r.iter().filter(|e| e.role == Role::Curator).count();
                format!(
                    "users.yaml: {admins} admin(s), {curators} curator(s); anyone else is a reader"
                )
            }
        }
    }

    /// Resolve an authenticated identity to a principal.
    ///
    /// With no roster every authenticated user is an admin — byte-identical to
    /// indice's behavior before roles, so upgrading changes nothing. With a
    /// roster, an identity that isn't on it is a `Reader`: authenticated, but
    /// with no more power than an anonymous visitor.
    pub fn resolve(&self, id: SubjectId) -> Principal {
        let Some(roster) = &self.roster else {
            // Admin, as before roles — but NOT a moderator. Deriving moderation
            // from the role here would quietly grant every signed-in user power
            // over other people's notes, which nobody had before this existed.
            return Principal::new(id, "", Role::Admin, false);
        };
        // Someone's own entry always beats another entry's stale alias;
        // otherwise a leftover alias would silently hand them that person's
        // role *and* subject id.
        let found = roster
            .iter()
            .find(|e| e.id == id)
            .or_else(|| roster.iter().find(|e| e.aliases.contains(&id)));
        match found {
            // Matched via an alias: adopt the entry's *canonical* id, so a note
            // written under an old address is still recognized as theirs.
            Some(e) => Principal::new(
                e.id.clone(),
                e.name.clone().unwrap_or_default(),
                e.role,
                e.role.can_administer(),
            ),
            None => Principal::new(id, "", Role::Reader, false),
        }
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

    /// Write a `users.yaml` into a temp home and load it.
    fn roster(yaml: &str) -> (tempfile::TempDir, Users) {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(Users::path(tmp.path()), yaml).unwrap();
        let users = Users::load(tmp.path()).unwrap();
        (tmp, users)
    }

    #[test]
    fn moderation_requires_an_explicit_roster() {
        // With no users.yaml everyone is an Admin, so deriving moderation from
        // the role would let any signed-in user delete anyone's notes — power
        // nobody had before roles existed.
        let tmp = tempfile::TempDir::new().unwrap();
        let default = Users::load(tmp.path())
            .unwrap()
            .resolve(SubjectId::parse("anyone@x.edu").unwrap());
        assert_eq!(default.role(), Role::Admin);
        assert!(!default.can_moderate(), "not without a roster");
        assert!(!default.may_edit(Some("someone-else@x.edu")));
        assert!(default.may_edit(Some("anyone@x.edu")), "still their own");

        // Naming an admin in a roster IS the opt-in.
        let (_t, users) = roster("users:\n  - id: boss@x.edu\n    role: admin\n");
        let boss = users.resolve(SubjectId::parse("boss@x.edu").unwrap());
        assert!(boss.can_moderate() && boss.may_edit(Some("someone-else@x.edu")));
        // ...but a curator on that same roster still only edits their own.
        let (_t2, users2) = roster("users:\n  - id: c@x.edu\n    role: curator\n");
        let c = users2.resolve(SubjectId::parse("c@x.edu").unwrap());
        assert!(!c.can_moderate());
    }

    #[test]
    fn an_own_entry_beats_another_entrys_stale_alias() {
        // A leftover alias must not hand someone else's role and subject id to
        // the person who actually owns that address.
        let (_t, users) = roster(
            "users:\n  \
             - id: boss@x.edu\n    role: admin\n    aliases: [alice@x.edu]\n  \
             - id: alice@x.edu\n    role: curator\n",
        );
        let alice = users.resolve(SubjectId::parse("alice@x.edu").unwrap());
        assert_eq!(alice.role(), Role::Curator, "her own entry wins");
        assert_eq!(alice.id().as_str(), "mailto:alice@x.edu");
        assert!(!alice.can_moderate());
    }

    #[test]
    fn a_typo_in_a_permissions_file_is_refused() {
        // An ignored `Role:` would silently fall back to the curator default,
        // granting more than the operator wrote down.
        let tmp = tempfile::TempDir::new().unwrap();
        for bad in [
            "users:\n  - id: a@x.edu\n    Role: reader\n",
            "users:\n  - id: a@x.edu\n    rol: reader\n",
            "user:\n  - id: a@x.edu\n",
            "users:\n  - id: a@x.edu\n    aliases: [\"\"]\n",
        ] {
            std::fs::write(Users::path(tmp.path()), bad).unwrap();
            assert!(Users::load(tmp.path()).is_err(), "should refuse: {bad:?}");
        }
    }

    #[test]
    fn no_users_file_preserves_the_pre_roles_behavior() {
        // The upgrade path: an existing deployment must not change at all.
        let tmp = tempfile::TempDir::new().unwrap();
        let users = Users::load(tmp.path()).unwrap();
        assert!(!users.is_configured());
        let p = users.resolve(SubjectId::parse("anyone@x.edu").unwrap());
        assert_eq!(p.role(), Role::Admin, "everyone authenticated is an admin");
    }

    #[test]
    fn an_empty_roster_is_not_the_same_as_no_roster() {
        // "users: []" is an operator saying nobody, and must be honored --
        // collapsing it into the permissive default would be a privilege bug.
        let (_t, users) = roster("users: []\n");
        assert!(users.is_configured());
        assert_eq!(
            users
                .resolve(SubjectId::parse("anyone@x.edu").unwrap())
                .role(),
            Role::Reader
        );
    }

    #[test]
    fn roster_assigns_roles_and_leaves_strangers_as_readers() {
        let (_t, users) = roster(
            "users:\n  \
             - id: boss@x.edu\n    role: admin\n    name: The Boss\n  \
             - id: Alice@X.edu\n",
        );
        let boss = users.resolve(SubjectId::parse("boss@x.edu").unwrap());
        assert_eq!(boss.role(), Role::Admin);
        assert_eq!(boss.display_name(), "The Boss");
        // role defaults to curator; the id matches case-insensitively.
        let alice = users.resolve(SubjectId::parse("alice@x.edu").unwrap());
        assert_eq!(alice.role(), Role::Curator);
        assert_eq!(alice.display_name(), "alice", "derived, never the address");
        // Authenticated but unlisted: no more power than an anonymous visitor.
        let stranger = users.resolve(SubjectId::parse("eve@x.edu").unwrap());
        assert_eq!(stranger.role(), Role::Reader);
    }

    #[test]
    fn an_alias_keeps_someone_their_old_notes() {
        let (_t, users) = roster("users:\n  - id: alice@new.edu\n    aliases: [alice@old.edu]\n");
        let p = users.resolve(SubjectId::parse("alice@old.edu").unwrap());
        assert_eq!(p.role(), Role::Curator, "recognized via the alias");
        // Adopting the canonical id is the point: notes written under either
        // address resolve to one person.
        assert_eq!(p.id().as_str(), "mailto:alice@new.edu");
        assert!(p.owns(Some("alice@new.edu")));
    }

    #[test]
    fn a_malformed_roster_is_an_error_not_a_shrug() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(Users::path(tmp.path()), "users:\n  - id: \"\"\n").unwrap();
        assert!(
            Users::load(tmp.path()).is_err(),
            "silently ignoring a typo in a permissions file grants access nobody intended"
        );
    }

    #[test]
    fn roles_nest_so_one_comparison_is_the_whole_check() {
        assert!(Role::Admin > Role::Curator && Role::Curator > Role::Reader);
        assert!(Role::Admin.can_curate() && Role::Admin.can_administer());
        assert!(Role::Curator.can_curate() && Role::Curator.can_annotate());
        assert!(
            !Role::Curator.can_administer(),
            "curators do not deaccession"
        );
        assert!(!Role::Reader.can_curate() && !Role::Reader.can_annotate());
    }

    #[test]
    fn a_principal_falls_back_to_a_derived_display_name() {
        let p = Principal::new(
            SubjectId::parse("alice@x.edu").unwrap(),
            "",
            Role::Curator,
            false,
        );
        assert_eq!(p.display_name(), "alice", "never the raw login value");
        let named = Principal::new(
            SubjectId::parse("alice@x.edu").unwrap(),
            "Alice Ramírez",
            Role::Curator,
            false,
        );
        assert_eq!(named.display_name(), "Alice Ramírez", "a chosen name wins");
    }

    #[test]
    fn a_curator_deaccessions_only_their_own_crawls() {
        let alice = Principal::new(
            SubjectId::parse("alice@x.edu").unwrap(),
            "",
            Role::Curator,
            false,
        );
        let boss = Principal::new(
            SubjectId::parse("boss@x.edu").unwrap(),
            "",
            Role::Admin,
            true,
        );
        assert!(alice.may_delete_crawl(Some("alice@x.edu")), "her own");
        assert!(!alice.may_delete_crawl(Some("bob@x.edu")), "not a peer's");
        // Unattributed (CLI-indexed, or from before custody existed) is
        // nobody's, so a curator can't claim it.
        assert!(!alice.may_delete_crawl(None));
        assert!(boss.may_delete_crawl(None), "an admin still can");
        assert!(boss.may_delete_crawl(Some("bob@x.edu")));

        // Upgrade safety: with no roster everyone is an Admin, so deaccession
        // keeps working exactly as it did before roles. This deliberately uses
        // the ROLE and not can_moderate — see may_delete_crawl.
        let tmp = tempfile::TempDir::new().unwrap();
        let default = Users::load(tmp.path())
            .unwrap()
            .resolve(SubjectId::parse("anyone@x.edu").unwrap());
        assert!(!default.can_moderate(), "not over other people's notes");
        assert!(
            default.may_delete_crawl(None),
            "but deaccession is unchanged"
        );
    }

    #[test]
    fn only_an_admin_may_edit_someone_elses_record() {
        let alice = Principal::new(
            SubjectId::parse("alice@x.edu").unwrap(),
            "",
            Role::Curator,
            false,
        );
        let boss = Principal::new(
            SubjectId::parse("boss@x.edu").unwrap(),
            "",
            Role::Admin,
            true,
        );
        assert!(alice.may_edit(Some("alice@x.edu")), "their own");
        assert!(!alice.may_edit(Some("bob@x.edu")), "not a peer's");
        assert!(boss.may_edit(Some("bob@x.edu")), "an admin moderates");
        // The loopback operator is an admin, so notes written on a laptop stay
        // editable after that home is later served behind a proxy.
        assert!(Principal::local_operator().may_edit(Some("local")));
    }

    #[test]
    fn the_local_operator_is_not_claimable_by_a_remote_user() {
        // `matches` treats a bare "local" as the loopback operator so those
        // notes stay editable — which means a proxy identity of "local" would
        // otherwise inherit every one of them.
        assert_eq!(SubjectId::parse("local"), Some(SubjectId::local()));
        assert_eq!(SubjectId::parse_remote("local"), None);
        assert_eq!(SubjectId::parse_remote("Local"), None);
        assert_eq!(SubjectId::parse_remote("urn:indice:user:local"), None);
        // Anyone else is unaffected.
        assert!(SubjectId::parse_remote("alice@x.edu").is_some());
        assert!(SubjectId::parse_remote("localadmin").is_some());
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
