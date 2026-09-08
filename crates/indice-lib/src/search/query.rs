//! The read path: full-text search, facet overviews, and the collection page
//! listing — a second `impl SearchIndex` block alongside the write path in
//! `mod.rs`.

use anyhow::Result;
use tantivy::aggregation::{AggContextParams, AggregationCollector};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::{TantivyDocument, Term};

use super::*;

/// How much more a title match counts than a body/url match when ranking.
const TITLE_BOOST: tantivy::Score = 3.0;

/// Upper bound on captures scanned per query for URL grouping (field collapsing
/// has no native Tantivy support, so we group over the top-scored window). When
/// a query matches more captures than this, grouping and `total_hits` cover only
/// the top `CANDIDATE_CAP` and [`SearchResponse::capped`] is set. Two cost
/// notes: every query reads this many stored docs (to read each candidate's URL
/// for grouping), and facet/timeline counts are unaffected by this bound — they
/// come from the aggregation, which is exact over the full match set.
const CANDIDATE_CAP: usize = 1000;

/// Headings rank above body text but below the title.
const HEADINGS_BOOST: tantivy::Score = 2.0;

/// The longest plain leading excerpt used as a last-resort snippet fallback.
const LEADING_EXCERPT_CHARS: usize = 300;

/// A plain, whitespace-collapsed leading excerpt of `s` (first
/// [`LEADING_EXCERPT_CHARS`] chars), for the last-resort snippet fallback.
fn leading_excerpt(s: &str) -> String {
    let mut out: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > LEADING_EXCERPT_CHARS {
        let end = out
            .char_indices()
            .nth(LEADING_EXCERPT_CHARS)
            .map(|(i, _)| i)
            .unwrap_or(out.len());
        out.truncate(end);
        out.push('…');
    }
    out
}

fn get_text(doc: &TantivyDocument, field: tantivy::schema::Field) -> String {
    doc.get_first(field)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn get_u64(doc: &TantivyDocument, field: tantivy::schema::Field) -> Option<u64> {
    doc.get_first(field).and_then(|v| v.as_u64())
}

impl SearchIndex {
    /// Facet counts across the whole archive (a match-all query), for homepage
    /// browse entry points. Runs only the aggregation — no result fetching or
    /// URL grouping — so it is cheap.
    pub fn facet_overview(&self) -> Result<Vec<FacetGroup>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        let agg_collector =
            AggregationCollector::from_aggs(facet_aggregations(), AggContextParams::default());
        let agg_results = searcher.search(&tantivy::query::AllQuery, &agg_collector)?;
        let agg_json = serde_json::to_value(&agg_results).unwrap_or_default();
        Ok(facets_from_aggregations(&agg_json))
    }

    /// Facet counts restricted to one collection or one crawl, for the scoped
    /// facet overview on detail pages. Same aggregation as [`facet_overview`], but
    /// run over a term query on the scope field instead of match-all — still just
    /// the aggregation (no result fetch), so still cheap.
    pub fn facet_overview_scoped(&self, scope: FacetScope) -> Result<Vec<FacetGroup>> {
        let schema = self.index.schema();
        let (field, value) = match scope {
            FacetScope::Collection(v) => (schema.get_field(FIELD_COLLECTION).unwrap(), v),
            FacetScope::Crawl(v) => (schema.get_field(FIELD_CRAWL_ID).unwrap(), v),
        };
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        let query = tantivy::query::TermQuery::new(
            Term::from_field_text(field, value),
            IndexRecordOption::Basic,
        );
        let agg_collector =
            AggregationCollector::from_aggs(facet_aggregations(), AggContextParams::default());
        let agg_results = searcher.search(&query, &agg_collector)?;
        let agg_json = serde_json::to_value(&agg_results).unwrap_or_default();
        Ok(facets_from_aggregations(&agg_json))
    }

    /// Search the top `limit` results by relevance. A thin wrapper over
    /// [`search_faceted`](Self::search_faceted) that returns only the hits (no
    /// facet counts, no pagination); kept for callers that don't need them.
    pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchResult>> {
        Ok(self.search_faceted(query_str, limit, 0)?.results)
    }

    /// Search with facet counts and pagination. Returns one page of results
    /// (`limit` hits starting at `offset`), the total number of matches, and
    /// facet buckets (counts per value) for each facet dimension, all computed
    /// from the same query in a single pass.
    pub fn search_faceted(
        &self,
        query_str: &str,
        limit: usize,
        offset: usize,
    ) -> Result<SearchResponse> {
        // Map the `crawl:` filter alias onto its real `crawl_id` field before
        // parsing (the query parser resolves against schema field names).
        let query_str = &rewrite_crawl_alias(query_str);
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        let schema = self.index.schema();

        let title_f = schema.get_field(FIELD_TITLE).unwrap();
        let body_f = schema.get_field(FIELD_BODY).unwrap();
        let doc_type_f = schema.get_field(FIELD_DOC_TYPE).unwrap();
        let coll_id_f = schema.get_field(FIELD_CRAWL_ID).unwrap();
        let coll_name_f = schema.get_field(FIELD_CRAWL_NAME).unwrap();
        let url_f = schema.get_field(FIELD_URL).unwrap();
        let ts_f = schema.get_field(FIELD_TS).unwrap();
        let domain_f = schema.get_field(FIELD_DOMAIN).unwrap();
        let description_f = schema.get_field(FIELD_DESCRIPTION).unwrap();
        let headings_f = schema.get_field(FIELD_HEADINGS).unwrap();
        let keywords_f = schema.get_field(FIELD_KEYWORDS).unwrap();
        let author_f = schema.get_field(FIELD_AUTHOR).unwrap();
        let url_tokens_f = schema.get_field(FIELD_URL_TOKENS).unwrap();
        let collection_f = schema.get_field(FIELD_COLLECTION).unwrap();
        let status_f = schema.get_field(FIELD_STATUS).unwrap();

        // Bare words search the title, headings, body, description, keywords,
        // author, and URL words. Other fields (domain:, url:, title:, author:)
        // are also reachable by explicit `field:` syntax.
        let mut query_parser = QueryParser::for_index(
            &self.index,
            vec![
                title_f,
                headings_f,
                body_f,
                description_f,
                keywords_f,
                author_f,
                url_tokens_f,
            ],
        );
        // Require all terms by default (`climate change` means both), which
        // matches what people expect from a search box more than OR does.
        query_parser.set_conjunction_by_default();
        // A title match is the strongest relevance signal; headings next.
        query_parser.set_field_boost(title_f, TITLE_BOOST);
        query_parser.set_field_boost(headings_f, HEADINGS_BOOST);
        // Parse leniently: a malformed query (stray quote, bad `field:`) yields
        // a best-effort query instead of a hard error, so the search box never
        // 500s while someone is experimenting with syntax.
        let (query, _errors) = query_parser.parse_query_lenient(query_str);

        // One pass over the query: a bounded window of top-scored captures (for
        // grouping), the total capture count, and the facet counts (a terms
        // aggregation per dimension over the fast fields).
        let top_collector = TopDocs::with_limit(CANDIDATE_CAP).order_by_score();
        let agg_collector =
            AggregationCollector::from_aggs(facet_aggregations(), AggContextParams::default());
        let (candidates, total_captures, agg_results) =
            searcher.search(&query, &(top_collector, Count, agg_collector))?;

        // Collapse repeat captures of the same URL: walking the candidates in
        // score order, the first (best-ranked) capture of a URL becomes the
        // result and later captures just bump its count. Collection-level docs
        // and any capture with no URL are never merged. Grouping is over the
        // top `CANDIDATE_CAP` captures, so `capped` flags when more matched.
        struct Group {
            addr: tantivy::DocAddress,
            captures: usize,
        }
        let mut groups: Vec<Group> = Vec::new();
        let mut by_url: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (_score, addr) in &candidates {
            let doc: TantivyDocument = searcher.doc(*addr)?;
            let url = get_text(&doc, url_f);
            let is_page = get_text(&doc, doc_type_f) == "page";
            if is_page && !url.is_empty() {
                if let Some(&gi) = by_url.get(&url) {
                    groups[gi].captures += 1;
                    continue;
                }
                by_url.insert(url, groups.len());
            }
            groups.push(Group {
                addr: *addr,
                captures: 1,
            });
        }
        let total_hits = groups.len();
        let capped = total_captures > CANDIDATE_CAP;

        // Generate snippets only for the requested page of groups (snippet
        // generation re-analyzes text, so we defer it past grouping).
        let mut snippet_gen = SnippetGenerator::create(&searcher, &query, body_f)?;
        // Tantivy's default snippet window is 150 chars; widen it for more
        // context around the matched terms in search results.
        snippet_gen.set_max_num_chars(350);

        let body_snip_f = schema.get_field(FIELD_BODY_SNIP).unwrap();
        let mut results = Vec::new();
        for g in groups.iter().skip(offset).take(limit) {
            let doc: TantivyDocument = searcher.doc(g.addr)?;
            // Snippets highlight the stored capped body prefix (the full body is
            // indexed but not stored). A match deeper than the cap yields an empty
            // highlight; the leading excerpt is a last-resort fallback for that.
            let body_snip = get_text(&doc, body_snip_f);
            let snippet = snippet_gen.snippet(&body_snip);
            results.push(SearchResult {
                doc_type: get_text(&doc, doc_type_f),
                crawl_id: get_text(&doc, coll_id_f),
                crawl_name: get_text(&doc, coll_name_f),
                collection: get_text(&doc, collection_f),
                url: get_text(&doc, url_f),
                domain: get_text(&doc, domain_f),
                timestamp: get_text(&doc, ts_f),
                title: get_text(&doc, title_f),
                author: get_text(&doc, author_f),
                description: get_text(&doc, description_f),
                snippet: snippet.to_html(),
                body_excerpt: leading_excerpt(&body_snip),
                capture_count: g.captures,
                status: get_u64(&doc, status_f).map(|s| s as u16),
            });
        }

        // Facets and the timeline are aggregation-derived: they count *captures*
        // and are *exact* over the whole match set. total_hits counts *distinct
        // URLs* and is bounded by CANDIDATE_CAP. So a facet's count is generally
        // higher than the number of grouped results it would yield — the two
        // measure different things on purpose. Serialize the aggregation once
        // and reuse it for both extractors.
        let agg_json = serde_json::to_value(&agg_results).unwrap_or_default();
        Ok(SearchResponse {
            total_hits,
            capped,
            results,
            facets: facets_from_aggregations(&agg_json),
            timeline: timeline_from_aggregations(&agg_json),
        })
    }

    /// Page documents within one collection, for the wabac `pagesQueryUrl`
    /// endpoint that backs multi-WACZ collection replay (the pages sidebar and
    /// on-demand URL→WACZ resolution). Three modes:
    /// - `url = Some(u)`: exact-match resolution — which member(s) hold that URL
    ///   (a direct term query on the raw `url` field; robust, no grouping/cap).
    /// - `search = Some(q)`: free-text page search within the collection.
    /// - both `None`: the collection's page list.
    ///
    /// Returns `(total_matches, one page of hits)`. Unlike `search_faceted`, this
    /// is not URL-collapsed or candidate-capped, so `total` is exact and deep
    /// pagination is complete — a page listing needs every page, not the top N.
    pub fn collection_pages(
        &self,
        collection_id: &str,
        url: Option<&str>,
        search: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, Vec<PageHit>)> {
        let schema = self.index.schema();
        let collection_f = schema.get_field(FIELD_COLLECTION).unwrap();
        let doc_type_f = schema.get_field(FIELD_DOC_TYPE).unwrap();
        let url_f = schema.get_field(FIELD_URL).unwrap();
        let ts_f = schema.get_field(FIELD_TS).unwrap();
        let title_f = schema.get_field(FIELD_TITLE).unwrap();
        let crawl_id_f = schema.get_field(FIELD_CRAWL_ID).unwrap();
        let status_f = schema.get_field(FIELD_STATUS).unwrap();

        let reader = self.index.reader()?;
        let searcher = reader.searcher();

        // Always scope to page docs in this collection.
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(collection_f, collection_id),
                    IndexRecordOption::Basic,
                )),
            ),
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(doc_type_f, "page"),
                    IndexRecordOption::Basic,
                )),
            ),
        ];
        // Exact-URL resolution: a term query on the raw (STRING) url field.
        if let Some(u) = url {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(url_f, u),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        // Free-text page search: parse over the same text fields as the main
        // search, leniently (a stray character never 500s the sidebar).
        if let Some(q) = search.map(str::trim).filter(|q| !q.is_empty()) {
            let body_f = schema.get_field(FIELD_BODY).unwrap();
            let headings_f = schema.get_field(FIELD_HEADINGS).unwrap();
            let description_f = schema.get_field(FIELD_DESCRIPTION).unwrap();
            let keywords_f = schema.get_field(FIELD_KEYWORDS).unwrap();
            let author_f = schema.get_field(FIELD_AUTHOR).unwrap();
            let url_tokens_f = schema.get_field(FIELD_URL_TOKENS).unwrap();
            let mut qp = QueryParser::for_index(
                &self.index,
                vec![
                    title_f,
                    headings_f,
                    body_f,
                    description_f,
                    keywords_f,
                    author_f,
                    url_tokens_f,
                ],
            );
            qp.set_conjunction_by_default();
            let (text_query, _errors) = qp.parse_query_lenient(q);
            clauses.push((Occur::Must, text_query));
        }

        let query = BooleanQuery::new(clauses);
        let want = offset.saturating_add(limit).max(1);
        let (total, top) =
            searcher.search(&query, &(Count, TopDocs::with_limit(want).order_by_score()))?;

        // Prefer good captures: rank HTTP 2xx ahead of everything else
        // (errors/redirects/no-recorded-status). This matters for URL→WACZ
        // resolution — when several crawls captured the same URL and one holds a
        // real 200 while another holds an archived 404, wabac must land on the
        // 200. wabac's resolve call carries no timestamp, so we can't pick by
        // time; a stable 2xx-first sort keeps the index's relevance order within
        // each rank. (For a paginated page-list this orders within the page,
        // which is harmless.)
        let mut ranked: Vec<(u8, PageHit)> = Vec::new();
        for (_score, addr) in top.into_iter().skip(offset).take(limit) {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let rank = match get_u64(&doc, status_f) {
                Some(s) if (200..300).contains(&s) => 0,
                _ => 1,
            };
            ranked.push((
                rank,
                PageHit {
                    url: get_text(&doc, url_f),
                    timestamp: get_text(&doc, ts_f),
                    title: get_text(&doc, title_f),
                    crawl_id: get_text(&doc, crawl_id_f),
                },
            ));
        }
        ranked.sort_by_key(|(rank, _)| *rank);
        let hits = ranked.into_iter().map(|(_, h)| h).collect();
        Ok((total, hits))
    }
}
