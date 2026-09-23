//! Guards on the Rust primer under `site/src/content/docs/primer/`.
//!
//! The primer teaches Rust by pointing at this codebase, so its references have
//! to stay true. The previous version cited code by `file.rs:line`, and by the
//! time anyone looked, 42 of its 126 citations pointed at files that no longer
//! existed and the rest had drifted onto unrelated lines — `collections.rs:92`
//! was cited as a type and was a blank line. Nothing failed, because nothing
//! was checking.
//!
//! These tests are the checking. They cannot tell whether an explanation is
//! still *true*, but they catch the two ways it rots silently.

use std::path::{Path, PathBuf};

fn primer_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../site/src/content/docs/primer")
        .canonicalize()
        .expect("the primer directory should exist")
}

fn pages() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(primer_dir()).expect("readable primer dir") {
        let path = entry.expect("readable entry").path();
        if path.extension().is_some_and(|e| e == "md") {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            out.push((name, std::fs::read_to_string(&path).expect("readable page")));
        }
    }
    assert!(!out.is_empty(), "no primer pages found — did they move?");
    out
}

/// No `file.rs:123` citations. This is the format that rotted.
///
/// A line number is invalidated by any edit above it, and it fails *silently*:
/// the reference still looks plausible and still resolves to some line, just
/// the wrong one. Cite `module::Item` instead — stable across edits, greppable,
/// and a rename is findable rather than invisible.
#[test]
fn the_primer_does_not_cite_line_numbers() {
    let mut offenders = Vec::new();
    for (name, body) in pages() {
        for (i, line) in body.lines().enumerate() {
            // `foo.rs:123`, the shape that rots. A bare `foo.rs` is fine.
            if let Some(hit) = find_line_citation(line) {
                offenders.push(format!("{name}:{}: {hit}", i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "cite `module::Item`, not a line number — these rot silently:\n  {}",
        offenders.join("\n  ")
    );
}

fn find_line_citation(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(rel) = line[from..].find(".rs:") {
        let at = from + rel;
        let after = &line[at + 4..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            // Walk back over the filename for a readable message.
            let start = bytes[..at]
                .iter()
                .rposition(|b| !(b.is_ascii_alphanumeric() || *b == b'_' || *b == b'/'))
                .map(|i| i + 1)
                .unwrap_or(0);
            return Some(format!("{}:{}", &line[start..at + 3], digits));
        }
        from = at + 4;
    }
    None
}

/// Every repository path the primer mentions has to exist.
///
/// The cheap half of keeping it honest: a chapter that walks through
/// `crates/indice-lib/src/index/ingest/` should fail loudly when that directory
/// is renamed, rather than sending a reader somewhere that is not there.
#[test]
fn every_path_the_primer_mentions_exists() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut missing = Vec::new();
    for (name, body) in pages() {
        for token in body.split(|c: char| !(c.is_alphanumeric() || "._/-".contains(c))) {
            if !token.starts_with("crates/") {
                continue;
            }
            let token = token.trim_end_matches(['.', ',', '/']);
            if token.is_empty() || !repo.join(token).exists() {
                missing.push(format!("{name}: {token}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "the primer points at paths that do not exist:\n  {}",
        missing.join("\n  ")
    );
}
