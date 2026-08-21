//! Build a WACZ from one or more WARC files and (via the caller) index it.
//!
//! indice otherwise only *reads* WACZ; this is the "I have WARCs, not WACZs"
//! on-ramp, and the reusable building block for the Archive-It importer. The
//! output is shaped to match what Webrecorder's own tools produce — the CDX
//! mirrors `warcio.js`'s `CDXIndexer` and the packaging mirrors
//! `browsertrix-crawler`'s `WACZ` class — so it both indexes in indice and
//! replays in ReplayWeb.page/wabac.js.
//!
//! The original WARC bytes are packaged **verbatim** (stored uncompressed in the
//! zip); the CDX offsets are read from the originals, never rewritten.

use anyhow::{Context, Result};
use serde::Serialize;
use url::Url;

use crate::warc::{iter_records, WarcRecord};

/// One CDXJ record's JSON payload, in `warcio.js` `CDXIndexer` field order.
/// Numbers are serialized as strings (quoted) to match warcio; `mime`/`digest`
/// are omitted when absent.
#[derive(Serialize)]
struct CdxjJson<'a> {
    url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<&'a str>,
    length: String,
    offset: String,
    filename: &'a str,
}

/// The CDXJ line for one record (`<surt> <14-digit-ts> {json}`), or `None` if
/// the record isn't indexed. Mirrors `warcio.js`'s `CDXIndexer`: excludes
/// `request`/`warcinfo`, indexes records that carry a target URI + HTTP status
/// (`response`/`revisit`). `mime` drops any `; charset=…` parameter; `digest` is
/// the WARC payload digest with its `algo:` prefix stripped. (POST fuzzy-match
/// keying is a follow-up — see the dlqv notes.)
fn cdxj_line(rec: &WarcRecord, filename: &str) -> Option<String> {
    let ty = rec.warc_type.to_ascii_lowercase();
    if ty == "request" || ty == "warcinfo" {
        return None;
    }
    if rec.target_uri.is_empty() {
        return None;
    }
    // `status` is present for response/revisit; absent for resource/metadata
    // records (e.g. Browsertrix `urn:text:`/`urn:pageinfo:`), which warcio still
    // indexes — and which indice relies on for rendered page text.
    let status = rec.http_status.map(|s| s.to_string());
    let mime = rec
        .content_type
        .split(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let digest = if rec.digest.is_empty() {
        None
    } else {
        Some(
            rec.digest
                .split_once(':')
                .map_or(rec.digest.as_str(), |(_, h)| h),
        )
    };
    let json = serde_json::to_string(&CdxjJson {
        url: &rec.target_uri,
        mime,
        status,
        digest,
        length: rec.record_length.to_string(),
        offset: rec.offset.to_string(),
        filename,
    })
    .ok()?;
    Some(format!(
        "{} {} {}",
        surt(&rec.target_uri),
        rec.timestamp,
        json
    ))
}

/// CDXJ lines for a single WARC, in record order (matching `warcio cdx-index`).
/// The final index sorts across all WARCs at merge time. Used by the
/// reference-oracle conformance test; the builder inlines the same per-record
/// logic in its single validation pass (`scan_warcs`).
#[cfg(test)]
pub(crate) fn cdxj_lines(warc_path: &std::path::Path, filename: &str) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for rec in
        iter_records(warc_path).with_context(|| format!("reading WARC {}", warc_path.display()))?
    {
        let rec = rec?;
        if let Some(line) = cdxj_line(&rec, filename) {
            lines.push(line);
        }
    }
    Ok(lines)
}

/// Compute a SURT (Sort-friendly URI Reordering Transform) key for a URL, a
/// direct port of `warcio.js`'s `getSurt` (`src/lib/utils.ts`) — the same canon
/// wabac.js/ReplayWeb.page apply at replay time, so matching it lets a WACZ we
/// build replay correctly. Ported verbatim (do not "improve" the rules): the
/// fidelity is pinned by a conformance test against `warcio.js` itself.
///
/// Steps: non-`http(s)` → passthrough; strip a leading `www\d*.`; lowercase the
/// whole URL; reverse the host's dot-labels joined by `,` then `)`; append
/// `:port` when present; append the path; sort the `&`-separated query params.
pub(crate) fn surt(url: &str) -> String {
    // getSurt bails (returns the URL unchanged) for non-http(s) schemes. The
    // check is case-sensitive, mirroring `startsWith("http:"/"https:")`.
    if !(url.starts_with("http:") || url.starts_with("https:")) {
        return url.to_string();
    }
    // Strip a leading `www\d*.` right after the scheme (regex is applied before
    // lowercasing, so it is case-sensitive on `www`, exactly like getSurt).
    let stripped = strip_www(url);
    let lower = stripped.to_lowercase();
    // On a parse failure getSurt's `catch` returns its (www-stripped) `url`.
    let parsed = match Url::parse(&lower) {
        Ok(u) => u,
        Err(_) => return stripped,
    };
    let Some(host) = parsed.host_str() else {
        return stripped;
    };
    let mut surt: String = host.split('.').rev().collect::<Vec<_>>().join(",");
    if let Some(port) = parsed.port() {
        surt.push(':');
        surt.push_str(&port.to_string());
    }
    surt.push(')');
    surt.push_str(parsed.path());
    if let Some(query) = parsed.query() {
        if !query.is_empty() {
            let mut args: Vec<&str> = query.split('&').collect();
            args.sort_unstable();
            surt.push('?');
            surt.push_str(&args.join("&"));
        }
    }
    surt
}

/// Remove a leading `www\d*.` from the host of an `http(s)` URL, mirroring
/// getSurt's `url.replace(/^(https?:\/\/)www\d*\./, "$1")`. Case-sensitive on
/// `www` (the regex runs before lowercasing). No match → returned unchanged.
fn strip_www(url: &str) -> String {
    for scheme in ["http://", "https://"] {
        let Some(rest) = url.strip_prefix(scheme) else {
            continue;
        };
        let Some(after_www) = rest.strip_prefix("www") else {
            return url.to_string();
        };
        // `\d*` then a required `.`
        let digits_end = after_www
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after_www.len());
        if let Some(host_rest) = after_www[digits_end..].strip_prefix('.') {
            return format!("{scheme}{host_rest}");
        }
        return url.to_string();
    }
    url.to_string()
}

// ── The builder ──────────────────────────────────────────────────────────────

/// Metadata for a WACZ being built. Every field is optional; sensible defaults
/// are filled in (`created` = now, `software` = indice's version). Descriptive
/// fields flow into `datapackage.json` and, on index, seed the collection
/// finding aid.
#[derive(Debug, Default, Clone)]
pub struct WaczBuildMeta {
    pub title: Option<String>,
    pub description: Option<String>,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub software: Option<String>,
    pub main_page_url: Option<String>,
    pub keywords: Vec<String>,
    pub licenses: Vec<String>,
    pub creator: Option<String>, // datapackage top-level `organization`
    /// Extra top-level keys to merge into `datapackage.json` — e.g.
    /// `"archiveitCrawl"` carrying the source Archive-It crawl record. Frictionless
    /// Data Package allows custom properties, so this travels the provenance in the
    /// file indice already parses (rather than an opaque sidecar), where it can be
    /// read back and displayed later.
    pub datapackage_extra: serde_json::Map<String, serde_json::Value>,
}

/// The result of a successful build.
#[derive(Debug)]
pub struct BuiltWacz {
    pub path: std::path::PathBuf,
    pub warc_records: u64,
    pub cdx_lines: u64,
    pub pages: u64,
}

#[derive(Serialize)]
struct Resource {
    name: String,
    path: String,
    bytes: u64,
    hash: String,
}

#[derive(Serialize)]
struct License {
    title: String,
}

#[derive(Serialize)]
struct DataPackage {
    profile: &'static str,
    wacz_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    created: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
    software: String,
    #[serde(rename = "mainPageUrl", skip_serializing_if = "Option::is_none")]
    main_page_url: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    keywords: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    licenses: Vec<License>,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization: Option<String>,
    resources: Vec<Resource>,
    /// Custom top-level properties (e.g. `archiveitCrawl`), merged in verbatim.
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Convert a 14-digit WARC timestamp (`20060102150405`) to RFC 3339
/// (`2006-01-02T15:04:05Z`) for `pages.jsonl`. Returns the input unchanged if it
/// isn't 14 digits.
fn ts14_to_rfc3339(ts: &str) -> String {
    match chrono::NaiveDateTime::parse_from_str(ts, "%Y%m%d%H%M%S") {
        Ok(dt) => dt
            .and_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        Err(_) => ts.to_string(),
    }
}

/// What the validation/index pass gathered from the input WARCs.
struct Scanned {
    cdxj: Vec<String>,  // sorted CDXJ lines across all inputs
    pages: Vec<String>, // pages.jsonl body lines (seed pages)
    records: u64,       // total WARC records seen
}

/// Assign each input WARC a unique in-zip basename, disambiguating collisions
/// (two inputs sharing a file name, e.g. from a glob across directories) as
/// `<stem>-<n>.<ext>`. The reader keys `warc_data_starts` by basename, so
/// without this two `archive/data.warc.gz` entries would alias and one WARC's
/// CDX offsets would resolve into the other file.
fn unique_basenames(warcs: &[std::path::PathBuf]) -> Result<Vec<String>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(warcs.len());
    for warc in warcs {
        let base = warc
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .with_context(|| format!("{} has no file name", warc.display()))?;
        let mut candidate = base.clone();
        let mut n = 1;
        while !seen.insert(candidate.clone()) {
            n += 1;
            candidate = match base.split_once('.') {
                Some((stem, ext)) => format!("{stem}-{n}.{ext}"),
                None => format!("{base}-{n}"),
            };
        }
        out.push(candidate);
    }
    Ok(out)
}

/// Validate (sniff-test) each WARC and gather its CDXJ + seed pages in a single
/// pass, tagging each record's CDX `filename` with the WARC's (unique) in-zip
/// basename. Fails fast if a file isn't a readable WARC or yields no indexable
/// records — so we never package a broken WACZ.
fn scan_warcs(warcs: &[std::path::PathBuf], names: &[String]) -> Result<Scanned> {
    let mut cdxj = Vec::new();
    let mut pages = Vec::new();
    let mut records = 0u64;
    for (warc, basename) in warcs.iter().zip(names) {
        let mut indexable = 0u64;
        for rec in iter_records(warc)
            .with_context(|| format!("{} does not look like a valid WARC", warc.display()))?
        {
            let rec =
                rec.with_context(|| format!("{} does not look like a valid WARC", warc.display()))?;
            records += 1;
            if let Some(line) = cdxj_line(&rec, basename) {
                indexable += 1;
                cdxj.push(line);
            }
            // Seed page: an HTML page that returned 200.
            if rec.http_status == Some(200)
                && rec.warc_type.eq_ignore_ascii_case("response")
                && rec.content_type.contains("html")
                && !rec.target_uri.is_empty()
            {
                let id = &sha256_hex(rec.target_uri.as_bytes())[..16];
                pages.push(
                    serde_json::json!({
                        "id": id,
                        "url": rec.target_uri,
                        "ts": ts14_to_rfc3339(&rec.timestamp),
                        "title": rec.target_uri,
                        "seed": true,
                    })
                    .to_string(),
                );
            }
        }
        if indexable == 0 {
            anyhow::bail!(
                "{} has no indexable records (no response/resource with a URL) — \
                 nothing to package",
                warc.display()
            );
        }
    }
    // Merge order = sort by the full line (SURT, then timestamp), matching
    // Browsertrix's `sort LC_ALL=C`; Rust's byte-wise str ordering == C locale.
    cdxj.sort_unstable();
    Ok(Scanned {
        cdxj,
        pages,
        records,
    })
}

/// Build a WACZ from one or more WARC files into `out_dir/<out_name>.wacz` and
/// return a summary. Headless and deterministic (no stdin/prompts) — the CLI
/// layer handles interactivity. WARC bytes are packaged **verbatim** (stored
/// uncompressed); every entry uses `Stored`, matching Browsertrix's `client-zip`.
pub fn build_wacz(
    warcs: &[std::path::PathBuf],
    meta: &WaczBuildMeta,
    out_dir: &std::path::Path,
    out_name: &str,
) -> Result<BuiltWacz> {
    use sha2::Digest;
    use std::io::{Read, Write};

    if warcs.is_empty() {
        anyhow::bail!("no WARC files given to build a WACZ from");
    }
    // One unique in-zip basename per input (disambiguates same-named inputs) —
    // used for both the CDX `filename` and the `archive/` entry so they stay 1:1.
    let names = unique_basenames(warcs)?;
    let scanned = scan_warcs(warcs, &names)?;

    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("creating output dir {}", out_dir.display()))?;
    let out_path = out_dir.join(format!("{out_name}.wacz"));
    let file = std::fs::File::create(&out_path)
        .with_context(|| format!("creating {}", out_path.display()))?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    let mut resources: Vec<Resource> = Vec::new();

    // 1. WARCs, copied verbatim (streamed + hashed), one entry per input under
    //    its unique basename.
    for (warc, basename) in warcs.iter().zip(&names) {
        let path = format!("archive/{basename}");
        zip.start_file(&path, stored)
            .with_context(|| format!("writing {path}"))?;
        let mut f =
            std::fs::File::open(warc).with_context(|| format!("opening {}", warc.display()))?;
        let mut hasher = sha2::Sha256::new();
        let mut buf = [0u8; 65536];
        let mut bytes = 0u64;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            bytes += n as u64;
        }
        let hash = hasher.finalize();
        resources.push(Resource {
            name: basename.clone(),
            path,
            bytes,
            hash: format!(
                "sha256:{}",
                hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ),
        });
    }

    // 2. CDX index (sorted, plain uncompressed .cdxj).
    let cdxj = if scanned.cdxj.is_empty() {
        String::new()
    } else {
        scanned.cdxj.join("\n") + "\n"
    };
    zip.start_file("indexes/index.cdxj", stored)?;
    zip.write_all(cdxj.as_bytes())?;
    resources.push(Resource {
        name: "index.cdxj".into(),
        path: "indexes/index.cdxj".into(),
        bytes: cdxj.len() as u64,
        hash: format!("sha256:{}", sha256_hex(cdxj.as_bytes())),
    });

    // 3. pages/pages.jsonl (header line + one seed page per HTML 200).
    let mut pages_jsonl = String::from(
        "{\"format\":\"json-pages-1.0\",\"id\":\"pages\",\"title\":\"Seed Pages\",\"hasText\":\"false\"}\n",
    );
    for line in &scanned.pages {
        pages_jsonl.push_str(line);
        pages_jsonl.push('\n');
    }
    zip.start_file("pages/pages.jsonl", stored)?;
    zip.write_all(pages_jsonl.as_bytes())?;
    resources.push(Resource {
        name: "pages.jsonl".into(),
        path: "pages/pages.jsonl".into(),
        bytes: pages_jsonl.len() as u64,
        hash: format!("sha256:{}", sha256_hex(pages_jsonl.as_bytes())),
    });

    // 4. datapackage.json (browsertrix shape + additive descriptive fields +
    //    any custom top-level properties like `archiveitCrawl`).
    let datapackage = DataPackage {
        profile: "data-package",
        wacz_version: "1.1.1",
        title: meta.title.clone(),
        description: meta.description.clone(),
        created: meta.created.clone().unwrap_or_else(|| {
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }),
        modified: meta.modified.clone(),
        software: meta
            .software
            .clone()
            .unwrap_or_else(|| format!("indice {}", env!("CARGO_PKG_VERSION"))),
        main_page_url: meta.main_page_url.clone(),
        keywords: meta.keywords.clone(),
        licenses: meta
            .licenses
            .iter()
            .map(|t| License { title: t.clone() })
            .collect(),
        organization: meta.creator.clone(),
        resources,
        extra: meta.datapackage_extra.clone(),
    };
    let datapackage_bytes = serde_json::to_vec_pretty(&datapackage)?;
    zip.start_file("datapackage.json", stored)?;
    zip.write_all(&datapackage_bytes)?;

    // 5. datapackage-digest.json.
    let digest = serde_json::json!({
        "path": "datapackage.json",
        "hash": format!("sha256:{}", sha256_hex(&datapackage_bytes)),
    });
    zip.start_file("datapackage-digest.json", stored)?;
    zip.write_all(serde_json::to_vec_pretty(&digest)?.as_slice())?;

    zip.finish().context("finalizing the WACZ zip")?;

    Ok(BuiltWacz {
        path: out_path,
        warc_records: scanned.records,
        cdx_lines: scanned.cdxj.len() as u64,
        pages: scanned.pages.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of URL → expected SURT, hand-derived from `warcio.js`'s `getSurt`
    /// semantics. The dev/CI reference-oracle test (separate) pins these against
    /// `warcio.js` itself; this table is the fast, node-free guard.
    #[test]
    fn surt_matches_getsurt_semantics() {
        let cases = [
            ("http://example.com/", "com,example)/"),
            ("http://example.com", "com,example)/"),
            // www + numeric-www variants are stripped.
            ("http://www.example.com/path", "com,example)/path"),
            ("https://www2.example.com/path", "com,example)/path"),
            // wwwx is NOT stripped (not `www\d*`).
            ("http://wwwx.example.com/", "com,example,wwwx)/"),
            // whole URL is lowercased (host + path).
            ("http://EXAMPLE.com/PaTh", "com,example)/path"),
            // non-default port kept; default ports dropped by the parser.
            ("http://example.com:8080/a", "com,example:8080)/a"),
            ("http://example.com:80/a", "com,example)/a"),
            ("https://example.com:443/a", "com,example)/a"),
            // query params sorted.
            ("http://example.com/p?b=2&a=1", "com,example)/p?a=1&b=2"),
            // subdomains reversed.
            ("https://a.b.example.com/", "com,example,b,a)/"),
            // non-http scheme → passthrough unchanged.
            ("mailto:x@example.com", "mailto:x@example.com"),
            ("urn:text:foo", "urn:text:foo"),
        ];
        for (input, expected) in cases {
            assert_eq!(surt(input), expected, "surt({input})");
        }
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Reference oracle: our CDXJ must match `warcio.js`'s own `cdx-index`
    /// line-for-line (the JS lib that feeds wabac.js/ReplayWeb.page). Gated on a
    /// warcio CLI being available via `INDICE_WARCIO_CLI` (path to warcio's
    /// `cli.js`); skipped with a message otherwise, so plain `cargo test` needs
    /// no Node. CI sets the env to enforce conformance.
    #[test]
    fn cdxj_conforms_to_warcio() {
        let Ok(cli) = std::env::var("INDICE_WARCIO_CLI") else {
            eprintln!("skipping: set INDICE_WARCIO_CLI=<path to warcio cli.js> to run the oracle");
            return;
        };
        // GET-response fixtures only; POST fuzzy-match keying is a follow-up.
        for name in ["simple.warc.gz", "a.warc.gz"] {
            let warc = fixture(name);
            let out = std::process::Command::new("node")
                .arg(&cli)
                .arg("cdx-index")
                .arg(&warc)
                .output()
                .expect("running warcio cli");
            assert!(
                out.status.success(),
                "warcio failed on {name}: {:?}",
                out.status
            );
            let their_lines: Vec<String> = String::from_utf8(out.stdout)
                .unwrap()
                .lines()
                .map(str::to_string)
                .collect();
            let our_lines = cdxj_lines(&warc, name).unwrap();

            // POST fuzzy-match keying (warcio folds the request body into the
            // SURT key + adds method/requestBody) is a deferred follow-up. Exclude
            // records warcio keyed as POST, matched by their unique byte offset,
            // and assert the rest match warcio line-for-line.
            let post_offsets: std::collections::HashSet<String> = their_lines
                .iter()
                .filter(|l| l.contains("\"method\":\"POST\""))
                .filter_map(|l| offset_of(l))
                .collect();
            let keep = |l: &&String| offset_of(l).is_none_or(|o| !post_offsets.contains(&o));
            let theirs: Vec<&String> = their_lines.iter().filter(keep).collect();
            let ours: Vec<&String> = our_lines.iter().filter(keep).collect();
            assert_eq!(ours, theirs, "CDXJ mismatch (non-POST) vs warcio on {name}");
        }
    }

    /// Build a WACZ from a fixture WARC, then confirm indice can read/index its
    /// own output: WARCs stored uncompressed, datapackage round-trips, and the
    /// built WACZ indexes to searchable docs.
    #[test]
    fn build_then_index_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let meta = WaczBuildMeta {
            title: Some("My Build".into()),
            creator: Some("Test Org".into()),
            keywords: vec!["alpha".into(), "beta".into()],
            ..Default::default()
        };
        let built = build_wacz(
            &[fixture("simple.warc.gz")],
            &meta,
            &tmp.path().join("out"),
            "built",
        )
        .unwrap();
        assert!(built.path.exists());
        assert_eq!(built.cdx_lines, 1, "one indexable record in simple.warc.gz");

        // WARCs are stored uncompressed (byte-range replay depends on it).
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&built.path).unwrap()).unwrap();
        assert!(crate::wacz::warcs_stored(&mut zip).unwrap());

        // The verbatim archive entry is byte-identical to the input WARC.
        let mut entry = zip.by_name("archive/simple.warc.gz").unwrap();
        let mut packaged = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut packaged).unwrap();
        drop(entry);
        let original = std::fs::read(fixture("simple.warc.gz")).unwrap();
        assert_eq!(packaged, original, "WARC packaged verbatim");

        // datapackage round-trips our metadata.
        let dp = crate::wacz::read_datapackage(&built.path).unwrap();
        assert_eq!(dp.title.as_deref(), Some("My Build"));
        assert_eq!(dp.creator.as_deref(), Some("Test Org"));
        assert_eq!(dp.keywords, vec!["alpha".to_string(), "beta".to_string()]);

        // indice indexes its own output.
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        crate::index::index_location(
            &built.path.to_string_lossy(),
            &home,
            None,
            "c",
            false,
            false,
            None,
            None,
        )
        .unwrap();
        let idx = crate::index::index_dir(&home).join("full_text");
        let si = crate::search::SearchIndex::open(&idx).unwrap();
        assert!(si.num_docs().unwrap() >= 1, "built WACZ indexed a page");
    }

    #[test]
    fn unique_basenames_disambiguates_collisions() {
        let paths = [
            std::path::PathBuf::from("/x/dup.warc.gz"),
            std::path::PathBuf::from("/y/dup.warc.gz"),
            std::path::PathBuf::from("/z/other.warc.gz"),
            std::path::PathBuf::from("/w/dup.warc.gz"),
        ];
        assert_eq!(
            unique_basenames(&paths).unwrap(),
            vec![
                "dup.warc.gz",
                "dup-2.warc.gz",
                "other.warc.gz",
                "dup-3.warc.gz"
            ]
        );
    }

    #[test]
    fn build_dedupes_same_named_inputs() {
        // Two inputs with the same basename (from different dirs) must not alias
        // in the zip / CDX. Both get distinct archive entries and it indexes.
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::copy(fixture("simple.warc.gz"), a.join("dup.warc.gz")).unwrap();
        std::fs::copy(fixture("simple.warc.gz"), b.join("dup.warc.gz")).unwrap();

        let built = build_wacz(
            &[a.join("dup.warc.gz"), b.join("dup.warc.gz")],
            &WaczBuildMeta::default(),
            &tmp.path().join("out"),
            "dup",
        )
        .unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&built.path).unwrap()).unwrap();
        assert!(zip.by_name("archive/dup.warc.gz").is_ok());
        assert!(zip.by_name("archive/dup-2.warc.gz").is_ok());
    }

    #[test]
    fn build_post_warc_has_no_post_keying_yet() {
        // post.warc.gz has a POST to /api. Until POST fuzzy-match keying lands
        // (rustyweb-wacz-build-post-keying) we emit a plain CDX line for it.
        let lines = cdxj_lines(&fixture("post.warc.gz"), "post.warc.gz").unwrap();
        assert!(!lines.is_empty());
        assert!(
            lines.iter().all(|l| !l.contains("__wb_method")),
            "POST keying is not applied yet: {lines:?}"
        );
    }

    #[test]
    fn build_from_a_plain_uncompressed_warc() {
        // A plain (non-gzip) .warc input is packaged verbatim and still indexes
        // in indice — the "won't CDX-stream" caveat degrades to a full scan, not
        // a failure. (Normalizing to per-record gzip is the --recompress
        // follow-up.)
        let tmp = tempfile::TempDir::new().unwrap();
        let gz = std::fs::read(fixture("simple.warc.gz")).unwrap();
        let mut decoder = flate2::read::GzDecoder::new(&gz[..]);
        let mut plain = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut plain).unwrap();
        let plain_path = tmp.path().join("plain.warc");
        std::fs::write(&plain_path, &plain).unwrap();

        let built = build_wacz(
            &[plain_path],
            &WaczBuildMeta::default(),
            &tmp.path().join("out"),
            "plain",
        )
        .unwrap();
        assert!(built.cdx_lines >= 1);

        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        crate::index::index_location(
            &built.path.to_string_lossy(),
            &home,
            None,
            "c",
            false,
            false,
            None,
            None,
        )
        .unwrap();
        let idx = crate::index::index_dir(&home).join("full_text");
        assert!(
            crate::search::SearchIndex::open(&idx)
                .unwrap()
                .num_docs()
                .unwrap()
                >= 1,
            "plain-WARC WACZ still indexes"
        );
    }

    #[test]
    fn build_rejects_a_non_warc() {
        let tmp = tempfile::TempDir::new().unwrap();
        let junk = tmp.path().join("junk.warc.gz");
        std::fs::write(&junk, b"this is not a warc").unwrap();
        let err = build_wacz(&[junk], &WaczBuildMeta::default(), tmp.path(), "x").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("valid WARC") || msg.contains("no indexable"),
            "unexpected error: {msg}"
        );
        assert!(!tmp.path().join("x.wacz").exists(), "no WACZ on failure");
    }

    /// Extract the `"offset":"N"` value from a CDXJ line (a record's unique key).
    fn offset_of(line: &str) -> Option<String> {
        let start = line.find("\"offset\":\"")? + "\"offset\":\"".len();
        let rest = &line[start..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }
}
