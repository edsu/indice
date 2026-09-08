//! Pulling title, body, description, headings, keywords, author and lang out
//! of an HTML response.

/// Text extracted from an HTML page for indexing.
#[derive(Debug, Default, PartialEq)]
pub struct HtmlText {
    pub title: String,
    pub body: String,
    /// `<meta name=description>` or `og:description`, if present.
    pub description: String,
    /// Concatenated `<h1>`/`<h2>` heading text.
    pub headings: String,
    /// `<meta name=keywords>` content, if present.
    pub keywords: String,
    /// Page author: `<meta name=author>`, falling back to `article:author`.
    pub author: String,
    /// The `<html lang>` attribute value, if present (e.g. `en`, `en-US`).
    pub lang: String,
    /// The page's social-preview image URL: `<meta property=og:image>`, falling
    /// back to `twitter:image`. Used as the crawl's representative thumbnail. May
    /// be relative (resolved against the page URL by the caller).
    pub og_image: String,
}

/// Extract title, body text, description, and headings from raw HTML bytes,
/// skipping script/style content.
pub fn extract_html_text(html: &[u8]) -> HtmlText {
    use scraper::{Html, Selector};

    let html_str = String::from_utf8_lossy(html);
    let doc = Html::parse_document(&html_str);

    let title_sel = Selector::parse("title").unwrap();
    let title = doc
        .select(&title_sel)
        .next()
        .map(|e| e.text().collect::<String>())
        .unwrap_or_default()
        .trim()
        .to_string();

    // Description: prefer <meta name="description">, fall back to og:description.
    let description = meta_content(&doc, "meta[name=description]")
        .or_else(|| meta_content(&doc, "meta[property=\"og:description\"]"))
        .unwrap_or_default();

    // Social-preview image: prefer og:image, fall back to twitter:image. This is
    // the crawl's representative thumbnail source.
    let og_image = meta_content(&doc, "meta[property=\"og:image\"]")
        .or_else(|| meta_content(&doc, "meta[name=\"twitter:image\"]"))
        .unwrap_or_default();

    // Keywords and author from <meta> tags (author falls back to article:author).
    let keywords = meta_content(&doc, "meta[name=keywords]").unwrap_or_default();
    let author = meta_content(&doc, "meta[name=author]")
        .or_else(|| meta_content(&doc, "meta[property=\"article:author\"]"))
        .unwrap_or_default();

    // Headings: h1 and h2 text, in document order.
    let heading_sel = Selector::parse("h1, h2").unwrap();
    let headings = doc
        .select(&heading_sel)
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    // Language from the <html lang="..."> attribute, if any.
    let html_sel = Selector::parse("html").unwrap();
    let lang = doc
        .select(&html_sel)
        .next()
        .and_then(|e| e.value().attr("lang"))
        .unwrap_or("")
        .trim()
        .to_string();

    let body_sel = Selector::parse("body").unwrap();
    let skip_sel = Selector::parse("script, style, noscript").unwrap();
    let mut body_parts: Vec<String> = Vec::new();

    if let Some(body) = doc.select(&body_sel).next() {
        let mut skip_ids = std::collections::HashSet::new();
        for skip_el in body.select(&skip_sel) {
            skip_ids.insert(skip_el.id());
            for desc in skip_el.descendants() {
                skip_ids.insert(desc.id());
            }
        }
        for node in body.descendants() {
            if skip_ids.contains(&node.id()) {
                continue;
            }
            if let scraper::node::Node::Text(t) = node.value() {
                let trimmed = t.trim();
                if !trimmed.is_empty() {
                    body_parts.push(trimmed.to_string());
                }
            }
        }
    }

    HtmlText {
        title,
        body: body_parts.join(" "),
        description,
        headings,
        keywords,
        author,
        lang,
        og_image,
    }
}

/// The trimmed `content` attribute of the first element matching `selector`,
/// dropped if empty. `selector` must be a valid CSS selector.
fn meta_content(doc: &scraper::Html, selector: &str) -> Option<String> {
    let sel = scraper::Selector::parse(selector).ok()?;
    doc.select(&sel)
        .next()
        .and_then(|e| e.value().attr("content"))
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
}
