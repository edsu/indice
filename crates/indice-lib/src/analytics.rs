//! Web analytics the operator chooses: a snippet of HTML from their analytics
//! service, added to the `<head>` of every page on the app origin, and the
//! origins it talks to, added to the app's Content-Security-Policy.
//!
//! indice trusts the operator here as it does with `users.yaml`: the snippet
//! goes into pages as it is written, and can do anything indice's own script
//! can. What indice keeps is the shape of the policy. Only the named origins
//! are let in, inline script is allowed by hash rather than wholesale (so no
//! `'unsafe-inline'`, which would also let in any script injected into a page),
//! and the replay origin, where archived pages run, gets nothing.

use anyhow::{bail, Result};
use base64::Engine;
use sha2::Digest;

use crate::config::AnalyticsConfig;

/// `analytics:` from config.yaml, checked and ready for the pages and the
/// policy.
#[derive(Debug, Clone)]
pub struct Analytics {
    /// The snippet, exactly as the operator wrote it.
    pub head: String,
    /// The origins it loads from and sends to, e.g. `https://plausible.io`,
    /// each written the way a policy expects (scheme, host, any port).
    pub origins: Vec<String>,
    /// A `'sha256-…'` source for each inline `<script>` in the snippet, so the
    /// policy allows exactly those.
    pub script_hashes: Vec<String>,
}

impl Analytics {
    /// `None` when there is no snippet, which is the default and means no
    /// analytics. Refused: `origins` without a snippet, which is a snippet
    /// lost to a typo, and an origin a policy can't name (see [`origin`]).
    pub fn from_config(cfg: &AnalyticsConfig) -> Result<Option<Analytics>> {
        let head = cfg.head.as_deref().map(str::trim).unwrap_or_default();
        if head.is_empty() {
            if !cfg.origins.is_empty() {
                bail!("analytics.origins is set but analytics.head is not: give the snippet too");
            }
            return Ok(None);
        }
        let origins = cfg
            .origins
            .iter()
            .map(|o| origin(o))
            .collect::<Result<Vec<_>>>()?;
        let script_hashes = inline_scripts(head)
            .into_iter()
            .map(|body| {
                let digest = sha2::Sha256::digest(body.as_bytes());
                let b64 = base64::engine::general_purpose::STANDARD.encode(digest);
                format!("'sha256-{b64}'")
            })
            .collect();
        Ok(Some(Analytics {
            head: head.to_string(),
            origins,
            script_hashes,
        }))
    }
}

/// One of `analytics.origins` as a policy source: `https://host` or
/// `https://host:port`. Only the origin, because a policy can name a path, but
/// a tracker's script and its collector are rarely under the same one, and an
/// operator writing the script's URL here means its host. Anything else, a
/// scheme other than http(s) or a value that isn't a URL, is refused rather
/// than written into a header.
fn origin(given: &str) -> Result<String> {
    let url = url::Url::parse(given.trim())
        .map_err(|e| anyhow::anyhow!("analytics.origins: {given:?} is not a URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("analytics.origins: {given:?} must be an http:// or https:// origin");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("analytics.origins: {given:?} must not hold a user name or password");
    }
    Ok(url.origin().ascii_serialization())
}

/// The text of each `<script>` element in `html` that has no `src`, which is
/// what a browser hashes to check it against the policy. An element with a
/// `src` runs the file it names and ignores its text, so it is let in by its
/// origin instead.
///
/// A scan rather than an HTML parser: the snippet is a few lines the operator
/// pasted from their analytics service, and in a `<script>` element the text
/// runs, unparsed, to the first `</script`.
fn inline_scripts(html: &str) -> Vec<&str> {
    // ASCII lowercasing keeps every byte offset, so positions found in
    // `lower` slice `html`.
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<script") {
        let open = at + i;
        let name_end = open + "<script".len();
        // `<scripts>` or `<script-x>` is another element.
        if !lower[name_end..].starts_with(|c: char| c == '>' || c == '/' || c.is_whitespace()) {
            at = name_end;
            continue;
        }
        let Some(tag_end) = tag_end(&lower, name_end) else {
            break;
        };
        let body_start = tag_end + 1;
        let body_end = lower[body_start..]
            .find("</script")
            .map_or(html.len(), |j| body_start + j);
        if !has_attribute(&lower[name_end..tag_end], "src") {
            out.push(&html[body_start..body_end]);
        }
        at = body_end;
    }
    out
}

/// Where the start tag that began before `from` ends: the first `>` not
/// inside a quoted attribute value.
fn tag_end(lower: &str, from: usize) -> Option<usize> {
    let mut quote = None;
    for (i, c) in lower[from..].char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '>') => return Some(from + i),
            _ => {}
        }
    }
    None
}

/// Whether a start tag's attributes (the text after its name) include `name`.
fn has_attribute(attrs: &str, name: &str) -> bool {
    let mut chars = attrs.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace() || *c == '/').is_some() {}
        let attr: String = std::iter::from_fn(|| {
            chars.next_if(|c| !c.is_whitespace() && !matches!(c, '=' | '>' | '/'))
        })
        .collect();
        if attr.is_empty() {
            return false;
        }
        if attr == name {
            return true;
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.next_if_eq(&'=').is_some() {
            while chars.next_if(|c| c.is_whitespace()).is_some() {}
            match chars.next_if(|c| *c == '"' || *c == '\'') {
                Some(q) => while chars.next().is_some_and(|c| c != q) {},
                None => while chars.next_if(|c| !c.is_whitespace()).is_some() {},
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(head: &str, origins: &[&str]) -> AnalyticsConfig {
        AnalyticsConfig {
            head: Some(head.into()),
            origins: origins.iter().map(|o| o.to_string()).collect(),
        }
    }

    #[test]
    fn no_snippet_is_no_analytics() {
        assert!(Analytics::from_config(&AnalyticsConfig::default())
            .unwrap()
            .is_none());
        assert!(Analytics::from_config(&cfg(" \n", &[])).unwrap().is_none());
        let err = Analytics::from_config(&cfg("", &["https://plausible.io"])).unwrap_err();
        assert!(err.to_string().contains("analytics.head"), "{err}");
    }

    #[test]
    fn origins_are_written_as_a_policy_expects() {
        let a = Analytics::from_config(&cfg(
            "<script src=x></script>",
            &[
                "https://plausible.io/js/script.js",
                "https://stats.example.org:8443/",
                "HTTPS://Example.COM",
            ],
        ))
        .unwrap()
        .unwrap();
        assert_eq!(
            a.origins,
            [
                "https://plausible.io",
                "https://stats.example.org:8443",
                "https://example.com"
            ]
        );
        for bad in [
            "plausible.io",
            "javascript:alert(1)",
            "data:text/javascript,1",
            "https://a b",
            "https://u:p@example.com",
            "'unsafe-inline'",
            "*",
        ] {
            assert!(
                Analytics::from_config(&cfg("<script></script>", &[bad])).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn only_inline_scripts_are_hashed() {
        let html = r#"<script defer data-domain="a.org" src="https://plausible.io/js/script.js"></script>
<SCRIPT type="text/javascript">window.plausible = window.plausible || function () {};</SCRIPT>
<script data-note="a > b src=c">one()</script><scripts>not()</scripts>
<noscript><img src="https://example.org/p.gif"></noscript>"#;
        assert_eq!(
            inline_scripts(html),
            [
                "window.plausible = window.plausible || function () {};",
                "one()"
            ]
        );
        assert_eq!(inline_scripts("<script async src=x.js></script>"), [""; 0]);
        assert_eq!(inline_scripts("<script>a()"), ["a()"], "unclosed");
    }

    #[test]
    fn a_hash_is_the_one_a_browser_computes() {
        // The value from CSP Level 3's own example of a hash source:
        // `alert('Hello, world.');` is `sha256-qznLcsROx4GACP2dm0UCKCzCG+HiZ1guq6ZZDob/Tng=`.
        let a = Analytics::from_config(&cfg("<script>alert('Hello, world.');</script>", &[]))
            .unwrap()
            .unwrap();
        assert_eq!(
            a.script_hashes,
            ["'sha256-qznLcsROx4GACP2dm0UCKCzCG+HiZ1guq6ZZDob/Tng='"]
        );
    }
}
