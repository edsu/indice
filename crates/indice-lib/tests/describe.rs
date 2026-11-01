//! Drafting a finding aid end to end: a real home with an indexed WACZ, the
//! real HTTP client, and a fake OpenAI-compatible service on a local port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indice_lib::describe::{self, DescribeSettings};
use indice_lib::llm::{Llm, LlmSettings};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// A home with one fixture WACZ indexed into the collection "coll".
fn home_with(fixture: &str) -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let dest = archive.join(fixture);
    std::fs::copy(Path::new(FIXTURES).join(fixture), &dest).unwrap();
    indice_lib::index::index_path(&dest, tmp.path(), None, "coll").unwrap();
    tmp
}

/// What the fake service was sent: the request line, headers, and body.
#[derive(Debug, Default, Clone)]
struct Seen {
    request_line: String,
    headers: Vec<String>,
    body: String,
}

/// A one-request OpenAI-compatible service answering with `content` as the
/// model's message, or never answering when `content` is `None`.
fn fake_service(content: Option<&str>) -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let record = seen.clone();
    let answer = content.map(|c| {
        serde_json::json!({"choices": [{"message": {"role": "assistant", "content": c}}]})
            .to_string()
    });
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut headers = Vec::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end().to_string();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
            headers.push(line);
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        *record.lock().unwrap() = Seen {
            request_line: request_line.trim_end().into(),
            headers,
            body: String::from_utf8(body).unwrap(),
        };
        match answer {
            Some(a) => {
                let mut stream = stream;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{a}",
                    a.len()
                )
                .unwrap();
            }
            // Hold the connection open and say nothing.
            None => std::thread::sleep(Duration::from_secs(30)),
        }
    });
    (endpoint, seen)
}

fn settings(endpoint: &str, timeout: Duration) -> LlmSettings {
    LlmSettings {
        endpoint: endpoint.into(),
        model: "fake-model".into(),
        timeout,
        notice: None,
    }
}

fn describe_settings() -> DescribeSettings {
    DescribeSettings {
        max_input_chars: 8_000,
        prompt: describe::BUILT_IN_PROMPT.into(),
        prompt_is_custom: false,
    }
}

#[test]
fn a_draft_is_asked_for_with_the_collections_material_and_applied_only_where_empty() {
    let home = home_with("github-bitcoin-mining.wacz");
    let index_dir = home.path().join("index");
    let manifest = indice_lib::collections::Manifest::open(&index_dir).unwrap();
    let search =
        indice_lib::search::SearchIndex::open_read_only(&index_dir.join("full_text")).unwrap();
    let material = describe::gather(&manifest, &search, "coll").unwrap();
    assert!(!material.pages.is_empty(), "the index has pages to sample");
    let seed = material.crawls[0]
        .seed_pages
        .first()
        .map(|p| p.url.clone())
        .expect("the fixture records a seed page");

    let (endpoint, seen) = fake_service(Some(
        r#"{"description": "Pages about Bitcoin mining on GitHub.",
            "narrative": "A crawl of a GitHub repository.",
            "subjects": ["Bitcoin", "Cryptocurrency mining"],
            "dates": "2024",
            "creator": "Should Be Ignored",
            "rights": "Should Be Ignored"}"#,
    ));
    let llm = Llm::new(
        settings(&endpoint, Duration::from_secs(10)),
        Some("secret-key".into()),
    );
    let draft = describe::draft(&llm, &describe_settings(), &material).unwrap();
    assert_eq!(draft.subjects, vec!["Bitcoin", "Cryptocurrency mining"]);

    // What was sent: the chat completion, the key as a bearer token, the
    // built-in prompt, and material that includes the seed and fenced pages.
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.request_line, "POST /v1/chat/completions HTTP/1.1");
    assert!(
        seen.headers
            .iter()
            .any(|h| h.eq_ignore_ascii_case("authorization: Bearer secret-key")),
        "{:?}",
        seen.headers
    );
    let body: serde_json::Value = serde_json::from_str(&seen.body).unwrap();
    assert_eq!(body["model"], "fake-model");
    assert_eq!(body["messages"][0]["content"], describe::BUILT_IN_PROMPT);
    let user = body["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains(&seed), "the seed page is listed: {user}");
    assert!(user.contains("<<<PAGE 1"), "pages are fenced: {user}");
    assert!(user.chars().count() <= 8_000 + 300, "within the budget");

    // Applied: only the empty fields, with the model recorded as provenance,
    // and the key nowhere in the finding aid.
    let collection = manifest.collection_by_id("coll").unwrap().clone();
    drop(manifest);
    let (mut fields, filled) = draft.fill_empty(&collection);
    assert!(!filled.is_empty());
    assert_eq!(fields.creator, None, "never the model's to fill");
    fields.drafted = Some(describe::provenance(&llm, &filled));
    indice_lib::index::set_collection(home.path(), &collection.name, &fields, None).unwrap();

    let after = indice_lib::collections::Manifest::open(&index_dir).unwrap();
    let c = after.collection_by_id("coll").unwrap();
    let drafted = c.drafted.as_ref().expect("provenance recorded");
    assert_eq!(drafted.model, "fake-model");
    assert_eq!(
        drafted.service,
        endpoint
            .trim_start_matches("http://")
            .trim_end_matches("/v1")
    );
    let mut recorded = drafted.fields.clone();
    recorded.sort();
    let mut expected: Vec<String> = filled.iter().map(|f| f.to_string()).collect();
    expected.sort();
    assert_eq!(recorded, expected);
    for f in &filled {
        match *f {
            "subjects" => assert_eq!(c.subjects, vec!["Bitcoin", "Cryptocurrency mining"]),
            "dates" => assert_eq!(c.dates.as_deref(), Some("2024")),
            _ => {}
        }
    }
    let readme = std::fs::read_to_string(home.path().join("collections/coll/README.md")).unwrap();
    assert!(readme.contains("drafted:"), "{readme}");
    assert!(!readme.contains("secret-key"));
    assert!(!readme.contains("Should Be Ignored"));
}

#[test]
fn a_service_that_never_answers_times_out() {
    let home = home_with("simple.wacz");
    let index_dir = home.path().join("index");
    let manifest = indice_lib::collections::Manifest::open(&index_dir).unwrap();
    let search =
        indice_lib::search::SearchIndex::open_read_only(&index_dir.join("full_text")).unwrap();
    let material = describe::gather(&manifest, &search, "coll").unwrap();

    let (endpoint, _seen) = fake_service(None);
    let llm = Llm::new(settings(&endpoint, Duration::from_secs(2)), None);
    let started = Instant::now();
    let err = describe::draft(&llm, &describe_settings(), &material).unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "gave up after {:?}",
        started.elapsed()
    );
    let msg = format!("{err:#}");
    assert!(msg.contains("127.0.0.1"), "names the service: {msg}");
}
