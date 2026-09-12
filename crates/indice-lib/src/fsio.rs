//! Writing a file so that a crash cannot leave it half-written.
//!
//! Most state in a indice home is a whole file rewritten on change: the
//! manifest, a finding aid, a crawl note, an annotation store. The obvious way
//! to do that, [`std::fs::write`], **truncates the file and then writes it**,
//! which leaves two windows where a reader or a crash sees something that was
//! never a valid document:
//!
//! - a crash between truncate and write leaves the file empty or short, and the
//!   previous contents are gone;
//! - a concurrent reader can observe the partial file, so an unrelated
//!   operation fails for reasons that have nothing to do with it.
//!
//! Neither is hypothetical here. The second one is how an over-wide attribution
//! bug got its teeth: `Manifest::open` failing on a partially-written
//! `waczs.json` turned a "read the manifest first" step into "assume the
//! manifest was empty".
//!
//! [`write_atomic`] closes both by writing a temp file in the same directory
//! and renaming it over the target. A rename within one filesystem is atomic,
//! so a reader sees either the whole old file or the whole new one, and a crash
//! at any point leaves one of those two on disk.
//!
//! This does not make writing *safe*, only *all-or-nothing*. Callers that
//! read-modify-write still need to hold whatever lock serializes them, or two
//! writers will each atomically install their own stale version.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Write `contents` to `path` atomically and durably.
///
/// Creates the parent directory if needed. The sequence is deliberate:
///
/// 1. write the bytes to a temp file **in the same directory**, because a
///    rename is only atomic within a filesystem and `/tmp` is often a
///    different one;
/// 2. `fsync` the temp file, so its bytes are on disk before anything points
///    at them (rename otherwise happily publishes a file whose contents are
///    still only in the page cache);
/// 3. rename it over the target, which is the atomic step;
/// 4. `fsync` the parent directory, so the rename itself survives power loss.
///
/// Step 4 is best-effort: opening a directory is not portable, and on Windows
/// it fails outright. Skipping it costs durability of the *rename* across a
/// hard power cut, never atomicity, so a failure here is not worth failing the
/// write over.
///
/// The two syncs cost a few milliseconds. Every caller is already doing far
/// more work than that (parsing a WACZ, committing a Tantivy segment), so
/// there is no non-syncing variant to choose wrongly between.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating a temp file in {}", parent.display()))?;
    tmp.write_all(contents)
        .with_context(|| format!("writing {}", path.display()))?;
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("flushing {} to disk", path.display()))?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("finalizing {}", path.display()))?;

    // Best-effort; see the note above.
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// [`write_atomic`] for text, which is what every caller in the crate has.
pub(crate) fn write_atomic_str(path: &Path, contents: &str) -> Result<()> {
    write_atomic(path, contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_a_file_without_a_window_where_it_is_short() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        write_atomic_str(&path, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        // A shorter replacement is the case plain `fs::write` handles worst:
        // truncate-then-write would briefly leave the old tail visible.
        write_atomic_str(&path, "second, and longer").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "second, and longer"
        );
        write_atomic_str(&path, "3").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "3");
    }

    #[test]
    fn creates_missing_parents() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a/b/c/state.json");
        write_atomic_str(&path, "hi").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi");
    }

    #[test]
    fn leaves_no_temp_files_behind() {
        // A stray temp file in `collections/<slug>/` would show up in a
        // curator's git status, which is a real cost in a committed directory.
        let tmp = tempfile::TempDir::new().unwrap();
        for i in 0..3 {
            write_atomic_str(&tmp.path().join("state.json"), &format!("{i}")).unwrap();
        }
        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["state.json"], "{entries:?}");
    }

    #[test]
    fn a_failed_write_leaves_the_previous_contents_intact() {
        // The property that matters on a full disk or a permissions problem:
        // the old document is still there, rather than truncated away.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        write_atomic_str(&path, "the good version").unwrap();

        // A directory where the temp file wants to go makes `persist` fail.
        let blocked = tmp.path().join("sub");
        std::fs::create_dir(&blocked).unwrap();
        let target = blocked.join("inner");
        std::fs::create_dir(&target).unwrap(); // renaming a file over a dir fails
        assert!(write_atomic_str(&target, "nope").is_err());

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "the good version",
            "an unrelated failure must not disturb what is already written"
        );
    }
}
