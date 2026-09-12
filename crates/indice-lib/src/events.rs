//! An append-only record of who changed what.
//!
//! The manifest says who *currently* holds a crawl
//! ([`Wacz::added_by`](crate::collections::Wacz::added_by)); this says what
//! happened. Together they answer "who deleted this collection", which indice
//! could not answer at all before: nothing recorded an actor, and the request
//! span logs the proxy's IP rather than the user's.
//!
//! # Append-only, literally
//!
//! Every other state file in `<home>` is rewritten wholesale on change. This
//! one must not be. A record that can be rewritten is not an audit trail, and
//! a truncating rewrite would destroy history rather than just risking it. So
//! writes go through `O_APPEND` and one line per record, never
//! read-modify-write, and there is deliberately no function here that edits or
//! removes an event.
//!
//! # Not published
//!
//! Unlike `annotations.jsonl`, which is designed to be committed and served,
//! this holds login identifiers and is **never** exposed over HTTP. No route
//! reads it. An operator who publishes their home directory should either
//! accept that or add `events/` to `.gitignore`; see the home-directory
//! reference.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::SubjectId;

/// Directory holding the monthly event logs, relative to a indice home.
pub const EVENTS_DIR: &str = "events";

/// What happened.
///
/// Mutations only. Reads are not recorded (an archive is meant to be read, and
/// the volume would swamp the signal), and neither are authentication failures
/// (valuable, but far noisier, and they belong in the operator's log rather
/// than in a file that lives with the archive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Created or edited a collection's finding aid. One variant rather than
    /// two because the endpoint behind it is a single create-or-update, and
    /// telling them apart would mean an extra manifest read purely to pick a
    /// label.
    CollectionSet,
    CollectionDelete,
    CrawlAdd,
    CrawlUpload,
    CrawlDelete,
    ImportBrowsertrix,
    ImportArchiveIt,
    AnnotationCreate,
    AnnotationUpdate,
    AnnotationDelete,
}

/// One authorized attempt to change the archive.
///
/// An *attempt*, deliberately: the record is written when the action has passed
/// authorization and is about to run, not after it succeeds. An operation that
/// fails part way, or crashes the process, is exactly when the record matters
/// most, and auditing afterwards is precisely when you would lose it. An
/// `outcome` field can be added later without breaking existing logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// RFC 3339, second precision. Also picks the file (see [`append`]).
    pub time: String,
    /// Who acted, as their canonical identity. Never a display name: this is a
    /// record, and a display name can be changed in `users.yaml` afterwards.
    pub actor: SubjectId,
    pub action: Action,
    /// What was acted on: a collection slug, a crawl id, an annotation id.
    pub target: String,
    /// Anything else worth keeping, e.g. `{"with_crawls": true}` on a
    /// collection delete. Never the content of a note, which already lives in
    /// `annotations.jsonl` with its own git history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl Event {
    pub fn new(actor: &SubjectId, action: Action, target: impl Into<String>) -> Self {
        Event {
            time: now_rfc3339(),
            actor: actor.clone(),
            action,
            target: target.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// `<home>/events`.
pub fn events_dir(home: &Path) -> PathBuf {
    home.join(EVENTS_DIR)
}

/// The file an event belongs in: `<home>/events/<YYYY-MM>.jsonl`.
///
/// Monthly files give rotation for almost nothing, and retrofitting rotation
/// onto one ever-growing file is a job you only want to do once. The month
/// comes from the event's own timestamp rather than a second clock read, so a
/// record can never land in a file that disagrees with it.
pub fn event_path(home: &Path, event: &Event) -> PathBuf {
    events_dir(home).join(month_file(&event.time))
}

/// The file name for a timestamp, built from *parsed numbers* rather than from
/// a slice of the input.
///
/// [`Event::time`] is a public field, so nothing structurally stops a caller
/// setting it to `"../../et"`; slicing seven bytes off it and joining that to a
/// directory would then escape the events directory. Re-formatting a parsed
/// year and month means the result is derived from two integers and can only
/// ever be `NNNN-NN.jsonl`, whatever the input was. An unparseable timestamp
/// lands in `unknown.jsonl` rather than being silently dropped.
fn month_file(time: &str) -> String {
    let parsed = (|| {
        let year: u16 = time.get(..4)?.parse().ok()?;
        if time.as_bytes().get(4) != Some(&b'-') {
            return None;
        }
        let month: u8 = time.get(5..7)?.parse().ok()?;
        (1..=12).contains(&month).then_some((year, month))
    })();
    match parsed {
        Some((year, month)) => format!("{year:04}-{month:02}.jsonl"),
        None => "unknown.jsonl".to_string(),
    }
}

/// Append one event.
///
/// `O_APPEND` means concurrent writers cannot overwrite each other: the seek to
/// the end and the write are one atomic step, so two processes appending at
/// once produce two records rather than one clobbering the other. The record is
/// serialized fully before the file is opened, and written in a single
/// `write_all`, so a line does not interleave with another writer's.
///
/// No fsync. An audit record is worth a little less than the operation it
/// describes, and syncing on every management write would be a poor trade; a
/// power loss can lose the tail of the log.
pub fn append(home: &Path, event: &Event) -> Result<()> {
    let mut line = serde_json::to_string(event).context("serializing an audit event")?;
    line.push('\n');
    let path = event_path(home, event);
    // Same gate the atomic writers use. `month_file` already makes the file
    // name safe by construction, so this is the backstop for whatever gets
    // added next.
    crate::fsio::ensure_within(home, &path)?;
    let dir = events_dir(home);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {} to append", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("appending to {}", path.display()))?;
    Ok(())
}

/// Read every event in a month's file, skipping unparseable lines.
///
/// For operator tooling and tests. Unparseable lines are skipped rather than
/// failing the read: a truncated tail from a power loss should not make the
/// rest of the history unreadable.
pub fn read_month(home: &Path, month: &str) -> Result<Vec<Event>> {
    // Through `month_file` for the same reason `event_path` is: the argument
    // reaches a path join, and operator tooling may well pass something it got
    // from elsewhere.
    let path = events_dir(home).join(month_file(month));
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(s: &str) -> SubjectId {
        SubjectId::parse(s).unwrap()
    }

    fn month_of(e: &Event) -> String {
        e.time[..7].to_string()
    }

    #[test]
    fn appending_never_rewrites_what_is_already_there() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let first = Event::new(&actor("alice@x.edu"), Action::CrawlAdd, "collection-a");
        append(home, &first).unwrap();
        let after_first = std::fs::read(event_path(home, &first)).unwrap();

        let second = Event::new(&actor("boss@x.edu"), Action::CrawlDelete, "abc123");
        append(home, &second).unwrap();
        let after_second = std::fs::read(event_path(home, &second)).unwrap();

        // The whole point: the first record is still there, byte for byte, as a
        // prefix. A rewrite would satisfy "two lines" but not this.
        assert!(
            after_second.starts_with(&after_first),
            "the existing history must be untouched"
        );
        let events = read_month(home, &month_of(&first)).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].action, Action::CrawlAdd);
        assert_eq!(events[0].actor, actor("alice@x.edu"));
        assert_eq!(events[1].action, Action::CrawlDelete);
    }

    #[test]
    fn events_are_filed_by_their_own_month() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut jan = Event::new(&actor("a@x.edu"), Action::CollectionSet, "c");
        jan.time = "2026-01-15T00:00:00Z".into();
        let mut feb = Event::new(&actor("a@x.edu"), Action::CollectionSet, "c");
        feb.time = "2026-02-01T00:00:00Z".into();
        append(tmp.path(), &jan).unwrap();
        append(tmp.path(), &feb).unwrap();

        assert!(event_path(tmp.path(), &jan).ends_with("2026-01.jsonl"));
        assert!(event_path(tmp.path(), &feb).ends_with("2026-02.jsonl"));
        assert_eq!(read_month(tmp.path(), "2026-01").unwrap().len(), 1);
        assert_eq!(read_month(tmp.path(), "2026-02").unwrap().len(), 1);
        assert!(read_month(tmp.path(), "2026-03").unwrap().is_empty());
    }

    #[test]
    fn a_hostile_timestamp_cannot_escape_the_events_directory() {
        // `time` is a public field, so this is reachable by construction even
        // though nothing in the crate does it. The file name is rebuilt from
        // parsed integers, so traversal has nothing to work with.
        let tmp = tempfile::TempDir::new().unwrap();
        for hostile in [
            "../../et",
            "/etc/pas",
            "..",
            "",
            "2026-13-01T00:00:00Z", // month out of range
            "abcd-ef",
            "2026_09",
        ] {
            let mut e = Event::new(&actor("a@x.edu"), Action::CrawlAdd, "c");
            e.time = hostile.to_string();
            let path = event_path(tmp.path(), &e);
            assert_eq!(
                path.parent(),
                Some(events_dir(tmp.path()).as_path()),
                "{hostile:?} escaped to {path:?}"
            );
            assert_eq!(path.file_name().unwrap(), "unknown.jsonl", "{hostile:?}");
            // And it is still actually writable, rather than erroring out.
            append(tmp.path(), &e).unwrap();
        }
        assert_eq!(read_month(tmp.path(), "unknown").unwrap().len(), 7);
        // Reading is guarded the same way, since that argument also reaches a
        // path join. A hostile month is redirected into the `unknown` bucket
        // rather than escaping, so it reads that file and never the one named.
        assert_eq!(read_month(tmp.path(), "../../etc/passwd").unwrap().len(), 7);
    }

    #[test]
    fn a_truncated_tail_does_not_hide_the_rest() {
        // A power loss can leave a partial last line. The earlier history is
        // still readable, which is the whole reason for one record per line.
        let tmp = tempfile::TempDir::new().unwrap();
        let e = Event::new(&actor("a@x.edu"), Action::CrawlAdd, "c");
        append(tmp.path(), &e).unwrap();
        let path = event_path(tmp.path(), &e);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(br#"{"time":"2026-09-01T00:00:00Z","actor":"mailto:a@x."#)
            .unwrap();
        drop(f);
        assert_eq!(read_month(tmp.path(), &month_of(&e)).unwrap().len(), 1);
    }

    #[test]
    fn detail_rides_along_and_is_omitted_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let plain = Event::new(&actor("a@x.edu"), Action::CrawlAdd, "c");
        let line = serde_json::to_string(&plain).unwrap();
        assert!(!line.contains("detail"), "{line}");

        let detailed = Event::new(&actor("a@x.edu"), Action::CollectionDelete, "c")
            .with_detail(serde_json::json!({ "with_crawls": true }));
        append(tmp.path(), &detailed).unwrap();
        let back = read_month(tmp.path(), &month_of(&detailed)).unwrap();
        assert_eq!(
            back[0].detail,
            Some(serde_json::json!({"with_crawls": true}))
        );
    }

    #[test]
    fn an_unwritable_home_is_an_error_the_caller_can_swallow() {
        // `append` reports failure rather than panicking, so a call site can
        // log and carry on: a full disk must not take management offline.
        let tmp = tempfile::TempDir::new().unwrap();
        let blocked = tmp.path().join("not-a-dir");
        std::fs::write(&blocked, b"i am a file").unwrap();
        let e = Event::new(&actor("a@x.edu"), Action::CrawlAdd, "c");
        assert!(append(&blocked, &e).is_err());
    }
}
