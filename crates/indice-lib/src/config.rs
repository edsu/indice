//! Home-level operator configuration (`<home>/config.yaml`).
//!
//! This is indice's first optional, hand-editable operator config — committable
//! alongside the archive, like the finding aids (and using the same YAML). It is
//! deliberately small and forgiving: the file is optional, every field has a
//! default, and unknown/missing keys are ignored, so it can grow (CSS
//! override, theming, …) without breaking older or newer homes.
//!
//! It carries index tuning — the stored-body cap that trades snippet depth for
//! on-disk size (the "frugality" knob), see the size/scale model in DESIGN.md —
//! the limits on what the browser write surface may add to the archive, and
//! the site's name, tagline and banner.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The config file name inside a indice home directory.
const CONFIG_FILE: &str = "config.yaml";

/// Default bytes of page body text stored per document for snippets (the full
/// body is always indexed for search). ~16 KiB keeps generous snippets; lower
/// trades snippet depth for a smaller doc store at scale.
pub const DEFAULT_STORED_BODY_CAP_BYTES: usize = 16 * 1024;

/// Default Tantivy indexing buffer budget (bytes) — the RAM the writer may use
/// before flushing a segment. Split across indexing threads by Tantivy.
pub const DEFAULT_WRITER_HEAP_BYTES: usize = 50 * 1024 * 1024;

/// Floor for the writer heap; Tantivy errors on a too-small budget, so a tiny
/// configured value is clamped up to something it accepts.
const MIN_WRITER_HEAP_BYTES: usize = 15 * 1024 * 1024;

/// Operator configuration for a indice home. Every section and field is
/// optional; an absent file (or key) means "use the defaults".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Index-tuning knobs (footprint vs. snippet richness).
    pub index: IndexConfig,
    /// How much a curator may add to the archive through the browser.
    pub uploads: UploadsConfig,
    /// What the archive calls itself, in the header and on the homepage.
    pub site: SiteConfig,
    // Future sections (CSS override, theming, …) slot in here; older/newer
    // homes ignore keys they don't know.
}

/// The archive's own words for the pages indice serves. Every field is
/// optional; [`crate::views::Site`] supplies the defaults and renders the
/// banner. Read once at startup, so a change takes a restart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteConfig {
    /// The archive's name: the header wordmark, the homepage heading, and the
    /// end of every page title. Defaults to `indice`.
    pub name: Option<String>,
    /// One line under the homepage heading saying what the archive holds.
    pub tagline: Option<String>,
    /// A notice shown under the header on every page, in Markdown, e.g. "This
    /// is a demo". Rendered by [`crate::markdown::render`], which escapes raw
    /// HTML and keeps links to safe schemes.
    pub banner: Option<String>,
    /// How to ask for more than read access, in Markdown, e.g. "Email
    /// [the archivist](mailto:…)". Shown to someone signed in but not on the
    /// roster, under the header and on the page refusing them a workroom page.
    pub request_access: Option<String>,
}

/// Index tuning — currently the stored-body cap (the "frugality" lever).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    /// How much page body text to STORE per document for snippets, in KiB. The
    /// full body is always indexed (search recall is unaffected); this only
    /// bounds the stored copy used to render snippets. `0` stores the full body
    /// (no cap); omit to use the default (16 KiB). Lower = smaller index, but
    /// matches deeper than the cap can't be highlighted. Applied on
    /// `index`/`reindex`; measure with `indice stats`.
    pub stored_body_cap_kb: Option<u64>,

    /// Tantivy indexing buffer budget in MiB — the RAM ceiling / throughput knob
    /// for building the index. Higher = fewer, larger segments (faster bulk
    /// ingest, more RAM); lower caps memory. Omit for the 50 MiB default. Unlike
    /// `stored_body_cap_kb`, `0` here is *not* "unlimited": any value below
    /// Tantivy's ~15 MiB minimum (including 0) is clamped up to it.
    pub writer_heap_mb: Option<u64>,

    /// The most indice inflates of any one WACZ member, in MiB, so a WACZ
    /// whose `pages.jsonl` or CDX expands to gigabytes is refused rather than
    /// read into memory. Omit for the default (1024 MiB); raise it for a
    /// genuine crawl that exceeds it, which the refusal names. `0` is no limit.
    pub max_member_mb: Option<u64>,
}

/// Limits on what the browser write surface may add to the archive.
///
/// Both are unset by default, which means unlimited, because there is no number
/// that is right for everyone: a WACZ of a large site is legitimately several
/// gigabytes, and a cap low enough to protect a small volume would refuse it.
/// An operator exposing indice to curators they do not know should set both.
///
/// They apply to the HTTP surface only. `indice index` on the command line is
/// the operator at the machine, and is not limited.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadsConfig {
    /// The largest single upload, in MiB.
    pub max_mb: Option<u64>,
    /// The most the archive may hold, in GiB, counting the local WACZs the
    /// manifest records. A crawl indexed from a URL is streamed in place and
    /// costs no local disk, so it does not count.
    pub max_total_gb: Option<u64>,
}

impl Config {
    /// The config path within `home`.
    pub fn path(home: &Path) -> PathBuf {
        home.join(CONFIG_FILE)
    }

    /// Load `<home>/config.yaml`, or the defaults if it's absent. Errors only on
    /// a present-but-malformed file (a clear signal beats silently ignoring a
    /// typo'd setting).
    pub fn load(home: &Path) -> Result<Config> {
        let path = Self::path(home);
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_yaml_ng::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Check the `uploads:` section strictly, for the server to call at startup.
    ///
    /// [`Config::load`] is forgiving by design, and every ingest calls it, the
    /// command line's included. The upload limits are the one place where that
    /// forgiveness fails open: a mistyped key is ignored and a key with no value
    /// reads as absent, and both mean *unlimited* on a server the operator
    /// believes is limited. So the section is checked here, where only the
    /// server looks, rather than in `load`, where a mistake in a setting the
    /// command line does not use would stop the command line.
    ///
    /// Refused: a key other than `max_mb` or `max_total_gb`, a key with no
    /// value, and a value of `0`, which someone wrote to forbid uploads and
    /// which reading as "no limit" would quietly invert.
    pub fn check_upload_limits(home: &Path) -> Result<()> {
        let path = Self::path(home);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(());
        };
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        let Some(uploads) = doc.get("uploads") else {
            return Ok(());
        };
        let Some(section) = uploads.as_mapping() else {
            anyhow::bail!("{}: uploads: must be a mapping of limits", path.display());
        };
        for (key, value) in section {
            let key = key.as_str().unwrap_or("?");
            if !matches!(key, "max_mb" | "max_total_gb") {
                anyhow::bail!(
                    "{}: uploads.{key} is not a setting; the upload limits are \
                     uploads.max_mb and uploads.max_total_gb",
                    path.display()
                );
            }
            if value.is_null() {
                anyhow::bail!(
                    "{}: uploads.{key} has no value, which would mean no limit. Give a \
                     size, or remove the key.",
                    path.display()
                );
            }
            if value.as_u64() == Some(0) {
                anyhow::bail!(
                    "{}: uploads.{key} is 0, which would refuse every upload. Omit it for \
                     no limit, or give a size.",
                    path.display()
                );
            }
        }
        Ok(())
    }

    /// The largest single upload in bytes, if limited.
    pub fn max_upload_bytes(&self) -> Option<u64> {
        self.uploads.max_mb.map(|mb| mb.saturating_mul(1024 * 1024))
    }

    /// The most the archive may hold in bytes, if limited.
    pub fn max_archive_bytes(&self) -> Option<u64> {
        self.uploads
            .max_total_gb
            .map(|gb| gb.saturating_mul(1024 * 1024 * 1024))
    }

    /// The stored-body cap in bytes: an explicit KiB value (`0` → unbounded),
    /// else the default.
    pub fn stored_body_cap_bytes(&self) -> usize {
        match self.index.stored_body_cap_kb {
            Some(0) => usize::MAX,
            Some(kb) => (kb as usize).saturating_mul(1024),
            None => DEFAULT_STORED_BODY_CAP_BYTES,
        }
    }

    /// The most to inflate of one WACZ member, in bytes; see
    /// [`IndexConfig::max_member_mb`].
    pub fn max_member_bytes(&self) -> u64 {
        match self.index.max_member_mb {
            Some(0) => u64::MAX,
            Some(mb) => mb.saturating_mul(1024 * 1024),
            None => crate::wacz::DEFAULT_MEMBER_LIMIT,
        }
    }

    /// The Tantivy writer heap in bytes: an explicit MiB value (clamped up to
    /// Tantivy's minimum), else the default.
    pub fn writer_heap_bytes(&self) -> usize {
        match self.index.writer_heap_mb {
            Some(mb) => (mb as usize)
                .saturating_mul(1024 * 1024)
                .max(MIN_WRITER_HEAP_BYTES),
            None => DEFAULT_WRITER_HEAP_BYTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_defaults() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = Config::load(tmp.path()).unwrap();
        assert_eq!(cfg.stored_body_cap_bytes(), DEFAULT_STORED_BODY_CAP_BYTES);
    }

    #[test]
    fn stored_body_cap_kb_resolves_to_bytes() {
        let unset = Config::default();
        assert_eq!(unset.stored_body_cap_bytes(), DEFAULT_STORED_BODY_CAP_BYTES);

        let frugal = Config {
            index: IndexConfig {
                stored_body_cap_kb: Some(4),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(frugal.stored_body_cap_bytes(), 4 * 1024);

        let unbounded = Config {
            index: IndexConfig {
                stored_body_cap_kb: Some(0),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(unbounded.stored_body_cap_bytes(), usize::MAX);
    }

    #[test]
    fn upload_limits_that_would_fail_open_are_refused_at_server_startup() {
        // Each of these reads as "no limit" under the forgiving parse, which on
        // a server the operator believes is limited is the worst way to be wrong.
        let tmp = tempfile::TempDir::new().unwrap();
        for (yaml, why) in [
            ("uploads:\n  max_mb: 0\n", "is 0"),
            ("uploads:\n  max_total_gb: 0\n", "is 0"),
            ("uploads:\n  max_size_mb: 200\n", "not a setting"),
            ("uploads:\n  max_mb:\n", "no value"),
        ] {
            std::fs::write(Config::path(tmp.path()), yaml).unwrap();
            let err = format!("{:#}", Config::check_upload_limits(tmp.path()).unwrap_err());
            assert!(
                err.contains(why),
                "{yaml:?} should be refused ({why}): {err}"
            );
            // And the forgiving load the CLI uses is not affected: a mistake in
            // a setting the command line ignores must not stop the command line.
            assert!(
                Config::load(tmp.path()).is_ok(),
                "{yaml:?} must not break Config::load"
            );
        }

        std::fs::write(
            Config::path(tmp.path()),
            "uploads:\n  max_mb: 200\n  max_total_gb: 20\n",
        )
        .unwrap();
        let c = Config::load(tmp.path()).unwrap();
        assert_eq!(c.max_upload_bytes(), Some(200 * 1024 * 1024));
        assert_eq!(c.max_archive_bytes(), Some(20 * 1024 * 1024 * 1024));

        // Absent means unlimited.
        assert_eq!(Config::default().max_upload_bytes(), None);
        assert_eq!(Config::default().max_archive_bytes(), None);
    }

    #[test]
    fn writer_heap_resolves_and_clamps() {
        assert_eq!(
            Config::default().writer_heap_bytes(),
            DEFAULT_WRITER_HEAP_BYTES
        );
        let big = Config {
            index: IndexConfig {
                writer_heap_mb: Some(256),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(big.writer_heap_bytes(), 256 * 1024 * 1024);
        // A too-small value is clamped up to Tantivy's minimum.
        let tiny = Config {
            index: IndexConfig {
                writer_heap_mb: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(tiny.writer_heap_bytes(), MIN_WRITER_HEAP_BYTES);
    }

    #[test]
    fn loads_from_yaml() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            Config::path(tmp.path()),
            "index:\n  stored_body_cap_kb: 8\n",
        )
        .unwrap();
        let cfg = Config::load(tmp.path()).unwrap();
        assert_eq!(cfg.stored_body_cap_bytes(), 8 * 1024);
    }

    #[test]
    fn partial_config_keeps_defaults() {
        // An empty/unrelated file still yields working defaults.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(Config::path(tmp.path()), "index: {}\n").unwrap();
        let cfg = Config::load(tmp.path()).unwrap();
        assert_eq!(cfg.stored_body_cap_bytes(), DEFAULT_STORED_BODY_CAP_BYTES);
    }
}
