//! The manifest's critical section.
//!
//! Every change to `waczs.json` is a read-modify-write: [`Manifest::save`]
//! rewrites the file wholesale from an in-memory vec, so a writer that reads,
//! thinks, and then saves will erase anything another writer committed in
//! between. The window is invisible in a diff, because each half looks like
//! ordinary correct code.
//!
//! [`manifest_write`] is the only sanctioned way to change it. It opens the
//! manifest, hands it to the caller, and saves — all inside one hold of the
//! manifest lock — so the read and the write cannot be separated by anything.
//! Locking `Manifest::save` alone would not do: by then the stale read has
//! already happened.
//!
//! # Why this is not the index lock
//!
//! Changing `waczs.json` writes no Tantivy documents, so a rebuild's output is
//! not stale with respect to it. Making a curator wait out a multi-hour rebuild
//! to save a description would be the wrong trade, so the manifest gets its own
//! lock, held only for as long as one read-modify-write takes.
//!
//! The consequence for the long operations is the rule that shapes this whole
//! module: **nobody may hold the manifest across a long operation.** An ingest
//! and a rebuild both used to open it once and save much later, which is
//! exactly the stale-read window above, with a multi-hour gap in the middle.
//! They now call in here once per crawl (the ingest) or once at the end (a
//! rebuild), and hold nothing in between.
//!
//! Ordering: the index lock first, this one inside it. An ingest needs both; a
//! finding-aid save needs only this one.

use std::path::Path;

use anyhow::Result;

use crate::collections::Manifest;

use super::paths::index_dir;

/// Read-modify-write the manifest under the manifest lock.
///
/// `what` names the caller for anyone who has to wait, in the same style as
/// the index lock ("a crawl deletion", "an ingest").
///
/// Keep the closure short: it runs with the lock held, and every other writer
/// in every other process is waiting on it. Do no I/O beyond the manifest, and
/// in particular never acquire another lock inside it — this is the innermost
/// one, and taking the index lock or the server's mutex here would invert the
/// order and deadlock.
///
/// The manifest is saved only if the closure returns `Ok`. A closure that
/// fails leaves the file untouched, so a caller can bail out mid-change
/// without having to undo anything.
pub(crate) fn manifest_write<T>(
    home: &Path,
    what: &str,
    f: impl FnOnce(&mut Manifest) -> Result<T>,
) -> Result<T> {
    let _guard = super::lock::lock_manifest(home, what)?;
    let index_dir = index_dir(home);
    let mut manifest = Manifest::open(&index_dir)?;
    let out = f(&mut manifest)?;
    manifest.save()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the helper: a writer that reads, pauses, and saves must not
    /// erase what another writer committed meanwhile.
    ///
    /// Both sides go through `manifest_write`, from two threads, so they
    /// contend on the real flock. Without the lock the slow writer's save
    /// rewrites the whole file from its stale vec and the fast writer's crawl
    /// is gone — which is the bug this module exists for, and which
    /// `Manifest::save` makes invisible in a diff because both halves look
    /// like ordinary correct code.
    #[test]
    fn a_slow_writer_does_not_erase_a_concurrent_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        std::fs::create_dir_all(index_dir(&home)).unwrap();

        let entry = |id: &str| -> crate::collections::Wacz {
            serde_json::from_str(&format!(
                r#"{{"id":"{id}","collection":"c","path":"/{id}.wacz","name":"{id}",
                     "date_indexed":"2026-01-01T00:00:00Z","file_size":1,"sha256":"x"}}"#
            ))
            .unwrap()
        };

        let (paused_tx, paused_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let slow = {
            let home = home.clone();
            std::thread::spawn(move || {
                manifest_write(&home, "slow", |m| {
                    m.upsert_wacz(entry("slow"));
                    // Stand in for the thinking a real writer does between
                    // reading and saving.
                    paused_tx.send(()).unwrap();
                    let _ = go_rx.recv();
                    Ok(())
                })
                .unwrap();
            })
        };
        paused_rx.recv().unwrap();

        // The fast writer must not be able to slip in mid-hold.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let fast = {
            let home = home.clone();
            std::thread::spawn(move || {
                manifest_write(&home, "fast", |m| {
                    m.upsert_wacz(entry("fast"));
                    Ok(())
                })
                .unwrap();
                done_tx.send(()).unwrap();
            })
        };
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "the second writer must wait for the first's whole read-modify-write"
        );
        go_tx.send(()).unwrap();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("and proceed once it is done");
        slow.join().unwrap();
        fast.join().unwrap();

        let m = Manifest::open(&index_dir(&home)).unwrap();
        let ids: Vec<&str> = m.waczs.iter().map(|w| w.id.as_str()).collect();
        assert!(
            ids.contains(&"slow") && ids.contains(&"fast"),
            "got {ids:?}"
        );
    }

    /// A closure that fails leaves the file alone, so a caller can bail out
    /// mid-change without undoing anything.
    #[test]
    fn a_failed_change_is_not_saved() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(index_dir(home)).unwrap();
        manifest_write(home, "seed", |m| {
            m.upsert_wacz(
                serde_json::from_str(
                    r#"{"id":"keep","collection":"c","path":"/k.wacz","name":"keep",
                        "date_indexed":"2026-01-01T00:00:00Z","file_size":1,"sha256":"x"}"#,
                )
                .unwrap(),
            );
            Ok(())
        })
        .unwrap();

        let err = manifest_write(home, "doomed", |m| -> Result<()> {
            m.remove_wacz("keep");
            anyhow::bail!("changed my mind")
        });
        assert!(err.is_err());

        let m = Manifest::open(&index_dir(home)).unwrap();
        assert_eq!(m.waczs.len(), 1, "the failed change must not have landed");
    }

    /// The manifest lock is not the index lock: holding one must not block the
    /// other, or a description could not be saved during a rebuild.
    #[test]
    fn the_manifest_lock_is_independent_of_the_index_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let _index =
            super::super::lock::lock_index(home, "reindex", crate::index::no_progress()).unwrap();
        // Would block forever if these shared a file.
        manifest_write(home, "a finding aid save", |_| Ok(())).unwrap();
    }
}
