//! Entry points for the fuzz targets in `fuzz/` (#225), which are a separate
//! crate and so can only call what is `pub`. Each runs a parser that reads
//! untrusted bytes the way indexing does, and throws the result away: what a
//! fuzzer is looking for is a panic, a hang or memory past what [`LIMIT`]
//! allows, not a wrong answer.
//!
//! Compiled into every build rather than behind a feature, so that a change
//! to a parser's signature breaks here, under the normal `cargo build`, and
//! not only when someone next runs the fuzzer.

use std::io::Cursor;

/// The member limit the targets read with, in place of `index.max_member_mb`
/// (1 GiB by default): small, so that a decompression bomb reaches it in
/// milliseconds, and so that a run with `-rss_limit_mb=1024` stopping means
/// memory grew past what the limit is meant to bound.
pub const LIMIT: u64 = 8 * 1024 * 1024;

/// The WARC record and `warcinfo` field parsers, on bytes as a decompressed
/// record slice holds them.
pub fn warc(data: &[u8]) {
    let _ = crate::warc::parse_warc_records(data, 0, data.len() as u64);
    let _ = crate::warc::parse_warc_fields(data);
}

/// One record's bytes as a CDX lookup fetches them, gzip member or plain,
/// inflated to at most [`LIMIT`] bytes.
pub fn warc_slice(data: &[u8]) {
    let _ = crate::wacz::records_from_slice(data, 0, data.len() as u64, LIMIT);
}

/// A WACZ, through everything indexing reads from one before its records:
/// `datapackage.json` and the page lists, nested WACZs, whether the WARCs are
/// stored, where they start, their `warcinfo`, and the CDX index. Each member
/// is read to at most [`LIMIT`] bytes.
pub fn wacz(data: &[u8]) {
    let _ = crate::wacz::read_datapackage_from(Cursor::new(data), LIMIT);
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(data)) else {
        return;
    };
    let _ = crate::wacz::nested_wacz_locations(&mut zip);
    let _ = crate::wacz::ensure_warcs_stored(&mut zip);
    let _ = crate::wacz::warc_data_starts(&mut zip);
    let _ = crate::wacz::find_warcinfo_streaming(&mut zip, LIMIT);
    let _ = crate::wacz::cdx_records(&mut zip, LIMIT);
    let _ = crate::wacz::for_each_page_text(&mut zip, LIMIT, |_, _| Ok(()));
    let scratch = crate::wacz::Scratch::new(Some(LIMIT));
    if let Ok(files) = crate::wacz::spool_page_lists(&mut zip, LIMIT, &scratch) {
        let _ = crate::wacz::for_each_spooled_page_text(&files, |_, _| Ok(()));
    }
}

/// An HTML response as indexing reads it for text, and as thumbnailing reads
/// the main page for images. Any page in an uploaded WACZ reaches both.
pub fn html(data: &[u8]) {
    let _ = crate::search::extract_html_text(data);
    let _ = crate::thumbnail::candidate_images(data);
}

/// A captured image as thumbnailing decodes, downscales and re-encodes it.
/// Any image in an uploaded WACZ can be picked as a crawl's thumbnail.
pub fn image(data: &[u8]) {
    let _ = crate::thumbnail::thumbnail_jpeg(data);
}

/// A search as the search box and `/api/search` run it, on queries no longer
/// than the server accepts, against a small index with a page, a PDF, an
/// annotation and a collection in it. Anyone who can reach the server can
/// send one, so a query that panics or runs for seconds is a finding.
pub fn search(data: &[u8]) {
    let Ok(query) = std::str::from_utf8(data) else {
        return;
    };
    if query.len() > crate::server::MAX_QUERY_LEN {
        return;
    }
    let _ = search_index().search_faceted(query, 20, 0);
}

/// Built once per process: indexing is far slower than a search.
fn search_index() -> &'static crate::search::SearchIndex {
    use crate::search::{Page, SearchIndex};
    static INDEX: std::sync::OnceLock<(tempfile::TempDir, SearchIndex)> =
        std::sync::OnceLock::new();
    let (_dir, index) = INDEX.get_or_init(|| {
        let dir = tempfile::TempDir::new().expect("temp dir for the fuzz index");
        let mut writer = SearchIndex::open(dir.path()).expect("fuzz index");
        let pages = [
            Page {
                url: "https://example.com/climate/report.html",
                timestamp: "20210304050607",
                title: "Climate report",
                body: "Sea levels and the climate, measured in 2021.",
                description: "A report",
                headings: "Findings",
                keywords: "climate, sea",
                author: "A. Author",
                media_type: "html",
                lang: "en",
                status: Some(200),
                modified_year: Some(2021),
                crawl_id: "c1",
                crawl_name: "Crawl one",
                collection: "climate",
            },
            Page {
                url: "https://example.org/data.pdf",
                timestamp: "19990101000000",
                title: "Données",
                body: "Le climat et la mer.",
                media_type: "pdf",
                lang: "fr",
                status: Some(404),
                crawl_id: "c2",
                crawl_name: "Crawl two",
                collection: "other",
                ..Default::default()
            },
        ];
        for page in &pages {
            writer.index_page(page).expect("fuzz page");
        }
        writer
            .index_annotation(
                "n1",
                "climate",
                "https://example.com/climate/report.html",
                "20210304050607",
                "Someone",
                "A note on sea levels",
            )
            .expect("fuzz annotation");
        writer.commit().expect("fuzz commit");
        drop(writer);
        let index = SearchIndex::open_read_only(dir.path()).expect("fuzz index");
        (dir, index)
    });
    index
}

/// A `Range` header against a file whose size is the first 8 bytes. Whatever
/// is served must be inside the file, start no later than it ends, and so
/// have a length that does not wrap.
pub fn byte_range(data: &[u8]) {
    let Some((size, header)) = data.split_first_chunk::<8>() else {
        return;
    };
    let size = u64::from_le_bytes(*size);
    let Ok(header) = std::str::from_utf8(header) else {
        return;
    };
    if let Some((start, end)) = crate::server::parse_byte_range(header, size) {
        assert!(
            start <= end && end < size,
            "{header:?} of {size} gave {start}-{end}"
        );
    }
}

/// Curator Markdown rendered for every visitor, which must not be able to
/// run script in their browser: see [`unsafe_html`].
pub fn markdown(data: &[u8]) {
    let Ok(md) = std::str::from_utf8(data) else {
        return;
    };
    let html = crate::markdown::render(md).0;
    if let Some(why) = unsafe_html(&html) {
        panic!("{why}\n--- markdown\n{md:?}\n--- html\n{html}");
    }
}

/// Why `html` could run script, if it could: a tag [`crate::markdown::render`]
/// does not make, an attribute other than the few it does (so no `on*=`), or
/// a link whose URL, read the way a browser reads it, has a scheme other than
/// `http`, `https` or `mailto`. An allowlist, so that anything the renderer
/// starts emitting is looked at before it is trusted.
pub fn unsafe_html(html: &str) -> Option<String> {
    const TAGS: &[&str] = &[
        "p",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "em",
        "strong",
        "code",
        "pre",
        "blockquote",
        "ul",
        "ol",
        "li",
        "a",
        "hr",
        "br",
    ];
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        // Text is escaped, so every `<` starts a tag, and attribute values
        // are escaped too, so the first `>` ends it.
        let Some(close) = rest[open..].find('>') else {
            return Some(format!("an unclosed tag: {:?}", &rest[open..]));
        };
        let tag = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let tag = tag.strip_prefix('/').unwrap_or(tag);
        let tag = tag.strip_suffix('/').unwrap_or(tag).trim_end();
        let (name, mut attrs) = tag.split_once(' ').unwrap_or((tag, ""));
        if !TAGS.contains(&name) {
            return Some(format!("a <{name}> tag"));
        }
        loop {
            attrs = attrs.trim_start();
            if attrs.is_empty() {
                break;
            }
            let Some((attr, after)) = attrs.split_once("=\"") else {
                return Some(format!(
                    "an attribute that is not name=\"value\": {attrs:?}"
                ));
            };
            let Some((value, after)) = after.split_once('"') else {
                return Some(format!("an unterminated {attr} value"));
            };
            attrs = after;
            match (name, attr) {
                ("a", "href") => {
                    if let Some(scheme) = browser_scheme(value) {
                        if !matches!(scheme.as_str(), "http" | "https" | "mailto") {
                            return Some(format!("a {scheme}: link: {value:?}"));
                        }
                    }
                }
                ("a", "title") | ("ol", "start") | ("code", "class") => {}
                _ => return Some(format!("a {attr} attribute on <{name}>")),
            }
        }
    }
    None
}

/// The scheme a browser would find in an `href` value, lowercased: after
/// decoding character references once, dropping leading and trailing
/// controls and spaces, and removing tabs and newlines, as the URL parser
/// does. `None` for a relative URL.
fn browser_scheme(href: &str) -> Option<String> {
    let mut decoded = String::new();
    let mut rest = href;
    while let Some(amp) = rest.find('&') {
        decoded.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let reference = rest.find(';').map(|semi| &rest[1..semi]);
        let ch = reference.and_then(|r| match r {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => {
                let n = r.strip_prefix('#')?;
                let code = match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => n.parse().ok()?,
                };
                char::from_u32(code)
            }
        });
        match (ch, reference) {
            (Some(ch), Some(r)) => {
                decoded.push(ch);
                rest = &rest[r.len() + 2..];
            }
            _ => {
                decoded.push('&');
                rest = &rest[1..];
            }
        }
    }
    decoded.push_str(rest);
    let url: String = decoded
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then(|| scheme.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_html_passes_what_render_makes() {
        let md = "# T\n\n*a* **b** `c` [l](https://x.test \"t\") [m](mailto:a@x.test) \
                  [r](/p#q)\n\n3. one\n4. two\n\n```rust\nlet x = 1;\n```\n\n> q\n\n---\n";
        let html = crate::markdown::render(md).0;
        assert_eq!(unsafe_html(&html), None, "{html}");
    }

    #[test]
    fn unsafe_html_catches_what_could_run_script() {
        for html in [
            "<script>x</script>",
            "<img src=\"x\">",
            "<p onclick=\"x\">",
            "<a href=\"javascript:x\">",
            "<a href=\"JavaScript:x\">",
            "<a href=\"&#x6A;avascript:x\">",
            "<a href=\"&#106;avascript:x\">",
            "<a href=\" \u{1}java\tscript:x\">",
            "<a href=\"data:text/html,x\">",
        ] {
            assert!(unsafe_html(html).is_some(), "{html}");
        }
    }

    #[test]
    fn every_target_runs_on_its_simplest_input() {
        search(b"climate");
        search(b"title:\"sea levels\" -crawl:c2 year:[1990 TO 2022]");
        byte_range(&[100, 0, 0, 0, 0, 0, 0, 0, b'b']);
        markdown(b"[x](javascript:alert(1)) <script>");
        html(b"<title>T</title><h1>H</h1><img srcset=\"a.jpg 1x\">");
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(4, 3)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        assert!(crate::thumbnail::thumbnail_jpeg(png.get_ref()).is_ok());
        image(png.get_ref());
    }
}
