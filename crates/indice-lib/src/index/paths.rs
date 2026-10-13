//! Path derivation and filesystem helpers shared across the index module:
//! home-relative directory layout, filename sanitization, and the atomic WACZ
//! downloader. Depended on by every other submodule; depends on nothing here.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Download a WACZ from a (presigned) URL to `dest`, atomically: stream into a
/// sibling `.part` file, then rename into place. Returns bytes written. Shared
/// by the `browsertrix` CLI import and the server's management import.
pub fn download_wacz(url: &str, dest: &Path) -> Result<u64> {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicU64, Ordering};

    // Unique temp name so two concurrent downloads to the same dest (e.g. two
    // admins importing the same crawl at once) can't corrupt each other's
    // partial file; the final rename is atomic, so either complete copy wins.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(format!(
        ".part.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = PathBuf::from(tmp);

    // Unrestricted on purpose. The only caller is a Browsertrix import, whose
    // presigned URL comes from a host the operator configured in the
    // environment and never from a request, so there is no curator-chosen
    // address here to guard against. See `http_range::FetchPolicy`.
    let mut reader =
        crate::http_range::get_reader(url, crate::http_range::FetchPolicy::Unrestricted)
            .with_context(|| format!("fetching {url}"))?;
    let mut file =
        std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("reading {url}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .with_context(|| format!("writing {}", tmp.display()))?;
        written += n as u64;
    }
    file.sync_all().ok();
    drop(file);
    std::fs::rename(&tmp, dest).with_context(|| format!("finalizing {}", dest.display()))?;
    Ok(written)
}

/// Sanitize a string into a single safe path component: keep `[A-Za-z0-9._-]`,
/// map everything else (including separators) to `_`, and neutralize `.`/`..`
/// so a hostile id can't traverse out of its directory.
pub fn safe_component(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    match mapped.as_str() {
        "" => "_".to_string(),
        "." => "_".to_string(),
        ".." => "__".to_string(),
        _ => mapped,
    }
}

/// A safe `.wacz` filename from a resource name (falling back to `fallback`
/// when blank), sanitized via [`safe_component`] with the extension ensured.
pub fn safe_wacz_filename(name: &str, fallback: &str) -> String {
    let base = if name.trim().is_empty() {
        fallback
    } else {
        name.trim()
    };
    let mut out = safe_component(base);
    if !out.to_ascii_lowercase().ends_with(".wacz") {
        out.push_str(".wacz");
    }
    out
}

/// Paths derived from a indice home directory.
pub fn index_dir(home: &Path) -> PathBuf {
    home.join("index")
}

/// Whether `home` has an index directory at all.
///
/// The cheap "is this an archive?" test, for operations that should do nothing
/// (or fail) rather than create one. Unlike `full_text/meta.json`, this is safe
/// to check *outside* the index lock: a rebuild's swap renames directories
/// **inside** `index/` and never removes `index/` itself, so there is no window
/// where this is momentarily false for an initialized archive.
pub(crate) fn index_initialized(home: &Path) -> bool {
    index_dir(home).is_dir()
}

/// The 4-digit year prefix of an ISO-8601-ish date string (`2022-…` → `2022`),
/// or `None` if the first four characters aren't all digits. Used to turn a
/// datapackage `created` / a Browsertrix date range into a coverage-year for the
/// finding aid. Panic-safe on short/multibyte input.
pub fn year_prefix(s: &str) -> Option<String> {
    s.get(..4)
        .filter(|y| y.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
}
pub fn archive_dir(home: &Path) -> PathBuf {
    home.join("archive")
}

/// Bytes on disk under `<home>/archive/`, the figure `uploads.max_total_gb`
/// limits.
///
/// Measured from the directory, not from the manifest. The manifest records
/// only what indexed successfully, and a failed ingest leaves its file behind:
/// `place_local_wacz` files an upload into the archive before it is parsed, so
/// a run of uploads that are not really WACZs would each leave a file here
/// while the manifest, and a manifest-derived total, stayed at zero. Walking
/// the directory counts what is actually using the disk, including a
/// Browsertrix download still being written as a `.part` file, which is how
/// concurrent downloads see each other's progress.
///
/// Symlinks are not followed. One pointing at a large tree elsewhere would
/// otherwise count that tree against the archive, and one pointing at an
/// ancestor would never finish.
///
/// A missing archive directory is zero, not an error: a fresh home has none.
pub fn archive_bytes(home: &Path) -> std::io::Result<u64> {
    fn walk(dir: &Path) -> std::io::Result<u64> {
        let mut total = 0u64;
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            // `DirEntry::file_type` does not follow symlinks.
            let kind = entry.file_type()?;
            if kind.is_dir() {
                total = total.saturating_add(walk(&entry.path())?);
            } else if kind.is_file() {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
        Ok(total)
    }
    match walk(&archive_dir(home)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_component_neutralizes_traversal_and_separators() {
        // A hostile/compromised Browsertrix item id must not escape its subdir.
        assert_eq!(safe_component(".."), "__");
        assert_eq!(safe_component("."), "_");
        // Embedded separators are mapped, leaving a single contained component.
        assert_eq!(safe_component("../../etc"), ".._.._etc");
        assert_eq!(safe_component("a/b"), "a_b");
        assert_eq!(safe_component(""), "_");
        // A normal id is untouched.
        assert_eq!(safe_component("manual-20250101-abc"), "manual-20250101-abc");
    }
    #[test]
    fn safe_wacz_filename_sanitizes_and_ensures_extension() {
        assert_eq!(safe_wacz_filename("my crawl.wacz", "id1"), "my_crawl.wacz");
        // Path separators and other unsafe chars become '_'; extension appended.
        assert_eq!(
            safe_wacz_filename("../etc/passwd", "id1"),
            ".._etc_passwd.wacz"
        );
        assert_eq!(safe_wacz_filename("plain", "id1"), "plain.wacz");
        // Empty name falls back to the item id.
        assert_eq!(safe_wacz_filename("   ", "abc123"), "abc123.wacz");
        // Already-correct names are preserved (case-insensitive extension check).
        assert_eq!(safe_wacz_filename("a-b_c.WACZ", "id1"), "a-b_c.WACZ");
    }
    #[test]
    fn year_prefix_extracts_or_rejects() {
        assert_eq!(year_prefix("2022-01-02T00:00:00Z").as_deref(), Some("2022"));
        assert_eq!(year_prefix("2022").as_deref(), Some("2022"));
        assert_eq!(year_prefix("not-a-date"), None);
        assert_eq!(year_prefix("22"), None); // too short, no panic
        assert_eq!(year_prefix(""), None);
    }
}

#[cfg(test)]
mod archive_bytes_tests {
    use super::*;

    #[test]
    fn counts_every_file_including_ones_the_manifest_never_recorded() {
        // The bypass this measurement exists to close: a failed ingest leaves its
        // file in archive/ with no manifest entry, so a manifest-derived total
        // never saw it. Here nothing is in a manifest at all.
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(
            archive_bytes(tmp.path()).unwrap(),
            0,
            "a fresh home has none"
        );

        let slug = archive_dir(tmp.path()).join("junk");
        std::fs::create_dir_all(slug.join("item")).unwrap();
        std::fs::write(slug.join("x.wacz"), vec![0u8; 300]).unwrap();
        std::fs::write(slug.join("x-2.wacz"), vec![0u8; 200]).unwrap();
        // A download in progress, which concurrent checks need to see.
        std::fs::write(slug.join("item").join("y.wacz.part.1.0"), vec![0u8; 50]).unwrap();
        assert_eq!(archive_bytes(tmp.path()).unwrap(), 550);
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_a_symlink_out_of_the_archive() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("big"), vec![0u8; 10_000]).unwrap();
        let archive = archive_dir(tmp.path());
        std::fs::create_dir_all(&archive).unwrap();
        std::os::unix::fs::symlink(&elsewhere, archive.join("link")).unwrap();
        // And one back at an ancestor, which a following walk would never finish.
        std::os::unix::fs::symlink(tmp.path(), archive.join("loop")).unwrap();
        assert_eq!(archive_bytes(tmp.path()).unwrap(), 0);
    }
}
