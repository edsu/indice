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

/// Default for the most of one record read for its text. Finding a record's
/// text costs many times its size (measured: a 100 MiB HTML page took 2.3 GB
/// to parse, and a 16 MiB PDF 1.2 GB to extract), and real pages are far
/// smaller, so this bounds the worst record without costing real ones.
pub const DEFAULT_EXTRACT_BYTES: usize = 16 * 1024 * 1024;

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
    /// A language model indice may use, if the operator configures one.
    pub llm: Option<LlmConfig>,
    /// How the model drafts a finding aid's description.
    pub describe: Option<DescribeConfig>,
    // Future sections (CSS override, theming, …) slot in here; older/newer
    // homes ignore keys they don't know.
}

/// Which language model to use: any service that speaks the OpenAI chat
/// completions API, hosted or local (Ollama, llama.cpp, vLLM). Named for the
/// model rather than for one use of it, so later uses share it. Its API key,
/// when it needs one, comes from `INDICE_LLM_API_KEY`, never this file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    /// The API's base URL, ending in `/v1`, e.g. `http://localhost:11434/v1`.
    pub endpoint: Option<String>,
    /// The model name the service knows, e.g. `llama3.1:8b`.
    pub model: Option<String>,
    /// How long to wait for an answer, in seconds. Default 120.
    pub timeout_secs: Option<u64>,
    /// What a curator should know about the service before sending it
    /// archived text, in Markdown: who runs it, what it keeps, a link to its
    /// privacy policy. Shown beside the editor's Draft button and printed by
    /// `indice collection draft`.
    pub notice: Option<String>,
}

/// How the model drafts a description. Needs an `llm:` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DescribeConfig {
    /// The most archived text and metadata to send, in characters, so a small
    /// local model's context and a large hosted one's can both be fitted.
    /// Default 24,000.
    pub max_input_chars: Option<usize>,
    /// A prompt to use instead of indice's own, relative to the home.
    pub prompt_file: Option<String>,
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

    /// The most of any one page indice reads for its text, in MiB. HTML and
    /// rendered text past it are not searchable; a PDF larger than it is
    /// indexed by its file name alone, since a PDF can't be read in part.
    /// Omit for the default (16 MiB); `0` is no limit. Each MiB can cost
    /// tens of MiB of memory while it's read, so raise it only with memory
    /// to match.
    pub max_extract_mb: Option<u64>,
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

    /// The model and the drafting settings, if both are configured, checked
    /// strictly: the server calls this at startup and the command line before
    /// it drafts, so a mistake is reported before anything is sent. `None`
    /// when there is no `llm:` section, which is the default and means the
    /// feature is off.
    ///
    /// Refused: `describe:` without `llm:`; `llm:` without an endpoint or a
    /// model; an endpoint that isn't http(s); a `max_input_chars` too small to
    /// hold a page; a
    /// `prompt_file` that can't be read.
    pub fn llm_settings(
        &self,
        home: &Path,
    ) -> Result<Option<(crate::llm::LlmSettings, crate::describe::DescribeSettings)>> {
        let path = Self::path(home);
        let Some(llm) = &self.llm else {
            if self.describe.is_some() {
                anyhow::bail!(
                    "{}: describe: needs an llm: section saying which model to use",
                    path.display()
                );
            }
            return Ok(None);
        };
        let given = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let Some(endpoint) = given(&llm.endpoint) else {
            anyhow::bail!(
                "{}: llm.endpoint is missing; give the service's base URL, e.g. \
                 http://localhost:11434/v1",
                path.display()
            );
        };
        let scheme = endpoint
            .split_once("://")
            .map(|(s, _)| s.to_ascii_lowercase());
        if !matches!(scheme.as_deref(), Some("http" | "https")) {
            anyhow::bail!(
                "{}: llm.endpoint must be an http:// or https:// URL, not {endpoint}",
                path.display()
            );
        }
        let Some(model) = given(&llm.model) else {
            anyhow::bail!(
                "{}: llm.model is missing; give the model name the service knows",
                path.display()
            );
        };
        let timeout = std::time::Duration::from_secs(
            llm.timeout_secs
                .unwrap_or(crate::llm::DEFAULT_TIMEOUT_SECS)
                .max(1),
        );
        let describe = self.describe.clone().unwrap_or_default();
        let max_input_chars = match describe.max_input_chars {
            Some(n) if n < crate::describe::MIN_INPUT_CHARS => anyhow::bail!(
                "{}: describe.max_input_chars is {n}, too little to hold even one page; \
                 give at least {}",
                path.display(),
                crate::describe::MIN_INPUT_CHARS
            ),
            Some(n) => n,
            None => crate::describe::DEFAULT_MAX_INPUT_CHARS,
        };
        let prompt_file = given(&describe.prompt_file);
        let prompt = match &prompt_file {
            Some(file) => {
                let p = home.join(file);
                std::fs::read_to_string(&p).with_context(|| {
                    format!(
                        "{}: reading describe.prompt_file {}",
                        path.display(),
                        p.display()
                    )
                })?
            }
            None => crate::describe::BUILT_IN_PROMPT.to_string(),
        };
        Ok(Some((
            crate::llm::LlmSettings {
                endpoint,
                model,
                timeout,
                notice: given(&llm.notice),
            },
            crate::describe::DescribeSettings {
                max_input_chars,
                prompt,
                prompt_is_custom: prompt_file.is_some(),
            },
        )))
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

    /// The most of one page to read for its text, in bytes; see
    /// [`IndexConfig::max_extract_mb`].
    pub fn max_extract_bytes(&self) -> usize {
        match self.index.max_extract_mb {
            Some(0) => usize::MAX,
            Some(mb) => (mb as usize).saturating_mul(1024 * 1024),
            None => DEFAULT_EXTRACT_BYTES,
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
    fn max_extract_mb_resolves_to_bytes() {
        assert_eq!(Config::default().max_extract_bytes(), DEFAULT_EXTRACT_BYTES);
        let parse = |yaml: &str| -> Config { serde_yaml_ng::from_str(yaml).unwrap() };
        assert_eq!(
            parse("index:\n  max_extract_mb: 4\n").max_extract_bytes(),
            4 * 1024 * 1024
        );
        assert_eq!(
            parse("index:\n  max_extract_mb: 0\n").max_extract_bytes(),
            usize::MAX
        );
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

    /// Write `yaml` as a home's config.yaml and resolve its model settings.
    fn llm_of(
        yaml: &str,
    ) -> (
        tempfile::TempDir,
        Result<Option<(crate::llm::LlmSettings, crate::describe::DescribeSettings)>>,
    ) {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(Config::path(tmp.path()), yaml).unwrap();
        let cfg = Config::load(tmp.path()).unwrap();
        let r = cfg.llm_settings(tmp.path());
        (tmp, r)
    }

    #[test]
    fn no_llm_section_means_drafting_is_off() {
        let (_t, r) = llm_of("index: {}\n");
        assert!(r.unwrap().is_none());
    }

    #[test]
    fn llm_settings_resolve_with_defaults() {
        let (_t, r) = llm_of("llm:\n  endpoint: http://localhost:11434/v1\n  model: llama3.1:8b\n");
        let (llm, describe) = r.unwrap().unwrap();
        assert_eq!(llm.model, "llama3.1:8b");
        assert_eq!(llm.service(), "localhost:11434");
        assert_eq!(llm.timeout.as_secs(), crate::llm::DEFAULT_TIMEOUT_SECS);
        assert_eq!(
            describe.max_input_chars,
            crate::describe::DEFAULT_MAX_INPUT_CHARS
        );
        assert_eq!(describe.prompt, crate::describe::BUILT_IN_PROMPT);
        assert!(!describe.prompt_is_custom);
    }

    #[test]
    fn a_prompt_file_and_budget_replace_the_defaults() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("prompts")).unwrap();
        std::fs::write(
            tmp.path().join("prompts/describe.md"),
            "Describe it briefly.",
        )
        .unwrap();
        std::fs::write(
            Config::path(tmp.path()),
            "llm:\n  endpoint: https://api.example.org/v1\n  model: m\n  timeout_secs: 30\n\
             describe:\n  max_input_chars: 8000\n  prompt_file: prompts/describe.md\n",
        )
        .unwrap();
        let cfg = Config::load(tmp.path()).unwrap();
        let (llm, describe) = cfg.llm_settings(tmp.path()).unwrap().unwrap();
        assert_eq!(llm.timeout.as_secs(), 30);
        assert_eq!(describe.max_input_chars, 8000);
        assert_eq!(describe.prompt, "Describe it briefly.");
        assert!(describe.prompt_is_custom);
    }

    #[test]
    fn the_notice_is_carried_and_a_blank_one_is_unset() {
        let (_t, r) = llm_of(
            "llm:\n  endpoint: http://localhost:1/v1\n  model: m\n  notice: \"Runs here. [More](https://x.org)\"\n",
        );
        let (llm, _) = r.unwrap().unwrap();
        assert_eq!(
            llm.notice.as_deref(),
            Some("Runs here. [More](https://x.org)")
        );
        let (_t, r) =
            llm_of("llm:\n  endpoint: http://localhost:1/v1\n  model: m\n  notice: '  '\n");
        assert_eq!(r.unwrap().unwrap().0.notice, None);
    }

    #[test]
    fn a_blank_prompt_file_is_the_built_in_prompt_and_schemes_ignore_case() {
        let (_t, r) = llm_of(
            "llm:\n  endpoint: HTTPS://api.example.org/v1\n  model: m\n\
             describe:\n  prompt_file: ''\n",
        );
        let (_, describe) = r.unwrap().unwrap();
        assert_eq!(describe.prompt, crate::describe::BUILT_IN_PROMPT);
        assert!(!describe.prompt_is_custom, "reported as built in");
    }

    #[test]
    fn mistakes_in_the_llm_sections_are_refused() {
        for (yaml, says) in [
            (
                "describe:\n  max_input_chars: 100\n",
                "needs an llm: section",
            ),
            ("llm:\n  model: m\n", "llm.endpoint is missing"),
            ("llm:\n  endpoint: http://x/v1\n", "llm.model is missing"),
            (
                "llm:\n  endpoint: ftp://x/v1\n  model: m\n",
                "http:// or https://",
            ),
            (
                "llm:\n  endpoint: http://x/v1\n  model: m\ndescribe:\n  max_input_chars: 0\n",
                "too little",
            ),
            (
                "llm:\n  endpoint: http://x/v1\n  model: m\ndescribe:\n  max_input_chars: 500\n",
                "too little",
            ),
            (
                "llm:\n  endpoint: http://x/v1\n  model: m\ndescribe:\n  prompt_file: nope.md\n",
                "prompt_file",
            ),
        ] {
            let (_t, r) = llm_of(yaml);
            let err = format!("{:#}", r.expect_err(yaml));
            assert!(err.contains(says), "{yaml} -> {err}");
        }
    }
}
