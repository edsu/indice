//! The read path: full-text search, facet overviews, and the collection page
//! listing — a second `impl SearchIndex` block alongside the write path in
//! `mod.rs`.

use anyhow::Result;
use std::ops::Bound;
use tantivy::aggregation::{AggContextParams, AggregationCollector};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, RangeQuery, TermQuery};
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
        let reader = self.reader()?;
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
    pub fn facet_overview_scoped(&self, scope: FacetScope) -> Result<ScopedOverview> {
        let schema = self.index.schema();
        let (field, value) = match scope {
            FacetScope::Collection(v) => (schema.get_field(FIELD_COLLECTION).unwrap(), v),
            FacetScope::Crawl(v) => (schema.get_field(FIELD_CRAWL_ID).unwrap(), v),
        };
        let reader = self.reader()?;
        let searcher = reader.searcher();
        let query = tantivy::query::TermQuery::new(
            Term::from_field_text(field, value),
            IndexRecordOption::Basic,
        );
        let agg_collector =
            AggregationCollector::from_aggs(facet_aggregations(), AggContextParams::default());
        let agg_results = searcher.search(&query, &agg_collector)?;
        let agg_json = serde_json::to_value(&agg_results).unwrap_or_default();
        Ok(ScopedOverview {
            facets: facets_from_aggregations(&agg_json),
            months: timeline_from_aggregations(&agg_json),
        })
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
        let reader = self.reader()?;
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
        let annotation_f = schema.get_field(FIELD_ANNOTATION_ID).unwrap();

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
                annotation_id: get_text(&doc, annotation_f),
                doc_type: get_text(&doc, doc_type_f),
                crawl_id: get_text(&doc, coll_id_f),
                crawl_name: get_text(&doc, coll_name_f),
                collection: get_text(&doc, collection_f),
                url: get_text(&doc, url_f),
                domain: get_text(&doc, domain_f),
                timestamp: get_text(&doc, ts_f),
                title: get_text(&doc, title_f),
                // Sanitized on the way out, not just on the way in: for an
                // annotation hit this is the note's author, and an index built
                // before display names were separated from login identities
                // still has an address stored here. Doing it on read means an
                // upgrade stops the leak without an `indice reindex`.
                author: {
                    let stored = get_text(&doc, author_f);
                    crate::identity::public_display_name(&stored).to_string()
                },
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

    /// Up to `limit` pages of a collection, with their stored text, chosen to
    /// show the collection's range rather than its bulk: the pages at the
    /// URLs in `first` (the seed pages) lead, then pages from each of `sites`
    /// (the collection's sites, most pages first) are dealt round-robin, so a
    /// site with tens of thousands of pages doesn't crowd out the others.
    /// Pages with no text and no description are skipped, and each URL is
    /// taken once.
    ///
    /// Queries each seed URL and each site directly rather than scanning the
    /// collection, so a sample costs the same in a collection of any size, and
    /// a site isn't missed because its pages come late in the index.
    pub fn sample_pages(
        &self,
        collection_id: &str,
        first: &[String],
        sites: &[String],
        limit: usize,
    ) -> Result<Vec<PageSample>> {
        let schema = self.index.schema();
        let f = |name| schema.get_field(name);
        let (collection_f, doc_type_f) = (f(FIELD_COLLECTION)?, f(FIELD_DOC_TYPE)?);
        let (url_f, title_f, ts_f) = (f(FIELD_URL)?, f(FIELD_TITLE)?, f(FIELD_TS)?);
        let (site_f, desc_f, snip_f) = (f(FIELD_SITE)?, f(FIELD_DESCRIPTION)?, f(FIELD_BODY_SNIP)?);
        let status_f = f(FIELD_STATUS)?;
        let searcher = self.reader()?.searcher();

        // Up to `n` pages of this collection that also match `field = value`,
        // leaving out captured error pages (a 404, a redirect): archived
        // faithfully, but they say nothing about what the collection holds.
        // A page with no recorded status is kept. The status is part of the
        // query, not checked afterwards, so a site whose first hits are
        // redirects still gets its `n` readable pages.
        let pages_where = |field, value: &str, n: usize| -> Result<Vec<PageSample>> {
            let term = |field, value: &str| -> Box<dyn Query> {
                Box::new(TermQuery::new(
                    Term::from_field_text(field, value),
                    IndexRecordOption::Basic,
                ))
            };
            let status = |lower: Bound<u64>, upper: Bound<u64>| -> Box<dyn Query> {
                Box::new(RangeQuery::new(
                    lower.map(|s| Term::from_field_u64(status_f, s)),
                    upper.map(|s| Term::from_field_u64(status_f, s)),
                ))
            };
            let query = BooleanQuery::new(vec![
                (Occur::Must, term(collection_f, collection_id)),
                (Occur::Must, term(doc_type_f, "page")),
                (Occur::Must, term(field, value)),
                (
                    Occur::MustNot,
                    status(Bound::Unbounded, Bound::Excluded(200)),
                ),
                (
                    Occur::MustNot,
                    status(Bound::Included(300), Bound::Unbounded),
                ),
            ]);
            let mut out = Vec::new();
            for (_score, addr) in
                searcher.search(&query, &TopDocs::with_limit(n).order_by_score())?
            {
                let doc: TantivyDocument = searcher.doc(addr)?;
                out.push(PageSample {
                    url: get_text(&doc, url_f),
                    title: get_text(&doc, title_f),
                    timestamp: get_text(&doc, ts_f),
                    site: get_text(&doc, site_f),
                    description: get_text(&doc, desc_f),
                    text: get_text(&doc, snip_f),
                });
            }
            Ok(out)
        };
        let readable =
            |p: &PageSample| !(p.text.trim().is_empty() && p.description.trim().is_empty());

        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for url in first {
            if out.len() == limit {
                break;
            }
            if seen.contains(url) {
                continue;
            }
            // Several captures of one URL say nothing new: take one, the
            // first with something to read.
            if let Some(p) = pages_where(url_f, url, 3)?.into_iter().find(readable) {
                seen.insert(p.url.clone());
                out.push(p);
            }
        }

        // Enough per site to fill the sample even if some sites run dry or
        // their pages were already taken as seeds, without reading thousands.
        let per_site = (limit / sites.len().max(1)).max(2) + 2;
        let mut queues: Vec<std::collections::VecDeque<PageSample>> = Vec::new();
        for site in sites {
            queues.push(pages_where(site_f, site, per_site)?.into_iter().collect());
        }
        while out.len() < limit {
            let mut took = false;
            for q in queues.iter_mut() {
                if out.len() == limit {
                    break;
                }
                while let Some(p) = q.pop_front() {
                    if readable(&p) && seen.insert(p.url.clone()) {
                        out.push(p);
                        took = true;
                        break;
                    }
                }
            }
            if !took {
                break;
            }
        }
        Ok(out)
    }

    /// Page documents within `scope`, for the wabac `pagesQueryUrl` endpoint
    /// that backs multi-WACZ replay (the pages sidebar and on-demand URL→WACZ
    /// resolution). Three modes:
    /// - `url = Some(u)`: exact-match resolution — which member(s) hold that URL
    ///   (a direct term query on the raw `url` field; robust, no grouping/cap).
    /// - `search = Some(q)`: free-text page search within the collection.
    /// - both `None`: the collection's page list.
    ///
    /// Returns `(total_matches, one page of hits)`. Unlike `search_faceted`, this
    /// is not URL-collapsed or candidate-capped, so `total` is exact and deep
    /// pagination is complete — a page listing needs every page, not the top N.
    /// The deepest offset `collection_pages` will page to.
    pub const MAX_PAGES_OFFSET: usize = 10_000;

    pub fn pages(
        &self,
        scope: PagesScope,
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

        let reader = self.reader()?;
        let searcher = reader.searcher();

        let term = |field, text: &str| -> Box<dyn Query> {
            Box::new(TermQuery::new(
                Term::from_field_text(field, text),
                IndexRecordOption::Basic,
            ))
        };
        // Always scope to page docs in the collection or crawls. No crawls
        // match nothing: a `BooleanQuery` with no clauses matches no document.
        let in_scope: Box<dyn Query> = match scope {
            PagesScope::Collection(id) => term(collection_f, id),
            PagesScope::Crawls(ids) => Box::new(BooleanQuery::new(
                ids.iter()
                    .map(|id| (Occur::Should, term(crawl_id_f, id)))
                    .collect(),
            )),
        };
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
            (Occur::Must, in_scope),
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

        // Prefer good captures: an HTTP 2xx scores above everything else
        // (errors, redirects, no recorded status). This matters for URL→WACZ
        // resolution: when several crawls captured the same URL and one holds a
        // real 200 while another holds an archived 404, wabac tries the WACZs in
        // the order given and must land on the 200. Its resolve call carries no
        // timestamp, so we can't pick by time. A boost in the query, rather than
        // a sort afterwards, ranks the whole result set, so a 2xx capture is
        // first however many captures there are, not only within one page.
        clauses.push((
            Occur::Should,
            Box::new(tantivy::query::BoostQuery::new(
                Box::new(tantivy::query::ConstScoreQuery::new(
                    Box::new(RangeQuery::new(
                        Bound::Included(Term::from_field_u64(status_f, 200)),
                        Bound::Excluded(Term::from_field_u64(status_f, 300)),
                    )),
                    1.0,
                )),
                1000.0,
            )),
        ));
        let query = BooleanQuery::new(clauses);
        // Deep pages of a page list are not worth their cost: collecting the
        // top `offset + limit` hits allocates for all of them.
        if offset > Self::MAX_PAGES_OFFSET {
            let total = searcher.search(&query, &Count)?;
            return Ok((total, Vec::new()));
        }
        let want = (offset + limit).max(1);
        let (total, top) =
            searcher.search(&query, &(Count, TopDocs::with_limit(want).order_by_score()))?;

        let mut hits = Vec::new();
        for (_score, addr) in top.into_iter().skip(offset).take(limit) {
            let doc: TantivyDocument = searcher.doc(addr)?;
            hits.push(PageHit {
                url: get_text(&doc, url_f),
                timestamp: get_text(&doc, ts_f),
                title: get_text(&doc, title_f),
                crawl_id: get_text(&doc, crawl_id_f),
            });
        }
        Ok((total, hits))
    }
}
