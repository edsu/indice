//! Index size/scale statistics (`IndexStats`) for the scale model.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use super::paths::index_dir;

/// A size/scale snapshot of the search index: total on-disk bytes, a breakdown
/// by Tantivy segment-file type, the live doc count, and the derived
/// bytes-per-doc — the inputs to the scale model (project bytes/doc to 1M / 100M
/// docs). Re-run after any change to compare.
#[derive(Debug, Clone)]
pub struct IndexStats {
    pub docs: u64,
    pub total_bytes: u64,
    /// `(file-type label, bytes)`, largest first. Labels are the Tantivy
    /// segment-file extensions — `store` (stored fields / snippets), `pos`
    /// (term positions), `term`/`idx` (inverted index), `fast` (columnar /
    /// facets), `fieldnorm` — plus `del` (deletes) and `meta` (JSON bookkeeping).
    pub by_type: Vec<(String, u64)>,
}

impl IndexStats {
    /// Bytes per live document (0 for an empty index).
    pub fn bytes_per_doc(&self) -> f64 {
        if self.docs == 0 {
            0.0
        } else {
            self.total_bytes as f64 / self.docs as f64
        }
    }

    /// Projected total bytes at `docs` documents, at today's bytes-per-doc.
    pub fn project(&self, docs: u64) -> u64 {
        (self.bytes_per_doc() * docs as f64) as u64
    }
}

/// Measure the on-disk footprint of the search index for the size/scale model:
/// total bytes, a per-file-type breakdown, and the live doc count. Reads only
/// file sizes + the doc count, so it's cheap to re-run.
pub fn index_stats(home: &Path) -> Result<IndexStats> {
    let full_text = index_dir(home).join("full_text");
    let docs = if full_text.join("meta.json").exists() {
        crate::search::SearchIndex::open_read_only(&full_text)?.num_docs()?
    } else {
        0
    };

    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    let mut total = 0u64;
    if full_text.exists() {
        for entry in std::fs::read_dir(&full_text)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip Tantivy's write-lock file (0 bytes; not part of the footprint).
            if !meta.is_file() || name.ends_with(".lock") {
                continue;
            }
            total += meta.len();
            // Segment files are `<uuid>.<ext>` (store/pos/term/idx/fast/fieldnorm)
            // or `<uuid>.<n>.del`; bookkeeping is `*.json`.
            let label = if name.ends_with(".json") {
                "meta"
            } else {
                Path::new(name.as_ref())
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("other")
            };
            *by.entry(label.to_string()).or_default() += meta.len();
        }
    }
    let mut by_type: Vec<(String, u64)> = by.into_iter().collect();
    by_type.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
    Ok(IndexStats {
        docs,
        total_bytes: total,
        by_type,
    })
}
