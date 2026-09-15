//! Cross-process exclusion for operations that write the search index.
//!
//! # Why this exists, and why Tantivy's own lock is not enough
//!
//! Tantivy takes an `INDEX_WRITER_LOCK` (`.tantivy-writer.lock`) so only one
//! `IndexWriter` can exist at a time, and that genuinely covers two concurrent
//! ingests: both open `index/full_text`, so the second fails immediately with
//! `LockFailure`.
//!
//! It does not cover a **rebuild**, and that is the case where the loss is
//! total. `reindex` builds into the *sibling* directory `index/full_text.new`,
//! and Tantivy's lock file is named relative to the index directory it was
//! opened on, so a rebuild and an ingest take two different locks and neither
//! sees the other. What then happens is:
//!
//! 1. `indice reindex` reads the manifest and starts building `full_text.new`.
//! 2. A curator adds a crawl through the workroom. It indexes into `full_text`
//!    and saves the manifest, both successfully.
//! 3. The rebuild finishes and swaps: `full_text` is renamed to
//!    `full_text.old`, `full_text.new` is promoted, and `full_text.old` is
//!    **deleted** (see [`swap`](super::swap)). The new crawl's documents go
//!    with it, and the rebuild then saves its own copy of the manifest, read
//!    back at step 1, erasing the entry too.
//!
//! Both halves are gone, silently, with no error on either side. There is no
//! server rebuild endpoint, so this is `indice reindex` against a serving
//! `serve --manage` — a workflow [`server`](crate::server) documents as
//! supported, which is why the lock has to be visible across processes rather
//! than being a mutex inside one.
//!
//! # What it protects, and what it deliberately does not
//!
//! This is the **index** tier: it serializes operations that write documents.
//! It is not the manifest's critical section. A write that only touches
//! `waczs.json` or a finding aid adds no documents, so a rebuild's output is
//! not stale with respect to it, and making a curator wait hours to save a
//! description would be the wrong trade. Those writes get their own, always
//! brief, hold (see bead `rustyweb-durable-writes-f4h5`).
//!
//! Readers take nothing. A reader opens the index read-only, Tantivy's
//! `META_LOCK` already stops segment files being collected from under a
//! reloading reader, and a shared lock here would queue every page render
//! behind a multi-hour rebuild.
//!
//! # Mechanism
//!
//! `flock(2)` via [`std::fs::File::lock`], on `<index_dir>/.index.lock`.
//!
//! - **The lock file is never renamed or replaced.** An `flock` belongs to the
//!   open file description, hence to the inode. If the file were replaced by a
//!   rename (as [`fsio::write_atomic`](crate::fsio::write_atomic) does), a
//!   holder would be holding an orphaned inode while the next process locked
//!   the new one: two "exclusive" holders, no error, no symptom until data is
//!   lost. So the holder line is written **in place**, which is this crate's
//!   one deliberate exception to the atomic-write rule, precisely because here
//!   the inode *is* the lock.
//! - **It never goes stale.** The OS releases an `flock` on panic, `exit` and
//!   `SIGKILL`, so unlike Tantivy's writer lock there is no leftover file to
//!   delete by hand after a crash.
//! - **It queues rather than failing.** Tantivy's writer lock is
//!   create-exclusive and non-blocking, so a second ingest fails outright;
//!   here the second waits, after saying who it is waiting for.
//! - **Not honored across a network filesystem.** On NFS/SMB/sshfs `flock` may
//!   be emulated or a silent no-op, and the re-entrancy map below cannot see
//!   another host at all. Two hosts writing one home degrades to the old
//!   behaviour. Documented rather than detected: you cannot probe for it
//!   without a second host.
//! - **Not FIFO.** `flock` makes no fairness guarantee, so a waiter can be
//!   overtaken.
//!
//! # Lock ordering
//!
//! The server still has its own in-process `AppState.write_lock`, and a job
//! takes that *before* calling into an ingest, which then takes this one. That
//! order is not enforced anywhere, so it is worth saying why it cannot invert:
//! nothing under `index/` can see `AppState`, so a library operation can never
//! take the server's mutex, and the server never takes this lock directly. The
//! same will hold for the manifest's critical section when it arrives — index
//! tier first, manifest tier inside it, because an ingest needs both while a
//! finding-aid save needs only the second.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::paths::index_dir;
use super::IndexProgress;

/// The lock file's name inside `<home>/index/`.
///
/// `index/` is derived state and already gitignored, so this never shows up in
/// a curator's `git status`.
const LOCK_FILE: &str = ".index.lock";

thread_local! {
    /// How many times *this thread* currently holds each lock file.
    ///
    /// `flock` conflicts between two open file descriptions *including two in
    /// the same process*, so any nesting deadlocks against itself — and it
    /// deadlocks, rather than failing, which is the worst way for this to go
    /// wrong. Nothing nests today; the near-term case is `delete_collection`
    /// taking the lock and then calling `delete_crawl` per member, which
    /// arrives with the short request-path writers.
    ///
    /// A thread-local is sound because every write path runs synchronously
    /// inside one `spawn_blocking` closure, and the guard is `!Send`, so it
    /// cannot be moved to a thread whose count would not know about it.
    static DEPTH: RefCell<HashMap<PathBuf, u32>> = RefCell::new(HashMap::new());
}

/// Exclusive permission to write the archive's search index, for as long as
/// this value lives.
///
/// Obtained from [`lock_index`]. Dropping it releases the lock (or, for a
/// nested acquisition, hands it back to the outer guard).
pub struct IndexLock {
    path: PathBuf,
    /// The locked file, held only by the *outermost* guard on this thread. A
    /// nested guard carries `None` and merely decrements the depth on drop.
    file: Option<File>,
    /// Makes the guard `!Send`, which is load-bearing twice over: the
    /// thread-local depth count is only correct if the guard stays on its
    /// thread, and an axum handler then *cannot* hold the index lock across an
    /// `.await`.
    _not_send: PhantomData<*const ()>,
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        DEPTH.with(|d| {
            let mut d = d.borrow_mut();
            match d.get_mut(&self.path) {
                Some(n) if *n > 1 => *n -= 1,
                _ => {
                    d.remove(&self.path);
                }
            }
        });
        // Only the outermost guard holds the file, and dropping it releases the
        // flock. Deliberately no `remove_file`: unlinking it would let the next
        // process create and lock a *different* inode while someone still holds
        // this one.
        if let Some(f) = self.file.take() {
            let _ = f.unlock();
        }
    }
}

fn lock_path(home: &Path) -> PathBuf {
    index_dir(home).join(LOCK_FILE)
}

/// Take the index write lock for `home`, blocking until it is free.
///
/// `what` names the operation ("reindex", "index") and is recorded in the lock
/// file so a waiter can say who it is waiting for rather than looking hung.
/// If the lock is already held, that is reported through `progress` and at INFO
/// before blocking — the same try-then-report-then-block shape the server's
/// job queue already uses.
///
/// Re-entrant: taking it again on the same thread succeeds immediately.
pub(crate) fn lock_index(
    home: &Path,
    what: &str,
    progress: &dyn IndexProgress,
) -> Result<IndexLock> {
    let path = lock_path(home);

    // Already ours on this thread: hand back a nested guard without touching
    // the file, since flock would conflict with our own open description.
    let nested = DEPTH.with(|d| {
        let mut d = d.borrow_mut();
        match d.get_mut(&path) {
            Some(n) => {
                *n += 1;
                true
            }
            None => false,
        }
    });
    if nested {
        return Ok(IndexLock {
            path,
            file: None,
            _not_send: PhantomData,
        });
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating index dir {}", dir.display()))?;
    }
    // `write(true)` matters beyond writing the holder line: Windows'
    // `LockFileEx` needs the handle opened for writing.
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening the index lock {}", path.display()))?;

    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            let holder = read_holder(&mut file).unwrap_or_else(|| "another operation".to_string());
            let msg = format!("waiting for {holder} to finish…");
            progress.phase(&msg);
            tracing::info!("index is locked by {holder}; waiting");
            file.lock()
                .with_context(|| format!("waiting for the index lock {}", path.display()))?;
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!("checking the index lock {} (is this home on a network filesystem? see the module docs)", path.display())
            })
        }
    }

    // Held. Record who we are, in place — see the module docs on why this one
    // write is not atomic.
    write_holder(&mut file, what);

    DEPTH.with(|d| d.borrow_mut().insert(path.clone(), 1));
    Ok(IndexLock {
        path,
        file: Some(file),
        _not_send: PhantomData,
    })
}

/// Describe the current holder, for a waiter's message. Best-effort: a lock
/// taken by an older version (or interrupted before it wrote its line) has no
/// holder recorded, and that must not turn into an error.
fn read_holder(file: &mut File) -> Option<String> {
    let mut s = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_to_string(&mut s).ok()?;
    let line = s.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Stamp `pid`, operation and start time into the lock file. Best-effort: the
/// lock is held by the `flock`, not by this text, so a failure to write it
/// costs a helpful message and nothing else.
fn write_holder(file: &mut File, what: &str) {
    let line = format!(
        "indice {what} (pid {}), started {}",
        std::process::id(),
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    let _ = file.set_len(0);
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.write_all(line.as_bytes());
    let _ = file.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::no_progress;

    #[test]
    fn a_second_holder_waits_for_the_first() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        let first = lock_index(&home, "index", no_progress()).unwrap();

        // A second *thread* gets its own depth count and its own open file
        // description, so it contends on the flock for real.
        let (tx, rx) = std::sync::mpsc::channel();
        let h = {
            let home = home.clone();
            std::thread::spawn(move || {
                let g = lock_index(&home, "reindex", no_progress()).unwrap();
                tx.send(()).unwrap();
                drop(g);
            })
        };
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "the second acquisition must not succeed while the first is held"
        );
        drop(first);
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("it proceeds once the first releases");
        h.join().unwrap();
    }

    /// Without re-entrancy this deadlocks rather than failing, so a plain
    /// `#[test]` would hang the suite. Keep the nesting shallow and obvious.
    #[test]
    fn taking_it_again_on_the_same_thread_succeeds() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let outer = lock_index(home, "index", no_progress()).unwrap();
        {
            let inner = lock_index(home, "index", no_progress()).unwrap();
            assert!(inner.file.is_none(), "the nested guard owns no file");
            assert_eq!(DEPTH.with(|d| d.borrow()[&lock_path(home)]), 2);
        }
        assert_eq!(
            DEPTH.with(|d| d.borrow()[&lock_path(home)]),
            1,
            "dropping the inner guard hands the lock back, it does not release it"
        );
        drop(outer);
        assert!(
            DEPTH.with(|d| d.borrow().get(&lock_path(home)).is_none()),
            "and the outermost drop clears the entry"
        );
    }

    #[test]
    fn two_homes_do_not_contend() {
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        let _ga = lock_index(a.path(), "index", no_progress()).unwrap();
        let _gb = lock_index(b.path(), "index", no_progress()).unwrap();
    }

    #[test]
    fn the_holder_is_recorded_for_a_waiter_to_report() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let _g = lock_index(home, "reindex", no_progress()).unwrap();
        let mut f = File::open(lock_path(home)).unwrap();
        let holder = read_holder(&mut f).expect("a holder line");
        assert!(
            holder.starts_with("indice reindex (pid "),
            "unexpected holder line: {holder}"
        );
    }

    #[test]
    fn the_lock_file_survives_release_so_the_inode_is_stable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let inode = {
            let _g = lock_index(home, "index", no_progress()).unwrap();
            file_id(&lock_path(home))
        };
        assert!(lock_path(home).exists(), "releasing must not unlink it");
        let _g = lock_index(home, "index", no_progress()).unwrap();
        assert_eq!(
            inode,
            file_id(&lock_path(home)),
            "re-locking must find the same inode, or two holders could coexist"
        );
    }

    fn file_id(p: &Path) -> u64 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(p).unwrap().ino()
        }
        #[cfg(not(unix))]
        {
            let _ = p;
            0
        }
    }
}
