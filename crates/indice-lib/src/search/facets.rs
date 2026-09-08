//! Facet dimensions and the `field:value` filter vocabulary, plus the Tantivy
//! aggregations behind the sidebar counts and the results timeline.

use tantivy::aggregation::agg_req::Aggregations;

use super::*;

/// The sidebar facet dimensions, in display order: `(index field, label)`. The
/// index field name doubles as the `field:value` filter name (e.g. `domain:`),
/// so a facet value links straight to a refine query.
const FACET_DIMENSIONS: [(&str, &str); 5] = [
    (FIELD_COLLECTION, "Collection"),
    (FIELD_YEAR, "Year"),
    (FIELD_SITE, "Site"),
    (FIELD_MEDIA_TYPE, "Type"),
    (FIELD_LANG, "Language"),
];

/// Filterable `field:value` fields that aren't sidebar facets: `month` (the
/// timeline) and `domain` (exact host — the Site facet uses the registrable
/// domain instead), with labels for their active-filter chips.
const EXTRA_FILTERS: [(&str, &str); 5] = [
    (FIELD_MONTH, "Month"),
    (FIELD_DOMAIN, "Host"),
    (FIELD_STATUS, "Status"),
    (FIELD_MODIFIED, "Modified"),
    // Scopes a search to a single crawl (WACZ). `crawl` is a friendly alias for
    // the internal `crawl_id` field (see `rewrite_crawl_alias`); its value is
    // an opaque WACZ id, so the server resolves it to the crawl's name for the
    // active-filter chip.
    (FILTER_CRAWL, "Crawl"),
];

/// User-facing filter name that scopes a search to a single crawl - a friendly
/// alias for the internal [`FIELD_CRAWL_ID`] field (a crawl is one WACZ, and
/// `crawl_id` would be both internal jargon and misleading in the UI).
const FILTER_CRAWL: &str = "crawl";

/// Rewrite the `crawl:` filter alias to the real `crawl_id:` field so the
/// query parser resolves it. Token-level: only a whole `crawl:<value>` token is
/// rewritten (a bare word `crawl` in the query text is left alone). Crawl ids are
/// simple tokens, so no range/quote handling is needed.
pub(super) fn rewrite_crawl_alias(query_str: &str) -> String {
    query_str
        .split_whitespace()
        .map(|tok| match tok.strip_prefix("crawl:") {
            Some(value) => format!("{FIELD_CRAWL_ID}:{value}"),
            None => tok.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `field` can be used as a `field:value` filter: a sidebar facet
/// dimension or one of the extra filter fields. The single source of truth the
/// server uses to recognize active filters, so the two can't drift.
pub fn is_filter_field(field: &str) -> bool {
    FACET_DIMENSIONS
        .iter()
        .chain(EXTRA_FILTERS.iter())
        .any(|(f, _)| *f == field)
}

/// The human label for a filterable field (for active-filter chips), sharing
/// the facet labels above so they stay in sync.
pub fn filter_label(field: &str) -> &'static str {
    FACET_DIMENSIONS
        .iter()
        .chain(EXTRA_FILTERS.iter())
        .find(|(f, _)| *f == field)
        .map(|(_, label)| *label)
        .unwrap_or("Filter")
}

/// Max buckets requested per facet dimension. A dimension with more distinct
/// values (e.g. many hosts under `domain`) is silently truncated to its top
/// `FACET_SIZE` by count — the sidebar shows only the busiest values, and the
/// terms result's `sum_other_doc_count` (the rest) is discarded.
const FACET_SIZE: u32 = 50;

/// Max month buckets for the timeline (~10 years); older months beyond this are
/// dropped from the histogram.
const TIMELINE_SIZE: u32 = 120;

/// Build the terms-aggregation request: one bucket set per facet dimension plus
/// a month bucket set for the timeline. Deserialized from JSON because
/// [`Aggregations`] is a serde type and the JSON form is far more legible than
/// the nested builder structs.
pub(super) fn facet_aggregations() -> Aggregations {
    let mut req = serde_json::Map::new();
    for (field, _label) in FACET_DIMENSIONS {
        req.insert(
            field.to_string(),
            serde_json::json!({ "terms": { "field": field, "size": FACET_SIZE } }),
        );
    }
    req.insert(
        FIELD_MONTH.to_string(),
        serde_json::json!({ "terms": { "field": FIELD_MONTH, "size": TIMELINE_SIZE } }),
    );
    // The request is well-formed by construction, so this never fails.
    serde_json::from_value(serde_json::Value::Object(req))
        .expect("facet aggregation request is valid")
}

/// Extract the month buckets from the aggregation results as a timeline sorted
/// oldest-first. Terms aggregations sort by count, so we re-sort chronologically.
pub(super) fn timeline_from_aggregations(value: &serde_json::Value) -> Vec<TimelineBucket> {
    let Some(buckets) = value
        .get(FIELD_MONTH)
        .and_then(|d| d.get("buckets"))
        .and_then(|b| b.as_array())
    else {
        return Vec::new();
    };
    let mut out: Vec<TimelineBucket> = buckets
        .iter()
        .filter_map(|b| {
            let count = b.get("doc_count")?.as_u64()?;
            let ym = b.get("key")?.as_f64()? as u64;
            (ym > 0).then_some(TimelineBucket { ym, count })
        })
        .collect();
    out.sort_by_key(|t| t.ym);
    out
}

/// Convert Tantivy's aggregation results into ordered [`FacetGroup`]s. Empty
/// values (e.g. the blank domain/lang of collection-level docs) are dropped.
/// `value` is the aggregation results already serialized to JSON (the terms
/// result is `{buckets:[{key,doc_count}]}`, simpler to read than the internal
/// bucket enums). Serializing once and passing it in avoids re-serializing for
/// the timeline.
pub(super) fn facets_from_aggregations(value: &serde_json::Value) -> Vec<FacetGroup> {
    let mut groups = Vec::new();
    for (field, label) in FACET_DIMENSIONS {
        let Some(buckets) = value
            .get(field)
            .and_then(|d| d.get("buckets"))
            .and_then(|b| b.as_array())
        else {
            continue;
        };
        let items: Vec<FacetBucket> = buckets
            .iter()
            .filter_map(|b| {
                let count = b.get("doc_count")?.as_u64()?;
                let value = match b.get("key")? {
                    serde_json::Value::String(s) => s.clone(),
                    // Numeric keys (year) come back as floats; show them as ints.
                    serde_json::Value::Number(n) => (n.as_f64()? as i64).to_string(),
                    _ => return None,
                };
                (!value.is_empty()).then_some(FacetBucket { value, count })
            })
            .collect();
        if !items.is_empty() {
            groups.push(FacetGroup {
                field: field.to_string(),
                label: label.to_string(),
                buckets: items,
            });
        }
    }
    groups
}
