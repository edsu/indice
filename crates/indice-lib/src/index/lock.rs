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
//!   here the second waits, after saying who it is waiting for
//!   (`reindex (pid 4242) (started 7m ago)`, read back out of the lock file).
//! - **Not honored across a network filesystem.** On NFS/SMB/sshfs `flock` may
//!   be emulated or a silent no-op, and the re-entrancy map below cannot see
//!   another host at all. Two hosts writing one home degrades to the old
//!   behaviour. Documented rather than detected: you cannot probe for it
//!   without a second host.
//! - **Not FIFO.** `flock` makes no fairness guarantee, so a waiter can be
//!   overtaken. Release is otherwise well behaved: the kernel wakes the
//!   waiters, one acquires, the rest block again — nothing spins and nothing
//!   errors. Starvation needs the lock to be essentially never free, which
//!   several curators doing occasional adds does not produce; the shape that
//!   would is an automated writer on a schedule or in a loop competing with an
//!   interactive one. No failure is reachable today (one in-flight write at a
//!   time is the assumption throughout `server`), so this is a property to
//!   know about rather than something to build a fair queue for.
//!
//! # Lock ordering — **this lock is always taken before `AppState.write_lock`**
//!
//! The server has its own in-process `AppState.write_lock`, and every site that
//! needs both takes *this* one first. That is not a style preference: acquiring
//! this lock from inside `write_lock` parks a blocking thread on a
//! cross-process `flock` while holding the mutex that gates every workroom
//! write, so a long rebuild in another process hangs the whole write surface.
//!
//! Nothing enforces the order, and it is now genuinely invertible — so it has
//! to be a rule rather than an observation. A handler that holds `write_lock`
//! and then calls a library function which takes this lock deadlocks against a
//! job that took them in the documented order: classic AB-BA, and it hangs
//! rather than failing. The handlers that hold `write_lock` today
//! (`delete_crawl`, `delete_collection`, `set_collection`, the annotation
//! writes) are safe only because the library functions they call do not take
//! this lock *yet*. Bringing those under it — the next slice of
//! `rustyweb-durable-writes-f4h5` — means hoisting the acquisition to the top
//! of each handler, above `write_lock`, not simply adding it to the library
//! function.
//!
//! Drop order follows from that: the guards are declared index-lock-first, so
//! they drop in reverse and the mutex is released before the `flock`. A waiter
//! that wakes therefore finds the mutex already free.
//!
//! The manifest's critical section, when it arrives, sits *inside* both: an
//! ingest needs all three, while a finding-aid save needs only the manifest
//! one.

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
/// It lives under `index/` because that is derived state — DESIGN recommends
/// gitignoring it — rather than beside the curator's committable files.
const LOCK_FILE: &str = ".index.lock";

thread_local! {
    /// The locks *this thread* holds, and how many times over.
    ///
    /// The `File` lives here rather than in the guard, which is not a detail.
    /// If the guard owned it, dropping an outer guard while a nested one was
    /// still alive would release the `flock` while the depth count still said
    /// "held" — and the next acquisition on the thread would hand back a guard
    /// backed by nothing, so an ingest or rebuild would run with no exclusion
    /// at all and no error. Keeping the file here makes release depend on the
    /// count reaching zero rather than on which guard drops first, so
    /// out-of-order drops are simply correct.
    ///
    /// `flock` conflicts between two open file descriptions *including two in
    /// the same process*, so re-entrancy is what stops a legitimate nesting
    /// from deadlocking against itself — and it would hang rather than fail,
    /// which is the worst way for this to go wrong. The server relies on it: a
    /// job takes the lock for its whole run, and the `index_location` inside
    /// takes it again.
    ///
    /// A thread-local is sound because every write path runs synchronously
    /// inside one `spawn_blocking` closure, and the guard is `!Send`, so it
    /// cannot be moved to a thread whose count would not know about it.
    static HELD: RefCell<HashMap<PathBuf, Held>> = RefCell::new(HashMap::new());
}

struct Held {
    /// Dropping this releases the `flock`; `unlock` is called first so a
    /// failure is at least observable in a debugger.
    file: File,
    depth: u32,
}

/// Exclusive permission to write the archive's search index, for as long as
/// this value lives.
///
/// Obtained from [`lock_index`]. The lock is released when the last guard for
/// that archive on this thread is dropped, in whatever order they drop.
pub struct IndexLock {
    /// `None` when locking is unsupported on this filesystem and we proceeded
    /// without exclusion — the guard is then inert.
    path: Option<PathBuf>,
    /// Makes the guard `!Send`, which is load-bearing twice over: the
    /// thread-local count is only correct if the guard stays on its thread, and
    /// an axum handler then *cannot* hold the index lock across an `.await`.
    _not_send: PhantomData<*const ()>,
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else { return };
        HELD.with(|h| {
            let mut h = h.borrow_mut();
            let Some(held) = h.get_mut(&path) else { return };
            held.depth -= 1;
            if held.depth == 0 {
                // Deliberately no `remove_file`: unlinking it would let the
                // next process create and lock a *different* inode while
                // someone still holds this one.
                if let Some(held) = h.remove(&path) {
                    let _ = held.file.unlock();
                }
            }
        });
    }
}

/// Where the lock file lives for `home`, with the directory created and the
/// path canonicalized.
///
/// Canonicalizing is what makes re-entrancy work. The depth map is keyed on
/// this path, so `/srv/arc` and `/srv/arc/`, an absolute and a relative spelling
/// of the same home, or one reached through a symlink would otherwise be
/// different keys for the same inode: the nested check would miss, a second
/// file description would be opened, and `flock` would block against the
/// caller's own outer description — forever.
fn lock_path(home: &Path) -> Result<PathBuf> {
    let dir = index_dir(home);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating index dir {}", dir.display()))?;
    // `canonicalize` needs the path to exist, which it now does. Falling back
    // to the raw path keeps a weird filesystem working, just without the
    // aliasing protection.
    let dir = dir.canonicalize().unwrap_or(dir);
    Ok(dir.join(LOCK_FILE))
}

/// Take the index write lock for `home`, blocking until it is free.
///
/// `what` names the holder for anyone who has to wait ("reindex", "workroom
/// add"), and is written into the lock file. Pass what a *person* would
/// recognize: a curator reading `indice index` in a wait message will go
/// looking for a shell that does not exist if the real holder is the server.
///
/// If the lock is already held, that is reported through `progress` *and* at
/// WARN before blocking — the same try-then-report-then-block shape the
/// server's job queue already uses. WARN rather than INFO because an
/// interactive `index`/`reindex` sets the default filter to `warn` while its
/// progress bar is up, and `progress.phase` is a no-op until `begin` has been
/// called, so an INFO line would leave a waiting curator with no output at all.
///
/// Re-entrant: taking it again on the same thread succeeds immediately.
pub(crate) fn lock_index(
    home: &Path,
    what: &str,
    progress: &dyn IndexProgress,
) -> Result<IndexLock> {
    let path = lock_path(home)?;

    // Already ours on this thread: take another token without touching the
    // file, since flock would conflict with our own open description. The
    // holder line is left as the outermost acquisition wrote it, which is the
    // one a waiter wants to hear about.
    let nested = HELD.with(|h| {
        h.borrow_mut()
            .get_mut(&path)
            .map(|held| held.depth += 1)
            .is_some()
    });
    if nested {
        return Ok(IndexLock {
            path: Some(path),
            _not_send: PhantomData,
        });
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
            tracing::warn!("the search index is locked by {holder}; waiting");
            file.lock()
                .with_context(|| format!("waiting for the index lock {}", path.display()))?;
        }
        // Advisory locking is not available here — some NFS/SMB mounts, and
        // some container overlay and FUSE setups. Degrade to the old
        // behaviour rather than refusing to index at all: a home that worked
        // yesterday must keep working, and this is what the module docs
        // promise. Loud, because the protection is genuinely absent.
        Err(std::fs::TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::Unsupported => {
            tracing::warn!(
                "this filesystem does not support locking ({e}), so {} cannot be \
                 serialized against other indice processes; do not run two at once \
                 on {}",
                what,
                home.display()
            );
            return Ok(IndexLock {
                path: None,
                _not_send: PhantomData,
            });
        }
        Err(e) => {
            return Err(e).with_context(|| format!("locking the index at {}", path.display()))
        }
    }

    // Held. Record who we are, in place — see the module docs on why this one
    // write is not atomic.
    write_holder(&mut file, what);

    HELD.with(|h| h.borrow_mut().insert(path.clone(), Held { file, depth: 1 }));
    Ok(IndexLock {
        path: Some(path),
        _not_send: PhantomData,
    })
}

/// Describe the current holder for a waiter's message, rendering how long it
/// has been running rather than making the reader subtract a UTC timestamp.
///
/// Best-effort throughout: a lock taken by an older version, or interrupted
/// before it wrote its line, has no holder recorded. On Windows the read can
/// fail outright, because `LockFileEx` locks are mandatory rather than advisory
/// and a second handle cannot read the locked region — so the message degrades
/// to "another operation" there. The `flock` is what provides exclusion; this
/// text only makes waiting legible.
fn read_holder(file: &mut File) -> Option<String> {
    let mut s = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_to_string(&mut s).ok()?;
    let line = s.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let (what, since) = line.split_once(HOLDER_SEP)?;
    let started = chrono::DateTime::parse_from_rfc3339(since.trim()).ok()?;
    let ago = chrono::Utc::now().signed_duration_since(started);
    Some(format!("{what} (started {} ago)", human_duration(ago)))
}

/// A coarse "7m", "3h", "2d" — enough to tell a stuck job from a working one.
fn human_duration(d: chrono::TimeDelta) -> String {
    let secs = d.num_seconds().max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// Separates the holder description from its start time, so `read_holder` can
/// render the age without re-parsing free text.
const HOLDER_SEP: &str = " · started ";

/// Stamp who we are and when we started into the lock file.
///
/// Best-effort: the lock is held by the `flock`, not by this text, so failing
/// to write it costs a helpful message and nothing else.
fn write_holder(file: &mut File, what: &str) {
    let line = format!(
        "{what} (pid {}){HOLDER_SEP}{}",
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

    fn depth(home: &Path) -> u32 {
        let p = lock_path(home).unwrap();
        HELD.with(|h| h.borrow().get(&p).map(|x| x.depth).unwrap_or(0))
    }

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

    /// Without re-entrancy this deadlocks rather than failing, so a regression
    /// here hangs the suite instead of reporting.
    #[test]
    fn taking_it_again_on_the_same_thread_succeeds() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let outer = lock_index(home, "index", no_progress()).unwrap();
        {
            let _inner = lock_index(home, "index", no_progress()).unwrap();
            assert_eq!(depth(home), 2);
        }
        assert_eq!(
            depth(home),
            1,
            "dropping the inner guard hands the lock back, it does not release it"
        );
        drop(outer);
        assert_eq!(depth(home), 0, "the last drop clears the entry");
    }

    /// Dropping the *outer* guard first must not release the lock while a
    /// nested guard is still alive.
    ///
    /// This is why the `File` lives in the thread-local rather than in the
    /// guard. When the guard owned it, this sequence released the flock while
    /// the count still said "held", so the next acquisition on the thread
    /// returned a guard backed by nothing and an ingest ran with no exclusion
    /// at all — silently. Reachable with `drop(outer)`, or simply by holding
    /// guards in a struct or `Vec` that drops in declaration order.
    #[test]
    fn dropping_the_outer_guard_first_keeps_the_lock_held() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        let outer = lock_index(&home, "index", no_progress()).unwrap();
        let inner = lock_index(&home, "index", no_progress()).unwrap();
        drop(outer);
        assert_eq!(depth(&home), 1, "still held by the nested guard");

        // The real check: another thread must still be excluded.
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
            "the flock must still be held after the outer guard dropped"
        );
        drop(inner);
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("released once the last guard goes");
        h.join().unwrap();
        assert_eq!(depth(&home), 0);
    }

    /// Two spellings of one home must be the same lock, or the nested check
    /// misses and the process blocks against its own open file description.
    #[test]
    fn a_differently_spelled_home_is_the_same_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let plain = tmp.path().to_path_buf();
        let trailing = PathBuf::from(format!("{}/", plain.display()));
        let indirect = plain.join("sub").join("..");
        std::fs::create_dir_all(plain.join("sub")).unwrap();

        let _outer = lock_index(&plain, "index", no_progress()).unwrap();
        // Each of these deadlocks if it is treated as a different lock.
        let _a = lock_index(&trailing, "index", no_progress()).unwrap();
        let _b = lock_index(&indirect, "index", no_progress()).unwrap();
        assert_eq!(depth(&plain), 3, "all three are the same lock");
    }

    #[test]
    fn two_homes_do_not_contend() {
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        let _ga = lock_index(a.path(), "index", no_progress()).unwrap();
        let _gb = lock_index(b.path(), "index", no_progress()).unwrap();
    }

    /// The holder line is what a waiter reports, so it has to round-trip.
    ///
    /// Read from the *holding* file description on purpose. On Windows
    /// `LockFileEx` locks are mandatory, so a second handle cannot read the
    /// locked region — reading through a fresh `File::open` here would pass on
    /// Unix and panic on Windows, which is a shipped release target that CI
    /// does not exercise. The degraded Windows behaviour (a waiter says
    /// "another operation") is documented on `read_holder`.
    #[test]
    fn the_holder_is_recorded_for_a_waiter_to_report() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let _g = lock_index(home, "reindex", no_progress()).unwrap();
        let path = lock_path(home).unwrap();
        let holder = HELD
            .with(|h| {
                let mut b = h.borrow_mut();
                read_holder(&mut b.get_mut(&path).unwrap().file)
            })
            .expect("a holder line");
        assert!(
            holder.starts_with("reindex (pid ") && holder.contains("(started "),
            "unexpected holder line: {holder}"
        );
    }

    #[test]
    fn a_holder_line_without_a_timestamp_is_not_an_error() {
        // A lock taken by an older version, or interrupted before it wrote its
        // line: the waiter falls back to a generic description rather than
        // failing to acquire.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("stray.lock");
        std::fs::write(&p, b"garbage with no separator").unwrap();
        let mut f = File::open(&p).unwrap();
        assert!(read_holder(&mut f).is_none());
    }

    #[test]
    fn the_lock_file_survives_release_so_the_inode_is_stable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let path = lock_path(home).unwrap();
        let inode = {
            let _g = lock_index(home, "index", no_progress()).unwrap();
            file_id(&path)
        };
        assert!(path.exists(), "releasing must not unlink it");
        let _g = lock_index(home, "index", no_progress()).unwrap();
        assert_eq!(
            inode,
            file_id(&path),
            "re-locking must find the same inode, or two holders could coexist"
        );
    }

    #[test]
    fn human_duration_reads_at_a_glance() {
        let s = |n| human_duration(chrono::TimeDelta::seconds(n));
        assert_eq!(s(5), "5s");
        assert_eq!(s(420), "7m");
        assert_eq!(s(7200), "2h");
        assert_eq!(s(200_000), "2d");
        assert_eq!(s(-5), "0s", "a clock skew must not print nonsense");
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
