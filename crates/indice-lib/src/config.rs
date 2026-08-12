//! Home-level operator configuration (`<home>/config.yaml`).
//!
//! This is indice's first optional, hand-editable operator config — committable
//! alongside the archive, like the finding aids (and using the same YAML). It is
//! deliberately small and forgiving: the file is optional, every field has a
//! default, and unknown/missing keys are ignored, so it can grow (site name,
//! branding, CSS override, theming, …) without breaking older or newer homes.
//!
//! Today it carries index tuning — the stored-body cap that trades snippet depth
//! for on-disk size (the "frugality" knob). See the size/scale model in
//! DESIGN.md.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The config file name inside a indice home directory.
const CONFIG_FILE: &str = "config.yaml";

/// Default bytes of page body text stored per document for snippets (the full
/// body is always indexed for search). ~16 KiB keeps generous snippets; lower
/// trades snippet depth for a smaller doc store at scale.
pub const DEFAULT_STORED_BODY_CAP_BYTES: usize = 16 * 1024;

/// Operator configuration for a indice home. Every section and field is
/// optional; an absent file (or key) means "use the defaults".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Index-tuning knobs (footprint vs. snippet richness).
    pub index: IndexConfig,
    // Future sections (site name, branding, CSS override, theming, …) slot in
    // here; older/newer homes ignore keys they don't know.
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

    /// The stored-body cap in bytes: an explicit KiB value (`0` → unbounded),
    /// else the default.
    pub fn stored_body_cap_bytes(&self) -> usize {
        match self.index.stored_body_cap_kb {
            Some(0) => usize::MAX,
            Some(kb) => (kb as usize).saturating_mul(1024),
            None => DEFAULT_STORED_BODY_CAP_BYTES,
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
            },
        };
        assert_eq!(frugal.stored_body_cap_bytes(), 4 * 1024);

        let unbounded = Config {
            index: IndexConfig {
                stored_body_cap_kb: Some(0),
            },
        };
        assert_eq!(unbounded.stored_body_cap_bytes(), usize::MAX);
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
