//! The Tantivy schema: every field name in one place, plus the field-option
//! helpers and `build_schema`. Everything else refers to fields through these
//! constants.

use tantivy::schema::{
    IndexRecordOption, Schema, TextFieldIndexing, TextOptions, FAST, INDEXED, STORED, STRING, TEXT,
};

pub(super) const FIELD_DOC_TYPE: &str = "doc_type";

pub(super) const FIELD_CRAWL_ID: &str = "crawl_id";

pub(super) const FIELD_CRAWL_NAME: &str = "crawl_name";

pub(super) const FIELD_URL: &str = "url";

pub(super) const FIELD_TS: &str = "timestamp";

pub(super) const FIELD_TITLE: &str = "title";

/// Full extracted page text — indexed (so search matches the whole page) but
/// NOT stored. The stored copy for snippets is the capped [`FIELD_BODY_SNIP`].
pub(super) const FIELD_BODY: &str = "body";

/// A capped prefix of the body text, STORED (not indexed) for snippet display.
/// Snippets highlight within this prefix; a match deeper in the page than the
/// cap still counts for search (the full body is indexed) but can't be
/// highlighted, so results fall back to the description / a leading excerpt.
/// Bounding the stored text is the headline on-disk size lever at scale.
pub(super) const FIELD_BODY_SNIP: &str = "body_snip";

/// Exact host of a page URL (e.g. `www.example.com`), for `domain:` filtering.
pub(super) const FIELD_DOMAIN: &str = "domain";

/// Registrable domain of a page URL (eTLD+1, e.g. `example.com` for any
/// `*.example.com` host), for the cross-subdomain `site:` filter and Site facet.
pub(super) const FIELD_SITE: &str = "site";

/// Tokenized words from a page URL (host + path), so URL words are searchable.
pub(super) const FIELD_URL_TOKENS: &str = "url_tokens";

/// Page description from `<meta name=description>` / `og:description`.
pub(super) const FIELD_DESCRIPTION: &str = "description";

/// Concatenated `<h1>`/`<h2>` heading text.
pub(super) const FIELD_HEADINGS: &str = "headings";

/// `<meta name=keywords>` content.
pub(super) const FIELD_KEYWORDS: &str = "keywords";

/// Page author (`<meta name=author>` / `article:author`), for `author:` search.
pub(super) const FIELD_AUTHOR: &str = "author";

/// Four-digit crawl year (from the page timestamp), for `year:` filtering.
pub(super) const FIELD_YEAR: &str = "year";

/// Six-digit crawl month `YYYYMM` (from the page timestamp), for `month:`
/// filtering/range and the results timeline.
pub(super) const FIELD_MONTH: &str = "month";

/// Coarse media type of the page: `html` or `pdf`, for `type:` filtering.
pub(super) const FIELD_MEDIA_TYPE: &str = "type";

/// Primary language subtag from `<html lang>` (e.g. `en`), for `lang:` filtering.
pub(super) const FIELD_LANG: &str = "lang";

/// Curated collection id (slug) this document belongs to, for `collection:` filtering.
pub(super) const FIELD_COLLECTION: &str = "collection";

/// HTTP response status code of the capture, for `status:200` filtering.
pub(super) const FIELD_STATUS: &str = "status";

/// Year from the HTTP `Last-Modified` header, for `modified:2015` filtering
/// (when the content was authored, vs `year:` = when it was crawled).
pub(super) const FIELD_MODIFIED: &str = "modified";

/// Stable id of an annotation document (`urn:indice:annotation:…`). Empty on
/// page/collection docs; set only on `doc_type = "annotation"` docs so a single
/// note can be upserted/deleted in the index by its id.
pub(super) const FIELD_ANNOTATION_ID: &str = "annotation_id";

/// A string field that is indexed as a single raw token (like [`STRING`]),
/// stored, **and** kept as a fast (columnar) field so it can back a terms
/// aggregation for facet counts. The `raw` tokenizer keeps the whole value as
/// one term, so a facet bucket is the exact field value (e.g. one `domain:`
/// host), not individual words.
fn facet_string() -> TextOptions {
    TextOptions::default()
        .set_stored()
        .set_fast(Some("raw"))
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic),
        )
}

/// A tokenized text field indexed WITHOUT term positions (frequencies only):
/// fully searchable as a bag of words, but not usable for phrase queries. Used
/// only where phrase matching adds nothing — `headings` (whose text is already
/// in `body`, which keeps positions) and `url_tokens` (URL words) — dropping
/// their positions from `.pos`. Everything phrase-useful (`title`, `body`,
/// `description`, `keywords`, `author`) keeps positions.
fn text_no_positions() -> TextOptions {
    TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(IndexRecordOption::WithFreqs),
    )
}

pub(super) fn build_schema() -> Schema {
    let mut builder = Schema::builder();
    builder.add_text_field(FIELD_DOC_TYPE, STRING | STORED);
    builder.add_text_field(FIELD_CRAWL_ID, STRING | STORED);
    builder.add_text_field(FIELD_CRAWL_NAME, STRING | STORED);
    // Curated collection id (slug), for `collection:` filtering and faceting.
    builder.add_text_field(FIELD_COLLECTION, facet_string());
    builder.add_text_field(FIELD_URL, STRING | STORED);
    builder.add_text_field(FIELD_TS, STRING | STORED);
    builder.add_text_field(FIELD_TITLE, TEXT | STORED);
    // Body is indexed for full-text recall but NOT stored; the stored copy for
    // snippets is the capped body_snip, so the doc store stays bounded.
    builder.add_text_field(FIELD_BODY, TEXT);
    builder.add_text_field(FIELD_BODY_SNIP, STORED);
    // Description is stored (a snippet fallback) and keeps positions.
    builder.add_text_field(FIELD_DESCRIPTION, TEXT | STORED);
    // Positions are dropped only where phrase queries add nothing: `headings`
    // (its text is duplicated into `body`, which keeps positions, so heading
    // phrases still match via `body`) and, below, `url_tokens` (URL words are
    // never phrase-searched). `keywords`/`author` keep positions — they're short,
    // so the positions cost is negligible, and dropping them would silently break
    // phrase queries against author/keywords text that isn't in `body`.
    builder.add_text_field(FIELD_HEADINGS, text_no_positions());
    builder.add_text_field(FIELD_KEYWORDS, TEXT);
    // STORED so an annotation result can show its note author (pages rarely set
    // a useful `<meta author>`, so this is cheap for the corpus at large).
    builder.add_text_field(FIELD_AUTHOR, TEXT | STORED);
    // Exact host, for `domain:host` filtering and results display.
    builder.add_text_field(FIELD_DOMAIN, facet_string());
    // Registrable domain, for the cross-subdomain `site:` filter and Site facet.
    builder.add_text_field(FIELD_SITE, facet_string());
    // Tokenized URL words; searchable but not stored, and no positions.
    builder.add_text_field(FIELD_URL_TOKENS, text_no_positions());
    // Numeric crawl year: indexed for `year:2021` / `year:[2020 TO 2023]`, and
    // fast so it can back the year facet.
    builder.add_u64_field(FIELD_YEAR, INDEXED | STORED | FAST);
    // Numeric crawl month `YYYYMM`: indexed for `month:202103` /
    // `month:[202101 TO 202106]`, fast so it backs the results timeline.
    builder.add_u64_field(FIELD_MONTH, INDEXED | STORED | FAST);
    // Coarse media type (`html`/`pdf`) and page language, for filtering + facets.
    builder.add_text_field(FIELD_MEDIA_TYPE, facet_string());
    builder.add_text_field(FIELD_LANG, facet_string());
    // HTTP status code, for `status:200` / `status:[200 TO 299]`.
    builder.add_u64_field(FIELD_STATUS, INDEXED | STORED);
    // Last-Modified year, for `modified:2015` / range filtering.
    builder.add_u64_field(FIELD_MODIFIED, INDEXED | STORED);
    // Annotation id: a single indexed term (like crawl_id) so one note doc can be
    // upserted/deleted by id. STORED so results can link back to the note.
    builder.add_text_field(FIELD_ANNOTATION_ID, STRING | STORED);
    builder.build()
}
