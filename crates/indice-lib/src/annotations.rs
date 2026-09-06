//! Page-level annotations — a public, committable provenance layer over the
//! archive (see DESIGN.md, "Annotations as provenance"; bead
//! `rustyweb-page-annotations-gnqf`).
//!
//! Each annotation is a [W3C Web Annotation] attached to a *specific capture*
//! (a page URL + its capture timestamp), either whole-page or anchored to a
//! highlighted passage via a [`TextQuoteSelector`]. Annotations are stored one
//! JSON object per line in `<home>/collections/<slug>/annotations.jsonl`,
//! alongside the finding aid and crawl notes — so they diff cleanly and travel
//! with the collection in version control.
//!
//! This module is the **storage layer** only (bead `gnqf.2`): the data model
//! plus read/write helpers. Deciding *which* collection a capture belongs to,
//! authenticating the author, and rendering all live at higher layers.
//!
//! [W3C Web Annotation]: https://www.w3.org/TR/annotation-model/
//! [`TextQuoteSelector`]: https://www.w3.org/TR/annotation-model/#text-quote-selector

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::collections::collection_dir;

/// The JSON-LD context every annotation carries.
const ANNO_CONTEXT: &str = "http://www.w3.org/ns/anno.jsonld";

/// A single page annotation, shaped as a W3C Web Annotation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Annotation {
    /// JSON-LD `@context` (always [`ANNO_CONTEXT`]).
    #[serde(rename = "@context")]
    pub context: String,
    /// Opaque, stable id (`urn:indice:annotation:<hex>`), minted at creation.
    pub id: String,
    /// JSON-LD type (always `"Annotation"`).
    #[serde(rename = "type")]
    pub kind: String,
    /// RFC-3339 creation time.
    pub created: String,
    /// RFC-3339 last-edit time, set on update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// Who wrote it.
    pub creator: Creator,
    /// What capture (and optionally what passage) it is attached to.
    pub target: Target,
    /// The note itself.
    pub body: Body,
}

/// The author of an annotation. `id` is the stable **author key** used to gate
/// edits/deletes (e.g. the identity a proxy forwards); `name` is the public
/// display name. Both are optional so callers can decide their privacy posture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Creator {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Creator {
    /// A `Person` creator with an author key and display name.
    pub fn person(id: impl Into<String>, name: impl Into<String>) -> Self {
        Creator {
            kind: Some("Person".into()),
            id: Some(id.into()),
            name: Some(name.into()),
        }
    }
}

/// What an annotation points at: a captured page, optionally a passage within it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Target {
    /// The archived page's original URL.
    pub source: String,
    /// The capture timestamp that pins the note to one memento (indice's
    /// 14-digit form, e.g. `20260828210522`). Keying on URL + timestamp is what
    /// lets annotations survive reindexing.
    pub timestamp: String,
    /// A region selector; absent means the note applies to the whole page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<Selector>,
}

/// How a region within a page is anchored. `TextQuoteSelector` (the quoted text
/// plus a little surrounding context) is the most robust anchor across markup
/// changes, so it is the only variant for now.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum Selector {
    TextQuoteSelector {
        exact: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suffix: Option<String>,
    },
}

/// The note text (Markdown).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Body {
    #[serde(rename = "type")]
    pub kind: String,
    pub format: String,
    pub value: String,
}

impl Body {
    fn markdown(value: impl Into<String>) -> Self {
        Body {
            kind: "TextualBody".into(),
            format: "text/markdown".into(),
            value: value.into(),
        }
    }
}

impl Annotation {
    /// A whole-page annotation on the capture `(url, timestamp)`.
    pub fn page(
        url: impl Into<String>,
        timestamp: impl Into<String>,
        note: impl Into<String>,
        creator: Creator,
    ) -> Self {
        Annotation::build(url, timestamp, None, note, creator)
    }

    /// A region annotation anchored to `selector` within the capture.
    pub fn region(
        url: impl Into<String>,
        timestamp: impl Into<String>,
        selector: Selector,
        note: impl Into<String>,
        creator: Creator,
    ) -> Self {
        Annotation::build(url, timestamp, Some(selector), note, creator)
    }

    fn build(
        url: impl Into<String>,
        timestamp: impl Into<String>,
        selector: Option<Selector>,
        note: impl Into<String>,
        creator: Creator,
    ) -> Self {
        Annotation {
            context: ANNO_CONTEXT.into(),
            id: mint_id(),
            kind: "Annotation".into(),
            created: now_rfc3339(),
            modified: None,
            creator,
            target: Target {
                source: url.into(),
                timestamp: timestamp.into(),
                selector,
            },
            body: Body::markdown(note),
        }
    }

    /// The author key, if any — used to gate edits/deletes.
    fn author_key(&self) -> Option<&str> {
        self.creator.id.as_deref()
    }
}

/// Outcome of an author-gated delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    /// The change was applied.
    Done,
    /// No annotation with that id exists in the collection.
    NotFound,
    /// The annotation exists but belongs to a different author.
    Forbidden,
}

/// Outcome of an author-gated update: the updated annotation on success (so the
/// caller needn't reload the store), or why it was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateResult {
    Updated(Box<Annotation>),
    NotFound,
    Forbidden,
}

/// Path to a collection's annotation store: `<home>/collections/<slug>/annotations.jsonl`.
pub fn annotations_path(home: &Path, collection: &str) -> PathBuf {
    collection_dir(home, collection).join("annotations.jsonl")
}

/// Load every annotation in a collection (empty if the store doesn't exist yet),
/// preserving file order. A malformed line aborts the load with context, rather
/// than being silently dropped.
pub fn load(home: &Path, collection: &str) -> Result<Vec<Annotation>> {
    ensure_safe_collection(collection)?;
    let path = annotations_path(home, collection);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let a: Annotation = serde_json::from_str(line)
            .with_context(|| format!("parsing annotation at {}:{}", path.display(), i + 1))?;
        out.push(a);
    }
    Ok(out)
}

/// Annotations attached to one capture `(url, timestamp)`, in file order.
pub fn list_by_page(
    home: &Path,
    collection: &str,
    url: &str,
    timestamp: &str,
) -> Result<Vec<Annotation>> {
    Ok(load(home, collection)?
        .into_iter()
        .filter(|a| a.target.source == url && a.target.timestamp == timestamp)
        .collect())
}

/// Fetch one annotation by id.
pub fn get(home: &Path, collection: &str, id: &str) -> Result<Option<Annotation>> {
    Ok(load(home, collection)?.into_iter().find(|a| a.id == id))
}

/// Append a new annotation to the collection's store, creating the file (and the
/// collection directory) if needed. The note body must be non-empty.
pub fn create(home: &Path, collection: &str, annotation: &Annotation) -> Result<()> {
    ensure_safe_collection(collection)?;
    if annotation.body.value.trim().is_empty() {
        bail!("annotation body is empty");
    }
    let path = annotations_path(home, collection);
    let mut line = serde_json::to_string(annotation)?;
    line.push('\n');
    let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
    existing.push_str(&line);
    write_atomic(&path, &existing)?;
    Ok(())
}

/// Replace an annotation's note text, if it exists and `author` wrote it. Sets
/// `modified`. Returns whether it was applied, not found, or forbidden.
pub fn update(
    home: &Path,
    collection: &str,
    id: &str,
    new_note: &str,
    author: &str,
) -> Result<UpdateResult> {
    if new_note.trim().is_empty() {
        bail!("annotation body is empty");
    }
    let mut all = load(home, collection)?;
    let Some(pos) = all.iter().position(|a| a.id == id) else {
        return Ok(UpdateResult::NotFound);
    };
    if all[pos].author_key() != Some(author) {
        return Ok(UpdateResult::Forbidden);
    }
    all[pos].body = Body::markdown(new_note);
    all[pos].modified = Some(now_rfc3339());
    write_all(home, collection, &all)?;
    Ok(UpdateResult::Updated(Box::new(all[pos].clone())))
}

/// Delete an annotation, if it exists and `author` wrote it.
pub fn delete(home: &Path, collection: &str, id: &str, author: &str) -> Result<EditOutcome> {
    let mut all = load(home, collection)?;
    let Some(pos) = all.iter().position(|a| a.id == id) else {
        return Ok(EditOutcome::NotFound);
    };
    if all[pos].author_key() != Some(author) {
        return Ok(EditOutcome::Forbidden);
    }
    all.remove(pos);
    write_all(home, collection, &all)?;
    Ok(EditOutcome::Done)
}

/// Rewrite the whole store (used by update/delete). One JSON object per line.
fn write_all(home: &Path, collection: &str, annotations: &[Annotation]) -> Result<()> {
    ensure_safe_collection(collection)?;
    let path = annotations_path(home, collection);
    let mut buf = String::new();
    for a in annotations {
        buf.push_str(&serde_json::to_string(a)?);
        buf.push('\n');
    }
    write_atomic(&path, &buf)?;
    Ok(())
}

/// Reject a collection slug that could escape the collections tree. Real slugs
/// are [`crate::collections::slugify`] output — ASCII alphanumerics and hyphens —
/// so anything containing a path separator or `.` is invalid. This closes the
/// path-traversal vector (CodeQL rust/path-injection) at the storage boundary,
/// before the value reaches `collection_dir`.
fn ensure_safe_collection(collection: &str) -> Result<()> {
    let safe = !collection.is_empty()
        && collection
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !safe {
        bail!("invalid collection id: {collection:?}");
    }
    Ok(())
}

/// Write `contents` to `path` atomically: a temp file in the same directory is
/// fully written, then renamed over `path` (a rename is atomic on one
/// filesystem), so a crash mid-write can never truncate the store. Creates the
/// parent directory if needed.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating a temp file in {}", parent.display()))?;
    tmp.write_all(contents.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    tmp.flush().ok();
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("finalizing {}", path.display()))?;
    Ok(())
}

/// Mint a process-unique opaque id without pulling in a UUID/RNG crate: hash a
/// high-resolution timestamp plus a monotonic counter with SHA-256 (already a
/// dependency) and take 128 bits. Uniqueness within a process comes from the
/// counter; across processes, from the nanosecond clock.
fn mint_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = Sha256::new();
    h.update(nanos.to_le_bytes());
    h.update(n.to_le_bytes());
    let digest = h.finalize();
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("urn:indice:annotation:{hex}")
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creator() -> Creator {
        Creator::person("mailto:ada@example.org", "Ada")
    }

    #[test]
    fn round_trips_through_json() {
        let a = Annotation::region(
            "https://example.org/report",
            "20260828210522",
            Selector::TextQuoteSelector {
                exact: "later retracted".into(),
                prefix: Some("the figure was ".into()),
                suffix: Some(" by the agency".into()),
            },
            "This claim was walked back.",
            creator(),
        );
        let line = serde_json::to_string(&a).unwrap();
        let back: Annotation = serde_json::from_str(&line).unwrap();
        assert_eq!(a, back);
        // shape sanity: it's a Web Annotation
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["@context"], ANNO_CONTEXT);
        assert_eq!(v["type"], "Annotation");
        assert_eq!(v["target"]["selector"]["type"], "TextQuoteSelector");
        assert_eq!(v["body"]["format"], "text/markdown");
    }

    #[test]
    fn create_list_and_filter_by_page() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let col = "example";

        let page = Annotation::page(
            "https://example.org/a",
            "2026",
            "whole-page note",
            creator(),
        );
        let other = Annotation::page("https://example.org/b", "2026", "different page", creator());
        create(home, col, &page).unwrap();
        create(home, col, &other).unwrap();

        // stored on disk at the expected path
        assert!(annotations_path(home, col).is_file());

        let all = load(home, col).unwrap();
        assert_eq!(all.len(), 2);

        let for_a = list_by_page(home, col, "https://example.org/a", "2026").unwrap();
        assert_eq!(for_a.len(), 1);
        assert_eq!(for_a[0].body.value, "whole-page note");
        assert!(for_a[0].target.selector.is_none());
    }

    #[test]
    fn update_and_delete_are_author_gated() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let col = "example";
        let a = Annotation::page("https://example.org/a", "2026", "first", creator());
        let id = a.id.clone();
        create(home, col, &a).unwrap();

        // wrong author can't edit or delete
        assert_eq!(
            update(home, col, &id, "hacked", "mailto:eve@example.org").unwrap(),
            UpdateResult::Forbidden
        );
        assert_eq!(
            delete(home, col, &id, "mailto:eve@example.org").unwrap(),
            EditOutcome::Forbidden
        );
        assert_eq!(get(home, col, &id).unwrap().unwrap().body.value, "first");

        // author can edit (sets modified) then delete
        assert!(matches!(
            update(home, col, &id, "second", "mailto:ada@example.org").unwrap(),
            UpdateResult::Updated(_)
        ));
        let edited = get(home, col, &id).unwrap().unwrap();
        assert_eq!(edited.body.value, "second");
        assert!(edited.modified.is_some());

        assert_eq!(
            delete(home, col, &id, "mailto:ada@example.org").unwrap(),
            EditOutcome::Done
        );
        assert!(get(home, col, &id).unwrap().is_none());

        // unknown id
        assert_eq!(
            delete(home, col, "nope", "mailto:ada@example.org").unwrap(),
            EditOutcome::NotFound
        );
    }

    #[test]
    fn load_of_missing_store_is_empty() {
        let home = tempfile::tempdir().unwrap();
        assert!(load(home.path(), "nascent").unwrap().is_empty());
    }

    #[test]
    fn rejects_unsafe_collection() {
        let home = tempfile::tempdir().unwrap();
        let a = Annotation::page("u", "t", "x", creator());
        // path-traversal / separators in the collection id are refused before
        // any filesystem access.
        assert!(create(home.path(), "../evil", &a).is_err());
        assert!(load(home.path(), "../evil").is_err());
        assert!(load(home.path(), "a/b").is_err());
        assert!(load(home.path(), "").is_err());
    }

    #[test]
    fn empty_body_is_rejected() {
        let home = tempfile::tempdir().unwrap();
        let a = Annotation::page("https://example.org/a", "2026", "   ", creator());
        assert!(create(home.path(), "example", &a).is_err());
    }

    #[test]
    fn ids_are_unique() {
        let a = Annotation::page("u", "t", "x", creator());
        let b = Annotation::page("u", "t", "x", creator());
        assert_ne!(a.id, b.id);
        assert!(a.id.starts_with("urn:indice:annotation:"));
    }
}
