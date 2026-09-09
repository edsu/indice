//! Render curator-authored Markdown (collection narratives, crawl notes) to a
//! **safe HTML subset** for splicing into the server-rendered pages.
//!
//! Curator- and importer-supplied content is untrusted, so we do not pass raw
//! HTML through: [`render`] treats HTML events as escaped text (no `<script>`
//! injection), validates link/image destinations to `http`/`https`/`mailto`
//! (dropping `javascript:`, `data:`, etc.), and neutralizes images to their alt
//! text. This uses only `pulldown-cmark` (pure Rust) — no `ammonia`/`html5ever`
//! sanitizer chain.

use maud::PreEscaped;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

/// Render a Markdown string to a sanitized HTML fragment.
pub fn render(md: &str) -> PreEscaped<String> {
    // `drop_link` tracks a link whose destination we rejected: we skip its
    // Start/End tags but keep the inner text (so the words survive, unlinked).
    // Links cannot nest in CommonMark, so a single bool would do; a counter is
    // simply robust.
    let mut drop_link = 0usize;
    let events: Vec<Event> = Parser::new(md)
        .filter_map(|ev| match ev {
            // Never pass raw HTML through — emit it as escaped text instead.
            Event::Html(s) | Event::InlineHtml(s) => Some(Event::Text(s)),

            // Drop images entirely; their alt text (inner Text events) remains.
            Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image) => None,

            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => match safe_dest(&dest_url) {
                Some(dest) => Some(Event::Start(Tag::Link {
                    link_type,
                    dest_url: dest.into(),
                    title,
                    id,
                })),
                None => {
                    drop_link += 1;
                    None
                }
            },
            Event::End(TagEnd::Link) if drop_link > 0 => {
                drop_link -= 1;
                None
            }

            other => Some(other),
        })
        .collect();

    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    PreEscaped(html)
}

/// Validate a link/image destination. Relative paths and `#anchors` are allowed;
/// an absolute URL is allowed only for `http`/`https`/`mailto`. Anything with
/// another scheme (`javascript:`, `data:`, …) is rejected (`None`).
fn safe_dest(url: &str) -> Option<String> {
    let u = url.trim();
    if u.is_empty() {
        return None;
    }
    // A "scheme" is text up to the first ':' that appears before any '/'.
    let colon = u.find(':');
    let slash = u.find('/');
    let has_scheme = match (colon, slash) {
        (Some(c), Some(s)) => c < s,
        (Some(_), None) => true,
        _ => false,
    };
    if has_scheme {
        let scheme = u[..colon.unwrap()].to_ascii_lowercase();
        matches!(scheme.as_str(), "http" | "https" | "mailto").then(|| u.to_string())
    } else {
        // Relative path or #anchor — and, deliberately, a protocol-relative URL
        // (`//host/path`), which has no scheme so it lands here. That's a live
        // link to another origin, but not a new capability: `http`/`https` are
        // allowed above, so a note can always link out. It is not script-capable,
        // which is what this function exists to prevent.
        Some(u.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(md: &str) -> String {
        render(md).0
    }

    #[test]
    fn allowed_subset_renders() {
        let out =
            html("# Heading\n\nA **bold** and *italic* and `code`.\n\n- one\n- two\n\n> quote\n");
        for frag in [
            "<h1>",
            "<strong>",
            "<em>",
            "<code>",
            "<ul>",
            "<li>",
            "<p>",
            "<blockquote>",
        ] {
            assert!(out.contains(frag), "expected {frag} in: {out}");
        }
    }

    #[test]
    fn raw_html_is_escaped_not_passed_through() {
        let out = html("hi <script>alert(1)</script> <b>x</b>");
        assert!(
            !out.contains("<script>"),
            "raw <script> must not pass through: {out}"
        );
        assert!(out.contains("&lt;script&gt;"), "should be escaped: {out}");
        assert!(
            !out.contains("<b>"),
            "raw inline HTML must not pass through: {out}"
        );
    }

    #[test]
    fn javascript_link_is_dropped_but_text_kept() {
        let out = html("click [here](javascript:alert(1)) now");
        assert!(
            !out.to_lowercase().contains("javascript"),
            "js scheme dropped: {out}"
        );
        assert!(!out.contains("<a "), "no anchor for a rejected dest: {out}");
        assert!(out.contains("here"), "link text preserved: {out}");
    }

    #[test]
    fn safe_link_is_kept() {
        let out = html("[site](https://example.org/x) and [mail](mailto:a@b.org)");
        assert!(out.contains("href=\"https://example.org/x\""), "{out}");
        assert!(out.contains("href=\"mailto:a@b.org\""), "{out}");
    }

    #[test]
    fn image_neutralized_to_alt_text() {
        let out = html("![a diagram](https://ex.org/x.png) and ![evil](javascript:x)");
        assert!(!out.contains("<img"), "images dropped: {out}");
        assert!(out.contains("a diagram"), "alt text kept: {out}");
        assert!(!out.to_lowercase().contains("javascript"), "{out}");
    }

    #[test]
    fn relative_and_anchor_links_allowed() {
        let out = html("[a](page.html) [b](#sec) [c](/root)");
        assert!(out.contains("href=\"page.html\""), "{out}");
        assert!(out.contains("href=\"#sec\""), "{out}");
        assert!(out.contains("href=\"/root\""), "{out}");
    }

    /// Bypass attempts against [`render`], since its output is spliced into the
    /// DOM with `innerHTML` (annotations.js) and `PreEscaped` (the finding-aid
    /// and annotation views). Annotation bodies are authored by one user and
    /// shown to others, so this is the stored-XSS boundary.
    ///
    /// Asserts the property, not the spelling: user input must never become an
    /// *element*, and no live anchor may carry a script-capable scheme. A
    /// substring check for "javascript:" would fail on correct output, because
    /// hostile markup is escaped into visible text that legitimately contains it.
    #[test]
    fn no_bypass_produces_executable_markup() {
        const ALLOWED: &[&str] = &[
            "p",
            "strong",
            "em",
            "code",
            "pre",
            "ul",
            "ol",
            "li",
            "blockquote",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "a",
            "hr",
            "br",
        ];
        let cases: &[(&str, &str)] = &[
            (
                "attribute break via href",
                r#"[x](/a" onmouseover="alert(1))"#,
            ),
            (
                "attribute break via title",
                r#"[x](http://a.com "t" onmouseover="alert(1)")"#,
            ),
            ("scheme case variation", "[x](JaVaScRiPt:alert(1))"),
            ("leading whitespace", "[x]( javascript:alert(1))"),
            ("entity-encoded tab", "[x](java&#9;script:alert(1))"),
            ("entity-encoded letter", "[x](&#106;avascript:alert(1))"),
            ("entity-encoded colon", "[x](javascript&#58;alert(1))"),
            ("embedded null", "[x](java\u{0}script:alert(1))"),
            ("autolink", "<javascript:alert(1)>"),
            ("reference link", "[x][r]\n\n[r]: javascript:alert(1)"),
            (
                "code-fence info string",
                "```\"><script>alert(1)</script>\ncode\n```",
            ),
            ("image with script url", "![x](javascript:alert(1))"),
            ("html comment", "<!--><script>alert(1)</script>-->"),
            ("cdata", "<![CDATA[<script>alert(1)</script>]]>"),
            (
                "raw event handler",
                r#"<div onmouseover="alert(1)">x</div>"#,
            ),
        ];
        for (name, md) in cases {
            let out = html(md);
            let lower = out.to_lowercase();
            let bytes = lower.as_bytes();
            for (i, _) in lower.match_indices('<') {
                let rest = &bytes[i + 1..];
                let rest = if rest.first() == Some(&b'/') {
                    &rest[1..]
                } else {
                    rest
                };
                if !rest.first().is_some_and(|c| c.is_ascii_alphabetic()) {
                    continue;
                }
                let tag: String = rest
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .map(|c| *c as char)
                    .collect();
                assert!(
                    ALLOWED.contains(&tag.as_str()),
                    "{name}: produced <{tag}>, which render() never emits\n  in : {md}\n  out: {out}"
                );
            }
            // Escaped anchors read `&lt;a`, so `<a` matches only live markup.
            for (i, _) in lower.match_indices("<a") {
                let end = lower[i..].find('>').map(|e| i + e).unwrap_or(lower.len());
                let tag = &lower[i..end];
                for scheme in ["javascript:", "data:", "vbscript:"] {
                    assert!(
                        !tag.contains(scheme),
                        "{name}: live anchor with {scheme}\n  in : {md}\n  out: {out}"
                    );
                }
            }
        }
    }
}
