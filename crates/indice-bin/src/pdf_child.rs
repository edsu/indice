//! PDF text extraction in a child process, so one PDF that needs gigabytes
//! costs its own text rather than the server (#251).
//!
//! The binary re-runs itself as `indice extract-pdf`: the PDF on stdin, its
//! text on stdout. The child caps its own memory on Linux, and the parent
//! ([`ChildExtractor`]) kills it at a timeout everywhere; a child that is
//! stopped, or dies,
//! leaves the PDF to be indexed by its file name. This lives in the binary
//! because it is process handling, which `indice-lib` leaves to its caller
//! (see [`indice_lib::pdf`]).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{Context, Result};
use indice_lib::pdf::{Extracted, PdfExtractor, PdfLimits};

/// The hidden subcommand the child runs.
pub const SUBCOMMAND: &str = "extract-pdf";

/// The child's exit status for a PDF with no text, or not a PDF: anything
/// but this and success means it was stopped or crashed.
const NOTHING: i32 = 3;

/// The child: cap its own memory at `memory` bytes (Linux), read a PDF from
/// stdin, write at most `max_text` bytes of its text to stdout, and exit `0`,
/// or [`NOTHING`] when there is none.
///
/// Runs before the async runtime starts, so the process is one thread with
/// one malloc arena, and the memory limit measures the extraction rather than
/// a runtime's reservations. The child sets the limit on itself rather than
/// the parent setting it between fork and exec: that needs `pre_exec`, which
/// makes std fork the whole server, every mapping of it, for each PDF, where
/// otherwise it can use `posix_spawn`.
pub fn serve(max_text: usize, memory: Option<u64>) -> Result<()> {
    #[cfg(target_os = "linux")]
    if let Some(bytes) = memory {
        limit_memory(bytes).context("setting the memory limit")?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = memory;
    // `pdf-extract` and `lopdf` can print to stdout, and here stdout carries
    // the text, so it is moved aside first: the text goes to a duplicate of
    // the pipe, and the real stdout is gagged, so a stray print goes nowhere
    // instead of into the index.
    let mut out = std::fs::File::from(stdout_dup().context("duplicating stdout")?);
    let _quiet = gag::Gag::stdout().ok();
    let mut pdf = Vec::new();
    std::io::stdin().lock().read_to_end(&mut pdf)?;
    match indice_lib::pdf::extract_capped(&pdf, max_text) {
        Some(text) => {
            out.write_all(text.as_bytes())?;
            out.flush()?;
            Ok(())
        }
        None => std::process::exit(NOTHING),
    }
}

#[cfg(unix)]
fn stdout_dup() -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsFd;
    std::io::stdout().as_fd().try_clone_to_owned()
}

#[cfg(windows)]
fn stdout_dup() -> std::io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::AsHandle;
    std::io::stdout().as_handle().try_clone_to_owned()
}

/// The parent: extract each PDF by running `exe extract-pdf`, one at a time.
///
/// One at a time, across every ingest in the process, so the most PDF
/// extraction costs at once is one memory limit whatever the indexing
/// concurrency: four children at 1 GiB each would not fit a 2.5 GB container.
/// So a PDF that runs to its timeout holds up every other PDF until then.
pub struct ChildExtractor {
    exe: PathBuf,
    turn: Mutex<()>,
}

impl ChildExtractor {
    pub fn new(exe: PathBuf) -> Self {
        Self {
            exe,
            turn: Mutex::new(()),
        }
    }
}

impl PdfExtractor for ChildExtractor {
    fn extract(&self, bytes: &[u8], max_text: usize, limits: &PdfLimits) -> Extracted {
        let _turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        run(&self.exe, bytes, max_text, limits)
            .unwrap_or_else(|e| Extracted::Stopped(format!("{e:#}")))
    }
}

/// What extracts PDFs when there is no executable to run as a child: every
/// PDF is indexed by its file name, rather than extracted in the server
/// with no limits. The reason is for the log.
pub struct Unavailable(pub String);

impl PdfExtractor for Unavailable {
    fn extract(&self, _bytes: &[u8], _max_text: usize, _limits: &PdfLimits) -> Extracted {
        Extracted::Stopped(self.0.clone())
    }
}

/// The path to re-run for a child: on Linux, `/proc/self/exe`, which is the
/// image this process is running even after an upgrade has replaced or
/// removed the file it started from; elsewhere the path it started from.
pub fn own_exe() -> std::io::Result<PathBuf> {
    if cfg!(target_os = "linux") {
        Ok(PathBuf::from("/proc/self/exe"))
    } else {
        std::env::current_exe()
    }
}

/// Run one child to completion, or until its timeout.
fn run(
    exe: &std::path::Path,
    bytes: &[u8],
    max_text: usize,
    limits: &PdfLimits,
) -> Result<Extracted> {
    let mut cmd = Command::new(exe);
    cmd.arg(SUBCOMMAND)
        .arg("--max-text")
        .arg(max_text.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Its panics and warnings are about a PDF we already handle.
        .stderr(Stdio::null());
    if let Some(memory) = limits.memory {
        cmd.arg("--memory").arg(memory.to_string());
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    let mut stdin = child.stdin.take().context("child stdin")?;
    let stdout = child.stdout.take().context("child stdout")?;
    let child = Mutex::new(child);
    let timed_out = std::sync::atomic::AtomicBool::new(false);
    let (done, finished) = std::sync::mpsc::channel::<()>();

    let mut text = Vec::new();
    std::thread::scope(|s| {
        // Written from its own thread, since the child reads all of its input
        // before writing any output, and a full pipe would block either side.
        // A child that dies early makes this fail, which its exit status says
        // better.
        s.spawn(move || {
            let _ = stdin.write_all(bytes);
        });
        if let Some(timeout) = limits.timeout {
            let (child, timed_out) = (&child, &timed_out);
            s.spawn(move || {
                if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                    finished.recv_timeout(timeout)
                {
                    timed_out.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = child.lock().unwrap_or_else(|e| e.into_inner()).kill();
                }
            });
        }
        // Ends when the child exits or is killed. The child writes no more
        // than the limit, and this reads no more, whatever it does.
        let _ = stdout.take(max_text as u64).read_to_end(&mut text);
        drop(done);
    });
    let status = child
        .into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .wait()?;

    // The status first: the watchdog can fire just after a child has written
    // its text and exited, and that text is good.
    if status.success() {
        let text = String::from_utf8_lossy(&text).trim().to_string();
        return Ok(if text.is_empty() {
            Extracted::Nothing
        } else {
            Extracted::Text(text)
        });
    }
    if status.code() == Some(NOTHING) {
        return Ok(Extracted::Nothing);
    }
    if timed_out.into_inner() {
        let timeout = limits.timeout.unwrap_or_default();
        return Ok(Extracted::Stopped(format!("took longer than {timeout:?}")));
    }
    // Past the memory limit an allocation fails, and Rust aborts rather than
    // unwind; the OOM killer sends SIGKILL. Either way the child is gone, and
    // all that is known is how.
    Ok(Extracted::Stopped(match limits.memory {
        Some(memory) if cfg!(target_os = "linux") => format!(
            "exceeded its {} MiB memory limit or crashed ({status})",
            memory / (1024 * 1024)
        ),
        _ => format!("crashed ({status})"),
    }))
}

/// Cap the address space of the process calling it. `RLIMIT_AS` rather than a
/// resident-set limit because Linux enforces only this one: an allocation past
/// it fails. It counts memory reserved as well as used, which for a single
/// thread allocating as it parses is close to what it uses.
#[cfg(target_os = "linux")]
fn limit_memory(bytes: u64) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: bytes as libc::rlim_t,
        rlim_max: bytes as libc::rlim_t,
    };
    // SAFETY: `setrlimit` reads the struct it is given and nothing else.
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A stand-in child: a shell script, so each way a child can end is
    /// tested without a PDF that takes that long or that much.
    fn child(dir: &tempfile::TempDir, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("child");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    const LIMITS: PdfLimits = PdfLimits {
        memory: None,
        timeout: Some(Duration::from_secs(30)),
    };

    #[test]
    fn each_way_a_child_ends_is_told_apart() {
        let dir = tempfile::TempDir::new().unwrap();
        let ends = |script: &str, limits: &PdfLimits| {
            run(&child(&dir, script), b"%PDF-", 1024, limits).unwrap()
        };
        assert_eq!(
            ends("cat >/dev/null; printf ' some text '", &LIMITS),
            Extracted::Text("some text".into())
        );
        assert_eq!(ends("cat >/dev/null; exit 3", &LIMITS), Extracted::Nothing);
        assert_eq!(ends("cat >/dev/null", &LIMITS), Extracted::Nothing);
        assert!(matches!(
            ends("cat >/dev/null; kill -9 $$", &LIMITS),
            Extracted::Stopped(why) if why.contains("crashed")
        ));
        // A child that never finishes is killed at its timeout. `exec`, so the
        // script is the process killed: a grandchild would keep the output
        // pipe open, which the real child, starting no processes, never has.
        let quick = PdfLimits {
            timeout: Some(Duration::from_millis(200)),
            ..LIMITS
        };
        let started = std::time::Instant::now();
        assert_eq!(
            ends("exec sleep 30", &quick),
            Extracted::Stopped("took longer than 200ms".into())
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_child_that_cannot_start_stops_the_pdf_rather_than_the_ingest() {
        let extractor = ChildExtractor::new(PathBuf::from("/nonexistent/indice"));
        assert!(matches!(
            extractor.extract(b"%PDF-", 1024, &LIMITS),
            Extracted::Stopped(why) if why.contains("starting")
        ));
    }
}
