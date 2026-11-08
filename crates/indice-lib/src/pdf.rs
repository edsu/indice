//! PDF text extraction for indexing archived PDF documents.
//!
//! How much memory extracting one PDF takes cannot be predicted from its
//! size: an 8 MB PDF whose page draws a chart point by point peaked at 2.2 GB
//! in `pdf-extract`, and a 4 MB one with a large structure tree at 1.3 GB,
//! while 30 MB PDFs took 100 MB (#251). So extraction is not bounded by a size
//! cap but run somewhere it can be stopped: the `indice` binary installs a
//! [`PdfExtractor`] that runs each PDF in a child process with a memory limit
//! and a timeout ([`set_extractor`]). The library itself does no process
//! handling, and without an installed extractor (as under `cargo test`, where
//! the current executable is the test binary) it extracts in process.

use std::sync::OnceLock;
use std::time::Duration;

/// What one PDF's extraction may cost, from `config.yaml` (see
/// [`crate::config::Config::pdf_limits`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfLimits {
    /// The most memory the extraction may use, in bytes (`index.pdf_memory_mb`);
    /// `None` is no limit. Only enforced where the platform can (Linux).
    pub memory: Option<u64>,
    /// How long it may take (`index.pdf_timeout_secs`); `None` is no limit.
    pub timeout: Option<Duration>,
}

/// The outcome of extracting one PDF.
#[derive(Debug, PartialEq, Eq)]
pub enum Extracted {
    /// Its text, trimmed and non-empty.
    Text(String),
    /// Not a PDF that could be read, or one with no text: the record is
    /// skipped, as any unreadable page is.
    Nothing,
    /// Stopped at a limit, or the extraction died: the PDF is indexed by its
    /// file name, as one too large to read is. The reason is for the log.
    Stopped(String),
}

/// How the indexer turns PDF bytes into at most `max_text` bytes of text
/// (`index.max_extract_mb`).
pub trait PdfExtractor: Send + Sync {
    fn extract(&self, bytes: &[u8], max_text: usize, limits: &PdfLimits) -> Extracted;
}

/// Extraction in this process, with no limits: what the library uses unless
/// the binary installs something else, and what the binary's child process
/// runs.
pub struct InProcess;

impl PdfExtractor for InProcess {
    fn extract(&self, bytes: &[u8], max_text: usize, _limits: &PdfLimits) -> Extracted {
        match extract_capped(bytes, max_text) {
            Some(text) => Extracted::Text(text),
            None => Extracted::Nothing,
        }
    }
}

/// A PDF's text, cut on a character boundary at `max_text` bytes; `None`
/// for no text. What [`InProcess`] returns and what the binary's child
/// writes, so the two cut the same way.
pub fn extract_capped(bytes: &[u8], max_text: usize) -> Option<String> {
    let mut text = extract_pdf_text(bytes)?;
    let end = crate::search::utf8_prefix(text.as_bytes(), max_text).len();
    text.truncate(end);
    Some(text)
}

static EXTRACTOR: OnceLock<Box<dyn PdfExtractor>> = OnceLock::new();

/// Install the extractor every ingest in this process uses. Once, at startup:
/// a second call is refused, and returns the extractor it was given.
///
/// Process-wide rather than an `Ingest` setter because ingests are built in
/// many places (each CLI command, each server job, each import), and a setter
/// forgotten at one of them would fall back to unlimited in-process extraction
/// without anyone noticing.
pub fn set_extractor(e: Box<dyn PdfExtractor>) -> Result<(), Box<dyn PdfExtractor>> {
    EXTRACTOR.set(e)
}

/// Extract one PDF's text with the installed extractor, or in process.
pub(crate) fn extract(bytes: &[u8], max_text: usize, limits: &PdfLimits) -> Extracted {
    match EXTRACTOR.get() {
        Some(e) => e.extract(bytes, max_text, limits),
        None => InProcess.extract(bytes, max_text, limits),
    }
}

/// Extract plain text from PDF bytes.
///
/// Returns `None` if the bytes are not a parseable PDF or yield no text.
/// `pdf-extract` (via `lopdf`) can *panic* on some malformed input rather than
/// return an error, so extraction runs inside `catch_unwind` to ensure one bad
/// PDF never aborts a whole index run.
pub fn extract_pdf_text(bytes: &[u8]) -> Option<String> {
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(bytes)
    }));
    match attempt {
        Ok(Ok(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Ok(Err(_)) => None, // parse/extraction error
        Err(_) => None,     // panic inside pdf-extract/lopdf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_pdf_bytes_return_none() {
        assert!(extract_pdf_text(b"this is not a pdf").is_none());
    }

    #[test]
    fn empty_bytes_return_none() {
        assert!(extract_pdf_text(b"").is_none());
    }
}
