//! The public input and result types callers hand in and get back.

/// One page from [`SearchIndex::collection_pages`]. `crawl_id` is the WACZ id
/// (`Wacz.id`), which the collection replay manifest emits as `resources[].name`
/// — so it maps directly onto wabac's `item.filename`.
#[derive(Debug, Clone)]
pub struct PageHit {
    pub url: String,
    /// 14-digit capture timestamp as stored (caller converts for wabac).
    pub timestamp: String,
    pub title: String,
    pub crawl_id: String,
}

/// The indexable fields of one page. Borrowed string slices so callers can pass
/// references without cloning; unset fields default to `""` via [`Default`], so
/// adding a field here does not force every call site to change.
#[derive(Debug, Default)]
pub struct Page<'a> {
    pub url: &'a str,
    pub timestamp: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub description: &'a str,
    pub headings: &'a str,
    /// `<meta name=keywords>` content.
    pub keywords: &'a str,
    /// Page author (`<meta name=author>` / `article:author`).
    pub author: &'a str,
    /// Coarse media type: `"html"` or `"pdf"` (empty if unknown).
    pub media_type: &'a str,
    /// Page language tag, e.g. `"en-US"` (stored as its primary subtag).
    pub lang: &'a str,
    /// HTTP response status code, if known.
    pub status: Option<u16>,
    /// Year from the HTTP `Last-Modified` header, if present.
    pub modified_year: Option<u64>,
    /// The WACZ this page came from (id and display name).
    pub crawl_id: &'a str,
    pub crawl_name: &'a str,
    /// The curated collection id (slug) this page's WACZ belongs to.
    pub collection: &'a str,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub doc_type: String,
    /// The WACZ this result came from (id and display name).
    pub crawl_id: String,
    pub crawl_name: String,
    /// The curated collection id (slug) the WACZ belongs to, for `collection:`
    /// filtering and linking to the collection page.
    pub collection: String,
    pub url: String,
    /// Exact host of the page URL (empty for collection results).
    pub domain: String,
    pub timestamp: String,
    pub title: String,
    /// The note author, for `doc_type = "annotation"` results (empty otherwise —
    /// pages rarely carry a usable `<meta author>`, so it's only surfaced for notes).
    pub author: String,
    /// Page description (`<meta description>` / og:description), if any.
    pub description: String,
    pub snippet: String,
    /// A plain leading excerpt of the stored body prefix — the last-resort
    /// snippet fallback when the query matched only deeper than the stored cap
    /// (so there's no highlight) and the page has no description.
    pub body_excerpt: String,
    /// How many captures of this URL matched (1 when there are no repeats). The
    /// result shown is the best-ranked capture; the rest are collapsed into it.
    pub capture_count: usize,
    /// HTTP response status of the capture, when recorded. Mostly `200`; the
    /// value is in flagging the exceptions (archived 404/500/… pages).
    pub status: Option<u16>,
}

/// One page of search results plus the facet counts and total match count for
/// the whole query (not just this page).
#[derive(Debug, Clone)]
pub struct SearchResponse {
    /// Total number of distinct results (URLs grouped) across all pages.
    pub total_hits: usize,
    /// Whether more captures matched than were scanned for grouping, so
    /// `total_hits` is a floor and deep pages may be incomplete.
    pub capped: bool,
    /// The requested page of results.
    pub results: Vec<SearchResult>,
    /// Facet counts per dimension, in display order.
    pub facets: Vec<FacetGroup>,
    /// Result counts per crawl month, oldest first (the results timeline).
    pub timeline: Vec<TimelineBucket>,
}

/// One month's slice of the results timeline.
#[derive(Debug, Clone)]
pub struct TimelineBucket {
    /// Crawl month as `YYYYMM` (e.g. `202503`).
    pub ym: u64,
    pub count: u64,
}

/// The counts for one facet dimension (e.g. "Site"), highest count first.
#[derive(Debug, Clone)]
pub struct FacetGroup {
    /// The index field name (e.g. `domain`), used to build `field:value` refine links.
    pub field: String,
    /// Human label for the dimension (e.g. `Site`).
    pub label: String,
    pub buckets: Vec<FacetBucket>,
}

/// One value within a facet dimension and how many results carry it.
#[derive(Debug, Clone)]
pub struct FacetBucket {
    pub value: String,
    pub count: u64,
}

/// What a scoped facet overview ([`SearchIndex::facet_overview_scoped`]) is
/// restricted to.
pub enum FacetScope<'a> {
    /// A curated collection, by its id/slug (the `collection` field).
    Collection(&'a str),
    /// A single crawl/WACZ, by its id (the `crawl_id` field).
    Crawl(&'a str),
}

/// Per-field stored-text sizes from [`SearchIndex::stored_field_sizes`].
#[derive(Debug, Clone)]
pub struct StoredFieldStats {
    /// How many live docs were scanned to produce these totals.
    pub scanned: usize,
    /// `(field name, total uncompressed bytes, doc count)`, largest field first.
    pub fields: Vec<(String, u64, u64)>,
}
