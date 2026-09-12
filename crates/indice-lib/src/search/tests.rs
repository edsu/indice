use super::parse::DETECT_SAMPLE_BYTES;
use super::*;
use tempfile::TempDir;

/// Build a page with the common fields; unset fields default to empty.
fn page<'a>(url: &'a str, title: &'a str, body: &'a str, cid: &'a str, cname: &'a str) -> Page<'a> {
    Page {
        url,
        title,
        body,
        crawl_id: cid,
        crawl_name: cname,
        ..Default::default()
    }
}

/// A page with a given URL and timestamp; fixed title/body for date tests.
fn page_ts<'a>(url: &'a str, ts: &'a str) -> Page<'a> {
    Page {
        url,
        timestamp: ts,
        title: "T",
        body: "shared content",
        crawl_id: "c1",
        crawl_name: "C1",
        ..Default::default()
    }
}

#[test]
fn extract_text_from_html() {
    let html = b"<html><head><title>Hello World</title></head><body><p>Some text</p><script>var x=1;</script></body></html>";
    let t = extract_html_text(html);
    assert_eq!(t.title, "Hello World");
    assert!(t.body.contains("Some text"), "body: {}", t.body);
    assert!(
        !t.body.contains("var x"),
        "should exclude script content: {}",
        t.body
    );
}

#[test]
fn extract_description_and_headings_from_html() {
    let html = br#"<html><head><title>T</title>
        <meta name="description" content="A concise summary">
        <meta property="og:description" content="OG fallback"></head>
        <body><h1>Main Heading</h1><h2>Sub Heading</h2><p>Body.</p></body></html>"#;
    let t = extract_html_text(html);
    assert_eq!(t.description, "A concise summary");
    assert!(
        t.headings.contains("Main Heading"),
        "headings: {}",
        t.headings
    );
    assert!(
        t.headings.contains("Sub Heading"),
        "headings: {}",
        t.headings
    );
}

#[test]
fn extract_keywords_and_author_from_meta() {
    let html = br#"<html><head><title>T</title>
        <meta name="keywords" content="climate, policy, marmots">
        <meta name="author" content="Ada Lovelace"></head>
        <body>x</body></html>"#;
    let t = extract_html_text(html);
    assert!(t.keywords.contains("marmots"), "keywords: {}", t.keywords);
    assert_eq!(t.author, "Ada Lovelace");
}

#[test]
fn author_falls_back_to_article_author() {
    let html = br#"<html><head>
        <meta property="article:author" content="Grace Hopper"></head>
        <body>x</body></html>"#;
    let t = extract_html_text(html);
    assert_eq!(t.author, "Grace Hopper");
}

#[test]
fn optimize_merges_segments_and_preserves_search() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    // NOTE: this pre-sets NoMergePolicy to build a fragmented index, which
    // also means it does NOT cover optimize's own NoMergePolicy guard
    // against the auto-merger race (the writer is already NoMerge here). That
    // race needs the default policy + many segments to reproduce and is hard
    // to trigger deterministically, so it's verified by reasoning + the live
    // 1557→8 run, not by this test.
    idx.disable_auto_merge(); // each commit -> its own segment
    for i in 0..4 {
        let url = format!("https://ex.com/{i}");
        let cid = format!("c{i}");
        idx.index_page(&Page {
            url: &url,
            title: "Snowfall",
            body: "snow in the mountains",
            crawl_id: &cid,
            crawl_name: "C",
            ..Default::default()
        })
        .unwrap();
        idx.commit().unwrap();
    }
    let before = idx.segment_count().unwrap();
    assert!(
        before >= 4,
        "expected a fragmented index, got {before} segments"
    );

    let (b, after) = idx.optimize(1, crate::index::no_progress()).unwrap();
    assert_eq!(b, before);
    assert_eq!(after, 1, "should compact to a single segment");
    assert_eq!(idx.segment_count().unwrap(), 1);
    // All four docs survive the merge and are still searchable.
    assert_eq!(idx.search("snow", 10).unwrap().len(), 4);
}

#[test]
fn annotation_is_searchable_and_upsertable_by_id() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    // A page and a note on it.
    idx.index_page(&Page {
        url: "https://ex.com/a",
        title: "Alpha",
        body: "hello world",
        collection: "coll",
        crawl_id: "c1",
        crawl_name: "C",
        ..Default::default()
    })
    .unwrap();
    idx.index_annotation(
        "urn:indice:annotation:aaa",
        "coll",
        "https://ex.com/a",
        "20240101000000",
        "grace",
        "a note about widgets",
    )
    .unwrap();
    idx.commit().unwrap();

    // The note matches on its body text and comes back as an annotation doc,
    // distinct from the page (they share a URL but don't collapse together).
    let hits = idx.search("widgets", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].doc_type, "annotation");
    assert_eq!(hits[0].author, "grace");
    assert_eq!(hits[0].url, "https://ex.com/a");
    assert_eq!(hits[0].collection, "coll");

    // Upsert by id: delete then re-add replaces the note text in place, no
    // duplicate left behind.
    idx.delete_annotation_doc("urn:indice:annotation:aaa");
    idx.index_annotation(
        "urn:indice:annotation:aaa",
        "coll",
        "https://ex.com/a",
        "20240101000000",
        "grace",
        "now about gadgets",
    )
    .unwrap();
    idx.commit().unwrap();
    assert_eq!(idx.search("widgets", 10).unwrap().len(), 0);
    assert_eq!(idx.search("gadgets", 10).unwrap().len(), 1);

    // Deleting the note drops it from search; the page is untouched.
    idx.delete_annotation_doc("urn:indice:annotation:aaa");
    idx.commit().unwrap();
    assert_eq!(idx.search("gadgets", 10).unwrap().len(), 0);
    assert_eq!(idx.search("hello", 10).unwrap().len(), 1);
}

#[test]
fn optimize_sweeps_orphaned_segment_files() {
    // Files left behind by a hard-killed (Ctrl-C) merge/ingest: `<uuid>.<ext>`
    // segment files that aren't referenced by any live segment and were never
    // registered with Tantivy's GC, so they'd otherwise linger forever.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    let mut idx = SearchIndex::open(&dir).unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/a",
        title: "Alpha",
        body: "hello world",
        crawl_id: "c1",
        crawl_name: "C",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    // Plant orphans (32-hex uuid that matches no live segment) + a file that
    // must be preserved (not a segment name).
    let orphans = [
        "deadbeefdeadbeefdeadbeefdeadbeef.pos",
        "deadbeefdeadbeefdeadbeefdeadbeef.store",
        "0123456789abcdef0123456789abcdef.1.del",
    ];
    for name in orphans {
        std::fs::write(dir.join(name), b"garbage").unwrap();
    }
    std::fs::write(dir.join("keep-me.txt"), b"not a segment").unwrap();

    idx.optimize(1, crate::index::no_progress()).unwrap();

    for name in orphans {
        assert!(
            !dir.join(name).exists(),
            "orphaned segment file should be swept: {name}"
        );
    }
    assert!(
        dir.join("keep-me.txt").exists(),
        "non-segment files must be left alone"
    );
    assert!(dir.join("meta.json").exists(), "meta.json must survive");
    // The live doc is intact and searchable — the sweep didn't touch it.
    assert_eq!(idx.search("hello", 10).unwrap().len(), 1);
}

#[test]
fn optimize_expunges_deletes_at_default_target() {
    // Deleting a crawl leaves tombstones whose data stays on disk until the
    // segment is rewritten. With few segments the count-based merge does
    // nothing at the default target, so the expunge pass must reclaim it.
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    for i in 0..100 {
        let crawl = if i < 50 { "keep" } else { "drop" };
        let url = format!("https://ex.com/{i}");
        idx.index_page(&Page {
            url: &url,
            title: "T",
            body: "hello world",
            crawl_id: crawl,
            crawl_name: "C",
            ..Default::default()
        })
        .unwrap();
    }
    idx.commit().unwrap();
    idx.delete_crawl_docs("drop");
    idx.commit().unwrap();

    let deleted_before: u32 = idx
        .index
        .searchable_segment_metas()
        .unwrap()
        .iter()
        .map(|m| m.num_deleted_docs())
        .sum();
    assert!(deleted_before > 0, "expected tombstones before optimize");
    assert!(
        idx.segment_count().unwrap() <= 8,
        "under the default target, so the count-based merge alone would reclaim nothing"
    );

    idx.optimize(8, crate::index::no_progress()).unwrap();

    let deleted_after: u32 = idx
        .index
        .searchable_segment_metas()
        .unwrap()
        .iter()
        .map(|m| m.num_deleted_docs())
        .sum();
    assert_eq!(deleted_after, 0, "deletes should be expunged");
    assert_eq!(idx.num_docs().unwrap(), 50, "only the kept crawl remains");
    assert_eq!(idx.search("hello", 100).unwrap().len(), 50);
}

#[test]
fn stored_field_sizes_shows_body_snip_dominant_and_omits_full_body() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/a",
        title: "Title",
        body: &"word ".repeat(500),
        description: "a short description",
        crawl_id: "c1",
        crawl_name: "Crawl",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    let fs = idx.stored_field_sizes(0).unwrap();
    assert_eq!(fs.scanned, 1);
    // The capped body snippet is the largest stored field.
    assert_eq!(fs.fields[0].0, "body_snip");
    let names: Vec<&str> = fs.fields.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"title") && names.contains(&"url"));
    // The full `body` is indexed but NOT stored, so it never shows here.
    assert!(!names.contains(&"body"), "full body must not be stored");
}

#[test]
fn optimize_respects_target_and_is_a_noop_when_already_small() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.disable_auto_merge();
    for i in 0..5 {
        let cid = format!("c{i}");
        idx.index_page(&Page {
            url: "https://ex.com/x",
            title: "T",
            body: "body",
            crawl_id: &cid,
            crawl_name: "C",
            ..Default::default()
        })
        .unwrap();
        idx.commit().unwrap();
    }
    assert!(idx.segment_count().unwrap() >= 5);
    // Compact toward 2, then optimizing again is a no-op (already ≤ target).
    let (_, after) = idx.optimize(2, crate::index::no_progress()).unwrap();
    assert_eq!(after, 2);
    let (before2, after2) = idx.optimize(2, crate::index::no_progress()).unwrap();
    assert_eq!(
        (before2, after2),
        (2, 2),
        "already-compact index is untouched"
    );
}

#[test]
fn body_past_the_stored_cap_is_searchable_but_not_highlighted() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    // A body longer than the stored cap: a unique marker near the start (in
    // the stored prefix) and another only *past* the cap.
    let filler = "lorem ipsum dolor sit amet ".repeat(1500); // ~40 KB > cap
    assert!(filler.len() > crate::config::DEFAULT_STORED_BODY_CAP_BYTES);
    let body = format!("zzmarkerhead {filler} zzmarkerdeep");
    idx.index_page(&Page {
        url: "https://ex.com/long",
        title: "long page",
        body: &body,
        crawl_id: "c1",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    // The deep term is still findable — the full body is indexed even though
    // only a capped prefix is stored (recall is unaffected by the cap).
    let deep = idx.search("zzmarkerdeep", 10).unwrap();
    assert_eq!(
        deep.len(),
        1,
        "a term past the stored cap is still findable"
    );
    assert!(
        !deep[0].snippet.contains("zzmarkerdeep"),
        "a match past the stored cap can't be highlighted"
    );
    assert!(
        !deep[0].body_excerpt.is_empty(),
        "a leading-excerpt fallback is available for a past-cap hit"
    );

    // A term inside the stored prefix is highlighted as usual.
    let head = idx.search("zzmarkerhead", 10).unwrap();
    assert_eq!(head.len(), 1);
    assert!(
        head[0].snippet.contains("<b>zzmarkerhead</b>"),
        "a match within the stored prefix is highlighted; got: {}",
        head[0].snippet
    );
}

#[test]
fn stored_body_cap_setting_bounds_snippets() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.set_stored_body_cap(64); // a deliberately tiny cap (the frugality knob)
    let body = format!("zzhead {} zztail", "x ".repeat(100)); // ~200 B > 64
    idx.index_page(&Page {
        url: "https://ex.com/1",
        title: "t",
        body: &body,
        crawl_id: "c1",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    // Both terms are findable — the full body is indexed regardless of cap.
    assert_eq!(
        idx.search("zztail", 10).unwrap().len(),
        1,
        "recall past cap"
    );
    let tail = idx.search("zztail", 10).unwrap();
    assert!(
        !tail[0].snippet.contains("zztail"),
        "a term past the (tiny) stored cap isn't highlighted"
    );
    let head = idx.search("zzhead", 10).unwrap();
    assert!(
        head[0].snippet.contains("<b>zzhead</b>"),
        "a term within the cap is highlighted"
    );
}

#[test]
fn keywords_and_author_are_searchable() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/a",
        title: "Plain",
        body: "ordinary body",
        keywords: "marmots rodentia",
        author: "Ada Lovelace",
        crawl_id: "c1",
        crawl_name: "C1",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    assert_eq!(
        idx.search("rodentia", 10).unwrap().len(),
        1,
        "keywords searchable"
    );
    assert_eq!(
        idx.search("Lovelace", 10).unwrap().len(),
        1,
        "author searchable by bare word"
    );
    assert_eq!(
        idx.search("author:Lovelace", 10).unwrap().len(),
        1,
        "author: field query"
    );
}

#[test]
fn search_results_carry_http_status() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/ok",
        title: "ok page",
        status: Some(200),
        crawl_id: "c1",
        ..Default::default()
    })
    .unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/missing",
        title: "gone page",
        status: Some(404),
        crawl_id: "c1",
        ..Default::default()
    })
    .unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/unknown",
        title: "statusless page",
        crawl_id: "c1",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    let by_url = |q: &str| idx.search(q, 10).unwrap().into_iter().next().unwrap();
    assert_eq!(by_url("ok").status, Some(200));
    assert_eq!(by_url("gone").status, Some(404), "non-200 flows through");
    assert_eq!(
        by_url("statusless").status,
        None,
        "a page with no recorded status stays None"
    );
}

#[test]
fn collection_pages_url_prefers_2xx_capture() {
    // The same URL captured by two crawls: an archived 404 (indexed first,
    // so it would win on relevance/doc order) and the real 200 page. URL
    // resolution must surface the 200 crawl first so wabac lands on it.
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/lgbt/",
        title: "404 Not Found",
        timestamp: "20250201000000",
        status: None, // an archived error capture (no clean 2xx)
        crawl_id: "badcrawl",
        collection: "c",
        ..Default::default()
    })
    .unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/lgbt/",
        title: "The Real Page",
        timestamp: "20250107000000",
        status: Some(200),
        crawl_id: "goodcrawl",
        collection: "c",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    let (total, hits) = idx
        .collection_pages("c", Some("https://ex.com/lgbt/"), None, 0, 25)
        .unwrap();
    assert_eq!(total, 2, "both captures matched");
    assert_eq!(
        hits[0].crawl_id, "goodcrawl",
        "the 200 capture resolves first, ahead of the archived 404"
    );
    assert_eq!(hits[1].crawl_id, "badcrawl");
}

#[test]
fn description_falls_back_to_og_description() {
    let html = br#"<html><head><meta property="og:description" content="OG only"></head>
        <body>x</body></html>"#;
    let t = extract_html_text(html);
    assert_eq!(t.description, "OG only");
}

#[test]
fn extract_lang_from_html_element() {
    let html = br#"<html lang="en-US"><head><title>T</title></head><body>x</body></html>"#;
    let t = extract_html_text(html);
    assert_eq!(t.lang, "en-US");
}

#[test]
fn detect_lang_fills_in_when_html_lang_absent() {
    // Enough English text to detect reliably.
    let en = "The quick brown fox jumps over the lazy dog near the riverbank \
              while the sun sets slowly behind the distant hills this evening.";
    assert_eq!(detect_lang(en).as_deref(), Some("en"));
    // French.
    let fr = "Le vif renard brun saute par-dessus le chien paresseux tandis que \
              le soleil se couche lentement derrière les collines lointaines ce soir.";
    assert_eq!(detect_lang(fr).as_deref(), Some("fr"));
    // Too short to trust.
    assert_eq!(detect_lang("hi there"), None);
}

#[test]
fn detect_lang_caps_long_multibyte_body_without_panicking() {
    // A body well over DETECT_SAMPLE_BYTES with multibyte chars (é, à) right
    // around the cut point must not panic on a mid-UTF-8 slice, and still
    // detect the dominant language.
    let sentence =
        "Le renard brun rapide sauté par-dessus le chien paresseux à côté de la rivière. ";
    let long = sentence.repeat(80); // > 2 KB, many multi-byte chars
    assert!(long.len() > DETECT_SAMPLE_BYTES);
    assert_eq!(detect_lang(&long).as_deref(), Some("fr"));
}

#[test]
fn indexing_detects_lang_only_as_fallback() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    let french = "Le renard brun rapide saute par-dessus le chien paresseux et \
                  le soleil se couche derrière les collines lointaines ce soir la.";
    // Declared lang wins even when the body is another language.
    idx.index_page(&Page {
        url: "https://ex.com/declared",
        title: "T",
        body: french,
        lang: "en-GB",
        crawl_id: "c1",
        crawl_name: "C1",
        ..Default::default()
    })
    .unwrap();
    // No declared lang -> detected from body.
    idx.index_page(&Page {
        url: "https://ex.com/detected",
        title: "T",
        body: french,
        lang: "",
        crawl_id: "c1",
        crawl_name: "C1",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    let en = idx.search("lang:en", 10).unwrap();
    assert_eq!(en.len(), 1);
    assert_eq!(
        en[0].url, "https://ex.com/declared",
        "declared en-GB wins over the body"
    );
    let fr = idx.search("lang:fr", 10).unwrap();
    assert_eq!(fr.len(), 1);
    assert_eq!(
        fr[0].url, "https://ex.com/detected",
        "empty lang detected as fr from body"
    );
}

#[test]
fn primary_lang_takes_the_first_subtag() {
    assert_eq!(primary_lang("en-US"), "en");
    assert_eq!(primary_lang("EN"), "en");
    assert_eq!(primary_lang("pt_BR"), "pt");
    assert_eq!(primary_lang(""), "");
}

#[test]
fn type_and_lang_filters() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/page",
        title: "Doc",
        body: "shared",
        media_type: "html",
        lang: "en-US",
        crawl_id: "c1",
        crawl_name: "C1",
        ..Default::default()
    })
    .unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/file.pdf",
        title: "Report",
        body: "shared",
        media_type: "pdf",
        lang: "",
        crawl_id: "c1",
        crawl_name: "C1",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    let r = idx.search("type:pdf", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].url, "https://ex.com/file.pdf");

    let r = idx.search("shared type:html", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].url, "https://ex.com/page");

    // lang is stored as its primary subtag, so `lang:en` matches `en-US`.
    let r = idx.search("lang:en", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].url, "https://ex.com/page");
}

#[test]
fn roundtrip_index_and_search() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();

    idx.index_page(&page(
        "http://example.com/",
        "Example Page",
        "This is some interesting content about Rust programming",
        "abc12345",
        "My Collection",
    ))
    .unwrap();
    idx.commit().unwrap();

    let results = idx.search("Rust programming", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "http://example.com/");
    assert_eq!(results[0].crawl_id, "abc12345");
    assert_eq!(results[0].doc_type, "page");
}

#[test]
fn description_and_headings_are_searchable() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/a",
        title: "Plain Title",
        body: "ordinary body",
        description: "a treatise on marmots",
        headings: "Notable Rodents",
        crawl_id: "c1",
        crawl_name: "C1",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    assert_eq!(
        idx.search("marmots", 10).unwrap().len(),
        1,
        "description searchable"
    );
    assert_eq!(
        idx.search("rodents", 10).unwrap().len(),
        1,
        "headings searchable"
    );
    assert_eq!(
        idx.search("a", 10).unwrap()[0].description,
        "a treatise on marmots"
    );
}

#[test]
fn collection_document_is_searchable() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();

    idx.index_collection(
        "abc12345",
        "My Archive",
        "my-archive",
        "A collection about digital preservation and web archiving",
    )
    .unwrap();
    idx.commit().unwrap();

    let results = idx.search("digital preservation", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].doc_type, "collection");
    assert_eq!(results[0].crawl_id, "abc12345");
}

#[test]
fn search_returns_snippet() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();

    idx.index_page(&page(
        "http://example.com/",
        "Example",
        "The quick brown fox jumps over the lazy dog near the riverbank",
        "abc12345",
        "Test",
    ))
    .unwrap();
    idx.commit().unwrap();

    let results = idx.search("fox", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0].snippet.contains("fox"),
        "snippet should contain matched term: {}",
        results[0].snippet
    );
}

#[test]
fn open_errors_on_schema_mismatch() {
    use tantivy::schema::{Schema, TEXT};
    let tmp = TempDir::new().unwrap();
    // Create an index with a different (older-style) schema.
    let mut b = Schema::builder();
    b.add_text_field("body", TEXT);
    tantivy::Index::create_in_dir(tmp.path(), b.build()).unwrap();

    // Opening with the current schema must fail cleanly (not panic), and
    // the message should point the user at reindex.
    let result = SearchIndex::open(tmp.path());
    assert!(result.is_err(), "opening a mismatched schema should error");
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("reindex"),
        "error should suggest reindex: {msg}"
    );
}

#[test]
fn search_no_results() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.commit().unwrap();

    let results = idx.search("nonexistent", 10).unwrap();
    assert!(results.is_empty());
}

#[test]
fn status_and_modified_year_filters() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/ok",
        title: "OK",
        body: "shared",
        status: Some(200),
        modified_year: Some(2015),
        crawl_id: "c1",
        crawl_name: "C1",
        ..Default::default()
    })
    .unwrap();
    idx.index_page(&Page {
        url: "https://ex.com/gone",
        title: "Gone",
        body: "shared",
        status: Some(404),
        modified_year: Some(2020),
        crawl_id: "c1",
        crawl_name: "C1",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    let r = idx.search("status:200", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].url, "https://ex.com/ok");

    let r = idx.search("shared modified:2020", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].url, "https://ex.com/gone");

    // Status ranges work (u64 field): 4xx only.
    let r = idx.search("status:[400 TO 499]", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].url, "https://ex.com/gone");
}

#[test]
fn site_of_extracts_registrable_domain() {
    // Subdomains unify to the registrable domain.
    assert_eq!(site_of("https://www.example.com/a"), "example.com");
    assert_eq!(site_of("https://blog.example.com/"), "example.com");
    // Multi-level public suffix handled via the PSL.
    assert_eq!(site_of("https://www.bbc.co.uk/news"), "bbc.co.uk");
    // Private suffixes (github.io) are effective TLDs, so subdomains stay distinct.
    assert_eq!(site_of("https://alice.github.io/"), "alice.github.io");
    // No host / unparseable input yields an empty site.
    assert_eq!(site_of("urn:text:foo"), "");
}

#[test]
fn site_filter_spans_subdomains_while_domain_is_exact() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&page(
        "https://www.example.com/a",
        "A",
        "shared",
        "c1",
        "C1",
    ))
    .unwrap();
    idx.index_page(&page(
        "https://blog.example.com/b",
        "B",
        "shared",
        "c1",
        "C1",
    ))
    .unwrap();
    idx.index_page(&page("https://other.org/c", "C", "shared", "c1", "C1"))
        .unwrap();
    idx.commit().unwrap();

    // site: matches the whole registrable domain across subdomains.
    let r = idx.search("site:example.com", 10).unwrap();
    assert_eq!(r.len(), 2, "site: spans www. and blog.");

    // domain: stays exact-host.
    let r = idx.search("domain:www.example.com", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].url, "https://www.example.com/a");
}

#[test]
fn domain_of_extracts_lowercased_host() {
    assert_eq!(domain_of("https://Example.COM/a/b?x=1"), "example.com");
    assert_eq!(domain_of("http://sub.example.org/"), "sub.example.org");
    // No host / unparseable input yields an empty domain.
    assert_eq!(domain_of("urn:text:foo"), "");
    assert_eq!(domain_of("not a url"), "");
}

#[test]
fn url_search_text_yields_host_and_path_words() {
    let text = url_search_text("https://github.com/DocNow/hydrator");
    assert_eq!(text, "github.com DocNow hydrator");
}

#[test]
fn search_matches_words_from_the_url() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&page(
        "https://github.com/DocNow/hydrator",
        "Some Title",
        "unrelated body text",
        "abc12345",
        "Test",
    ))
    .unwrap();
    idx.commit().unwrap();

    // "hydrator" appears only in the URL, but url words are searchable.
    let results = idx.search("hydrator", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://github.com/DocNow/hydrator");
    assert_eq!(results[0].domain, "github.com");
}

#[test]
fn domain_filter_restricts_to_exact_host() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&page(
        "https://example.com/one",
        "One",
        "shared word",
        "c1",
        "C1",
    ))
    .unwrap();
    idx.index_page(&page(
        "https://other.org/two",
        "Two",
        "shared word",
        "c1",
        "C1",
    ))
    .unwrap();
    idx.commit().unwrap();

    let results = idx.search("domain:example.com", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://example.com/one");

    // Combined with a term, still AND-scoped to that domain.
    let results = idx.search("domain:example.com shared", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://example.com/one");
}

#[test]
fn collection_filter_restricts_results() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&Page {
        url: "https://a.com/1",
        title: "A",
        body: "shared",
        crawl_id: "w1",
        crawl_name: "W1",
        collection: "demo",
        ..Default::default()
    })
    .unwrap();
    idx.index_page(&Page {
        url: "https://b.com/1",
        title: "B",
        body: "shared",
        crawl_id: "w2",
        crawl_name: "W2",
        collection: "other",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    let r = idx.search("collection:demo", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].collection, "demo");

    // Combined with a term, still scoped to the collection.
    let r = idx.search("collection:demo shared", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].url, "https://a.com/1");
}

#[test]
fn multi_word_queries_require_all_terms() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&page("https://ex.com/a", "A", "alpha beta", "c1", "C1"))
        .unwrap();
    idx.index_page(&page("https://ex.com/b", "B", "alpha gamma", "c1", "C1"))
        .unwrap();
    idx.commit().unwrap();

    // AND-by-default: only the page containing BOTH words matches.
    let results = idx.search("alpha beta", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://ex.com/a");

    // A term present in neither-together combination returns nothing.
    let results = idx.search("beta gamma", 10).unwrap();
    assert!(results.is_empty());
}

#[test]
fn title_matches_rank_above_body_matches() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    // The term is in page 1's title and page 2's body only.
    idx.index_page(&page(
        "https://ex.com/title-hit",
        "kangaroo",
        "filler text",
        "c1",
        "C1",
    ))
    .unwrap();
    idx.index_page(&page(
        "https://ex.com/body-hit",
        "filler",
        "kangaroo text",
        "c1",
        "C1",
    ))
    .unwrap();
    idx.commit().unwrap();

    let results = idx.search("kangaroo", 10).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].url, "https://ex.com/title-hit",
        "title match should rank first"
    );
}

#[test]
fn year_of_parses_leading_year() {
    assert_eq!(year_of("20210417120000"), Some(2021));
    assert_eq!(year_of("2021"), Some(2021));
    assert_eq!(year_of(""), None);
    assert_eq!(year_of("notadate"), None);
    assert_eq!(year_of("0099010100"), None); // implausible year
}

#[test]
fn year_filter_exact_and_range() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&page_ts("https://ex.com/2019", "20190101000000"))
        .unwrap();
    idx.index_page(&page_ts("https://ex.com/2021", "20210101000000"))
        .unwrap();
    idx.index_page(&page_ts("https://ex.com/2023", "20230101000000"))
        .unwrap();
    idx.commit().unwrap();

    // Exact year.
    let r = idx.search("year:2021", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].url, "https://ex.com/2021");

    // Inclusive range.
    let r = idx.search("year:[2020 TO 2023]", 10).unwrap();
    assert_eq!(r.len(), 2, "2021 and 2023 fall in range");

    // Combined with a term (AND-scoped).
    let r = idx.search("shared year:2019", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].url, "https://ex.com/2019");
}

/// Look up one facet dimension's buckets as a value->count map.
fn facet_map(resp: &SearchResponse, field: &str) -> std::collections::HashMap<String, u64> {
    resp.facets
        .iter()
        .find(|g| g.field == field)
        .map(|g| {
            g.buckets
                .iter()
                .map(|b| (b.value.clone(), b.count))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn facet_counts_reflect_the_query() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    // Three "shared" pages: two on example.com (2021 html), one on other.org
    // (2023 pdf), spread across two collections.
    idx.index_page(&Page {
        url: "https://example.com/a",
        title: "A",
        body: "shared",
        timestamp: "20210101000000",
        media_type: "html",
        lang: "en-US",
        collection: "demo",
        crawl_id: "w1",
        crawl_name: "W1",
        ..Default::default()
    })
    .unwrap();
    idx.index_page(&Page {
        url: "https://example.com/b",
        title: "B",
        body: "shared",
        timestamp: "20210601000000",
        media_type: "html",
        lang: "en",
        collection: "demo",
        crawl_id: "w1",
        crawl_name: "W1",
        ..Default::default()
    })
    .unwrap();
    idx.index_page(&Page {
        url: "https://other.org/c",
        title: "C",
        body: "shared",
        timestamp: "20230101000000",
        media_type: "pdf",
        lang: "fr",
        collection: "news",
        crawl_id: "w2",
        crawl_name: "W2",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    let resp = idx.search_faceted("shared", 10, 0).unwrap();
    assert_eq!(resp.total_hits, 3);

    // The Site facet is the registrable domain (FIELD_SITE), not the host.
    let sites = facet_map(&resp, FIELD_SITE);
    assert_eq!(sites.get("example.com"), Some(&2));
    assert_eq!(sites.get("other.org"), Some(&1));

    let years = facet_map(&resp, FIELD_YEAR);
    assert_eq!(years.get("2021"), Some(&2));
    assert_eq!(years.get("2023"), Some(&1));

    let types = facet_map(&resp, FIELD_MEDIA_TYPE);
    assert_eq!(types.get("html"), Some(&2));
    assert_eq!(types.get("pdf"), Some(&1));

    let colls = facet_map(&resp, FIELD_COLLECTION);
    assert_eq!(colls.get("demo"), Some(&2));
    assert_eq!(colls.get("news"), Some(&1));

    // Narrowing the query narrows the facet counts to the matching subset.
    let resp = idx
        .search_faceted("shared domain:example.com", 10, 0)
        .unwrap();
    assert_eq!(resp.total_hits, 2);
    assert_eq!(facet_map(&resp, FIELD_YEAR).get("2021"), Some(&2));
    assert!(
        !facet_map(&resp, FIELD_YEAR).contains_key("2023"),
        "2023 filtered out"
    );
}

#[test]
fn repeat_captures_of_a_url_are_grouped() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    // The same URL captured three times (different crawls), plus a distinct URL.
    for ts in ["20210101000000", "20220101000000", "20230101000000"] {
        idx.index_page(&Page {
            url: "https://ex.com/a",
            title: "A",
            body: "shared",
            timestamp: ts,
            crawl_id: "c1",
            crawl_name: "C1",
            ..Default::default()
        })
        .unwrap();
    }
    idx.index_page(&Page {
        url: "https://ex.com/b",
        title: "B",
        body: "shared",
        crawl_id: "c1",
        crawl_name: "C1",
        ..Default::default()
    })
    .unwrap();
    idx.commit().unwrap();

    let resp = idx.search_faceted("shared", 10, 0).unwrap();
    // Two distinct results, not four captures.
    assert_eq!(resp.total_hits, 2);
    assert!(!resp.capped);
    let a = resp
        .results
        .iter()
        .find(|r| r.url == "https://ex.com/a")
        .unwrap();
    assert_eq!(
        a.capture_count, 3,
        "three captures of /a collapse into one result"
    );
    let b = resp
        .results
        .iter()
        .find(|r| r.url == "https://ex.com/b")
        .unwrap();
    assert_eq!(b.capture_count, 1);
}

#[test]
fn month_of_parses_year_and_month() {
    assert_eq!(month_of("20210417120000"), Some(202104));
    assert_eq!(month_of("202412"), Some(202412));
    assert_eq!(month_of("20211320"), None, "month 13 is invalid");
    assert_eq!(month_of("2021"), None, "too short for a month");
    assert_eq!(month_of(""), None);
}

#[test]
fn timeline_is_chronological_and_counts_months() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    // Two captures in 2021-03, one in 2021-01, one in 2023-07 (distinct URLs
    // so month counts aren't affected by URL grouping).
    for (url, ts) in [
        ("https://ex.com/1", "20210301000000"),
        ("https://ex.com/2", "20210315000000"),
        ("https://ex.com/3", "20210101000000"),
        ("https://ex.com/4", "20230701000000"),
    ] {
        idx.index_page(&page_ts(url, ts)).unwrap();
    }
    idx.commit().unwrap();

    let resp = idx.search_faceted("shared", 10, 0).unwrap();
    let tl: Vec<(u64, u64)> = resp.timeline.iter().map(|t| (t.ym, t.count)).collect();
    // Oldest first, one bucket per distinct month, with correct counts.
    assert_eq!(tl, vec![(202101, 1), (202103, 2), (202307, 1)]);

    // Filtering by month narrows to that month only.
    let resp = idx.search_faceted("shared month:202103", 10, 0).unwrap();
    assert_eq!(resp.total_hits, 2);
}

#[test]
fn pagination_offsets_and_reports_total() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    for i in 0..25 {
        let url = format!("https://ex.com/{i:02}");
        idx.index_page(&Page {
            url: &url,
            title: "T",
            body: "shared",
            crawl_id: "c1",
            crawl_name: "C1",
            ..Default::default()
        })
        .unwrap();
    }
    idx.commit().unwrap();

    let p1 = idx.search_faceted("shared", 20, 0).unwrap();
    assert_eq!(
        p1.total_hits, 25,
        "total counts all matches, not just the page"
    );
    assert_eq!(p1.results.len(), 20, "first page is full");

    let p2 = idx.search_faceted("shared", 20, 20).unwrap();
    assert_eq!(p2.total_hits, 25);
    assert_eq!(p2.results.len(), 5, "second page holds the remainder");

    // No overlap between the two pages.
    let urls1: std::collections::HashSet<_> = p1.results.iter().map(|r| &r.url).collect();
    assert!(
        p2.results.iter().all(|r| !urls1.contains(&r.url)),
        "pages must not overlap"
    );
}

#[test]
fn malformed_query_does_not_error() {
    let tmp = TempDir::new().unwrap();
    let mut idx = SearchIndex::open(tmp.path()).unwrap();
    idx.index_page(&page("https://ex.com/a", "A", "hello world", "c1", "C1"))
        .unwrap();
    idx.commit().unwrap();

    // An unbalanced quote would be a parse error; lenient parsing must not
    // propagate it as an Err (the search box should never 500).
    assert!(idx.search("\"hello", 10).is_ok());
    assert!(idx.search("title:", 10).is_ok());
}
