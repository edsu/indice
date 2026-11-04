//! Headless-browser smoke test for multi-WACZ *collection* replay.
//!
//! Companion to `browser.rs` (single-WACZ). It proves the thing only a real
//! browser can: that wabac.js consumes our collection manifest
//! (`GET /collection/{id}/replay.json`) and replays pages from *different member
//! WACZs* within one collection — i.e. genuine multi-WACZ replay, not just one
//! WACZ loaded. `#[ignore]`d by default (needs a WebDriver + browser).
//!
//! Get a version-matched Chrome + chromedriver with
//! `./scripts/fetch-browser.sh` (it prints the exact commands), then:
//!
//! ```sh
//! "<path>/chromedriver" --port=9515 &
//! CHROME_BIN="<path>/Google Chrome for Testing" WEBDRIVER_URL=http://localhost:9515 \
//!   cargo test -p indice-lib --test browser_collection -- --ignored
//! ```

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use thirtyfour::prelude::*;

/// The **anonymous** view, which is what these tests check.
///
/// `Access::proxy` with credentials nobody sends: every request is an anonymous
/// visitor, exactly as one arriving at a server. `Access::local` would make each
/// request the operator and put workroom chrome on every page.
fn public_config(home: &std::path::Path) -> indice_lib::server::ServerConfig {
    // A server refuses to start without a roster, so that everyone the identity
    // provider admits is not silently an admin. Empty is the right one here:
    // these tests are about what an anonymous reader sees, and `users: []`
    // means nobody may write.
    let roster = home.join("users.yaml");
    if !roster.exists() {
        std::fs::write(&roster, "users: []\n").unwrap();
    }
    indice_lib::server::ServerConfig::new(indice_lib::server::Access::proxy(
        "X-Forwarded-Email",
        "test-secret-not-sent",
    ))
}

/// Serve `home` as indice does: the app on one ephemeral loopback port and
/// replay on another, so replay really runs on an origin of its own. Returns
/// the app's address.
async fn serve(
    home: &Path,
    mut config: indice_lib::server::ServerConfig,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let app = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = app.local_addr().unwrap();
    let replay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    // A server will not start without its public URL; this one's is the
    // address it is about to listen on.
    if config.site_url().is_none() {
        config.set_site_url(&format!("http://{addr}")).unwrap();
    }
    config.replay_origin = Some(indice_lib::server::ReplayOrigin::for_listener(
        replay.local_addr().unwrap(),
    ));
    let home = home.to_path_buf();
    let handle = tokio::spawn(async move {
        indice_lib::server::serve_on_listeners(
            app,
            replay,
            &home,
            None,
            config,
            indice_lib::server::Providers::default(),
        )
        .await
        .unwrap();
    });
    (addr, handle)
}

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(FIXTURES).join(name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running WebDriver (chromedriver) and browser; run with --ignored"]
async fn browser_renders_multi_wacz_collection() {
    // 1. Index two different real WACZs into ONE collection.
    let tmp = tempfile::TempDir::new().unwrap();
    let coll = "Test Collection";
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, coll).unwrap();
    indice_lib::index::index_path(
        &fixture("github-bitcoin-mining.wacz"),
        tmp.path(),
        None,
        coll,
    )
    .unwrap();
    let id = indice_lib::collections::slugify(coll);

    // 2. Serve on an ephemeral port (localhost is a secure context for the SW).
    let (addr, server) = serve(tmp.path(), public_config(tmp.path())).await;

    // 3. Headless Chrome via WebDriver.
    let driver = connect_driver().await;

    let result = drive_and_check(&driver, addr, &id).await;
    let _ = driver.quit().await;
    server.abort();
    result.unwrap();
}

/// A collection whose members have no whole-file `sha256` (as streamed remote
/// WACZs do — e.g. Browsertrix without `--download`) must still replay. Earlier
/// the manifest gave every hashless member the same empty `"sha256:"` hash, which
/// collapsed them in wabac's loader so *no* members loaded ("Archived Page Not
/// Found", no `/files/…` requests). Regression guard: blank the members' sha256
/// and confirm the collection still replays a page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running WebDriver (chromedriver) and browser; run with --ignored"]
async fn browser_renders_collection_with_hashless_members() {
    let tmp = tempfile::TempDir::new().unwrap();
    let coll = "Test Collection";
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, coll).unwrap();
    indice_lib::index::index_path(
        &fixture("github-bitcoin-mining.wacz"),
        tmp.path(),
        None,
        coll,
    )
    .unwrap();

    // Blank every member's sha256, as a streamed source has none.
    let waczs = tmp.path().join("index").join("waczs.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&waczs).unwrap()).unwrap();
    for w in v.as_array_mut().unwrap() {
        w["sha256"] = serde_json::Value::String(String::new());
    }
    std::fs::write(&waczs, serde_json::to_vec(&v).unwrap()).unwrap();

    let id = indice_lib::collections::slugify(coll);
    let (addr, server) = serve(tmp.path(), public_config(tmp.path())).await;

    let driver = connect_driver().await;
    let result = render_check(
        &driver,
        addr,
        &id,
        "https://storymaps.arcgis.com/stories/278e1b5c18a3474082e583e889705179",
        "20260609213407",
        "2Tone",
    )
    .await;
    let _ = driver.quit().await;
    server.abort();
    result.unwrap();
}

/// Replaying one page of a collection opens only the member WACZ that holds
/// it. The collection's `replay.json` names indice's lookup
/// (`pagesQueryUrl`), so wabac asks which member holds the page instead of
/// opening and indexing every member first, which for streamed crawls is
/// hundreds of remote reads before anything shows. Checked from the server's
/// request log: the other member's WACZ is never fetched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running WebDriver (chromedriver) and browser; run with --ignored"]
async fn collection_replay_opens_only_the_member_holding_the_page() {
    let log = request_log();
    let tmp = tempfile::TempDir::new().unwrap();
    let coll = "Test Collection";
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, coll).unwrap();
    indice_lib::index::index_path(
        &fixture("github-bitcoin-mining.wacz"),
        tmp.path(),
        None,
        coll,
    )
    .unwrap();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let member = |file: &str| {
        manifest
            .waczs
            .iter()
            .find(|w| w.source.location().ends_with(file))
            .map(|w| w.id.clone())
            .unwrap()
    };
    let (holds_page, other) = (member("a.wacz"), member("github-bitcoin-mining.wacz"));
    let id = indice_lib::collections::slugify(coll);
    let (addr, server) = serve(tmp.path(), public_config(tmp.path())).await;

    let driver = connect_driver().await;
    let result = render_check(
        &driver,
        addr,
        &id,
        "https://storymaps.arcgis.com/stories/278e1b5c18a3474082e583e889705179",
        "20260609213407",
        "2Tone",
    )
    .await;
    let _ = driver.quit().await;
    server.abort();
    result.unwrap();

    let log = String::from_utf8_lossy(&log.lock().unwrap()).into_owned();
    assert!(
        log.contains(&format!("/collection/{id}/pages")),
        "wabac asked indice which member holds the page"
    );
    assert!(
        log.contains(&format!("/files/{holds_page}")),
        "the member holding the page was read"
    );
    assert!(
        !log.contains(&format!("/files/{other}")),
        "the other member was never opened"
    );
}

/// A page reached through a redirect still replays when indice resolves
/// pages: the seed is an `arcg.is` short link captured as a redirect to the
/// story, so wabac looks up the short link, follows the redirect, and looks
/// up the story in turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running WebDriver (chromedriver) and browser; run with --ignored"]
async fn collection_replay_follows_a_redirect_through_the_lookup() {
    let tmp = tempfile::TempDir::new().unwrap();
    let coll = "Test Collection";
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, coll).unwrap();
    indice_lib::index::index_path(
        &fixture("github-bitcoin-mining.wacz"),
        tmp.path(),
        None,
        coll,
    )
    .unwrap();
    let id = indice_lib::collections::slugify(coll);
    let (addr, server) = serve(tmp.path(), public_config(tmp.path())).await;
    let driver = connect_driver().await;
    let result = render_check(
        &driver,
        addr,
        &id,
        "https://arcg.is/1zLCSC4",
        "20260609213406",
        "2Tone",
    )
    .await;
    let _ = driver.quit().await;
    server.abort();
    result.unwrap();
}

/// A page of a Browsertrix crawl replays with a stylesheet that Browsertrix
/// wrote into another of the crawl's WACZs.
///
/// As in a real crawl, the stylesheet is in the crawl's first file, with its
/// seed page, and the page is in a later one. Three more files hold neither.
/// For a resource its page's file lacks, wabac tries the first three files by
/// name in sorted order, so the first file is given the id that sorts last:
/// only the crawl's manifest naming its files in the crawl's order puts it
/// among the three.
///
/// The same browser first replays the page as a crawl of one file, where the
/// stylesheet is missing, and then as one Browsertrix crawl. The second
/// replay has a cache of its own, so the first one's is not reused for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running WebDriver (chromedriver) and browser; run with --ignored"]
async fn a_crawls_page_replays_with_a_stylesheet_from_another_of_its_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let coll = "Split Crawl";
    let warcs = tmp.path().join("warcs");
    std::fs::create_dir_all(&warcs).unwrap();
    let mut files: Vec<(&str, String)> = vec![
        (
            "first",
            [
                warc_response(
                    "https://split.example/",
                    "2025-01-25T22:32:00Z",
                    "text/html",
                    r#"<html><head><link rel="stylesheet" href="/style.css"></head><body>home</body></html>"#,
                ),
                warc_response(
                    "https://split.example/style.css",
                    "2025-01-25T22:32:01Z",
                    "text/css",
                    "#probe { color: rgb(1, 2, 3); }",
                ),
            ]
            .concat(),
        ),
        (
            "later",
            warc_response(
                PAGE,
                "2025-01-25T22:32:02Z",
                "text/html",
                r#"<html><head><link rel="stylesheet" href="/style.css"></head><body><p id="probe">split crawl</p></body></html>"#,
            ),
        ),
    ];
    for n in 1..=3 {
        files.push((
            ["other-1", "other-2", "other-3"][n - 1],
            warc_response(
                &format!("https://split.example/other-{n}.txt"),
                "2025-01-25T22:32:03Z",
                "text/plain",
                "neither",
            ),
        ));
    }
    let mut ids = std::collections::HashMap::new();
    for (name, records) in &files {
        let warc = warcs.join(format!("{name}.warc"));
        std::fs::write(&warc, records).unwrap();
        let built = indice_lib::wacz_build::build_wacz(
            &[warc],
            &indice_lib::wacz_build::WaczBuildMeta::default(),
            &tmp.path().join("archive"),
            name,
        )
        .unwrap();
        indice_lib::index::index_path(&built.path, tmp.path(), None, coll).unwrap();
        let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
        let w = manifest
            .waczs
            .iter()
            .find(|w| w.source.location().ends_with(&format!("/{name}.wacz")));
        ids.insert(*name, w.unwrap().id.clone());
    }
    // The manifest is read per request, so each rewrite takes effect at once.
    let waczs = tmp.path().join("index").join("waczs.json");
    let rewrite = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&waczs).unwrap()).unwrap();
        v.as_array_mut().unwrap().iter_mut().for_each(f);
        std::fs::write(&waczs, serde_json::to_vec(&v).unwrap()).unwrap();
    };
    let first = ids["first"].clone();
    rewrite(&|w| {
        if w["id"] == first.as_str() {
            w["id"] = serde_json::json!("ffffffff");
        }
    });
    let page_file = &ids["later"];
    let (addr, server) = serve(tmp.path(), public_config(tmp.path())).await;
    let viewer = format!("http://{addr}/crawl/{page_file}/20250125223202/{PAGE}");
    let driver = connect_driver().await;

    let alone = probe_color(&driver, &viewer).await;
    rewrite(&|w| {
        w["browsertrix"] = serde_json::json!({
            "host": "https://app.browsertrix.com",
            "item_id": "manual-20250125223202-f7a72d33-167",
        });
    });
    let together = probe_color(&driver, &viewer).await;
    let _ = driver.quit().await;
    server.abort();

    let alone = alone.unwrap();
    assert_ne!(alone, STYLED, "one file holds no stylesheet to apply");
    assert_eq!(together.unwrap(), STYLED, "the crawl's first file was read");
}

const PAGE: &str = "https://split.example/page.html";

/// The colour the split crawl's stylesheet gives the probe.
const STYLED: &str = "rgb(1, 2, 3)";

/// One WARC `response` record: `url` answering 200 with `body`.
fn warc_response(url: &str, date: &str, content_type: &str, body: &str) -> String {
    let http = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    format!(
        "WARC/1.1\r\nWARC-Type: response\r\nWARC-Record-ID: <urn:uuid:{}>\r\n\
         WARC-Date: {date}\r\nWARC-Target-URI: {url}\r\n\
         Content-Type: application/http; msgtype=response\r\nContent-Length: {}\r\n\r\n{http}\r\n\r\n",
        uuid_for(url),
        http.len()
    )
}

/// A record id that differs per URL, which is all a test WARC needs.
fn uuid_for(url: &str) -> String {
    let n = url
        .bytes()
        .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b.into()));
    format!("00000000-0000-4000-8000-{:012x}", n & 0xffff_ffff_ffff)
}

/// Open `viewer` and return the probe's colour once the stylesheet has had a
/// moment to apply, searching the replay frame's same-origin frames.
async fn probe_color(driver: &WebDriver, viewer: &str) -> Result<String, String> {
    driver.goto(viewer).await.map_err(|e| e.to_string())?;
    driver
        .find(By::Id("replay-frame"))
        .await
        .map_err(|e| format!("the viewer has no replay frame: {e}"))?
        .enter_frame()
        .await
        .map_err(|e| e.to_string())?;
    let script = r#"
        function find(doc) {
            if (!doc) return null;
            const el = doc.getElementById && doc.getElementById('probe');
            if (el) return el;
            const nodes = doc.querySelectorAll ? doc.querySelectorAll('*') : [];
            for (const n of nodes) {
                if (n.shadowRoot) { const f = find(n.shadowRoot); if (f) return f; }
                if (n.tagName === 'IFRAME') {
                    try { const f = find(n.contentDocument); if (f) return f; } catch (e) {}
                }
            }
            return null;
        }
        const el = find(document);
        if (!el) return null;
        const sheetsDone = el.ownerDocument.readyState === 'complete';
        return sheetsDone ? el.ownerDocument.defaultView.getComputedStyle(el).color : null;
    "#;
    let read = || async {
        let ret = driver
            .execute(script, vec![])
            .await
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(ret.json().as_str().map(str::to_string))
    };
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut color = None;
    while color.is_none() && Instant::now() <= deadline {
        color = read().await?;
        if color.is_none() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    // Rendered but unstyled: a moment more for a stylesheet still on its way.
    if color.as_deref().is_some_and(|c| c != STYLED) {
        tokio::time::sleep(Duration::from_secs(3)).await;
        color = read().await?.or(color);
    }
    driver
        .enter_default_frame()
        .await
        .map_err(|e| e.to_string())?;
    color.ok_or_else(|| format!("the probe never rendered ({viewer})"))
}

/// Everything this test binary logs, for a test to read back which paths were
/// requested. One global subscriber for the binary; each test's homes give
/// its crawls ids of their own, so lines from other tests don't match.
fn request_log() -> &'static std::sync::Arc<std::sync::Mutex<Vec<u8>>> {
    use std::sync::{Arc, Mutex, OnceLock};
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || LogSink(sink.clone()))
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        buf
    })
}

struct LogSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Connect to a headless Chrome via WebDriver. `WEBDRIVER_URL` overrides the
/// chromedriver endpoint; `CHROME_BIN` selects a specific Chrome binary (e.g. a
/// matched Chrome for Testing from @puppeteer/browsers).
async fn connect_driver() -> WebDriver {
    let wd = std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://localhost:9515".into());
    let mut caps = DesiredCapabilities::chrome();
    if let Ok(bin) = std::env::var("CHROME_BIN") {
        caps.set_binary(&bin).unwrap();
    }
    caps.add_arg("--headless=new").unwrap();
    caps.add_arg("--no-sandbox").unwrap();
    caps.add_arg("--disable-gpu").unwrap();
    caps.add_arg("--disable-dev-shm-usage").unwrap();
    WebDriver::new(&wd, caps)
        .await
        .expect("connect to WebDriver - is chromedriver running?")
}

async fn drive_and_check(driver: &WebDriver, addr: SocketAddr, id: &str) -> Result<(), String> {
    // Both pages are replayed through the SAME collection manifest source; only
    // the target url/ts differ. Each lives in a *different* member WACZ, so
    // rendering both proves wabac resolved them across members via our manifest.

    // Member A (a.wacz): the 200 HTML story page (the arcg.is seed 301s to it).
    render_check(
        driver,
        addr,
        id,
        "https://storymaps.arcgis.com/stories/278e1b5c18a3474082e583e889705179",
        "20260609213407",
        "2Tone",
    )
    .await?;

    // Member B (github-bitcoin-mining.wacz): a different WACZ entirely.
    render_check(
        driver,
        addr,
        id,
        "https://github.com/DocNow/hydrator/pull/78/files",
        "20210417155642",
        "hydrator",
    )
    .await
}

/// Load the collection viewer on `url`/`ts` and poll until `needle` appears in
/// the frame tree (or time out with diagnostics).
async fn render_check(
    driver: &WebDriver,
    addr: SocketAddr,
    collection: &str,
    url: &str,
    ts: &str,
    needle: &str,
) -> Result<(), String> {
    let viewer = format!("http://{addr}/collection/{collection}/{ts}/{url}");
    driver.goto(&viewer).await.map_err(|e| e.to_string())?;

    // ReplayWeb.page lives in the viewer's replay frame; search there.
    driver
        .find(By::Id("replay-frame"))
        .await
        .map_err(|e| format!("the viewer has no replay frame: {e}"))?
        .enter_frame()
        .await
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() <= deadline {
        if deep_contains(driver, needle).await? {
            driver
                .enter_default_frame()
                .await
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let diag = diagnostics(driver).await.unwrap_or_else(|e| e);
    Err(format!(
        "'{needle}' never rendered from collection member ({url}).\nDIAG: {diag}"
    ))
}

async fn diagnostics(driver: &WebDriver) -> Result<String, String> {
    let script = r#"
        const out = {};
        out.loc = location.href;
        out.swController = !!(navigator.serviceWorker && navigator.serviceWorker.controller);
        const rwp = document.querySelector('replay-web-page');
        out.rwp = !!rwp;
        const frames = [];
        function walk(node) {
            if (!node) return;
            if (node.shadowRoot) walk(node.shadowRoot);
            if (node.tagName === 'IFRAME') {
                let info = { src: node.getAttribute('src'), same: false, sample: null };
                try { if (node.contentDocument) { info.same = true;
                    info.sample = (node.contentDocument.body ? node.contentDocument.body.innerText : '').slice(0,120); } }
                catch (e) { info.err = String(e); }
                frames.push(info);
                try { if (node.contentDocument) walk(node.contentDocument); } catch(e){}
            }
            const kids = node.childNodes || [];
            for (let i=0;i<kids.length;i++) walk(kids[i]);
        }
        walk(document);
        out.frames = frames;
        return JSON.stringify(out);
    "#;
    let ret = driver
        .execute(script, vec![])
        .await
        .map_err(|e| e.to_string())?;
    Ok(ret.json().as_str().unwrap_or("<no diag>").to_string())
}

/// True if `needle` appears in any text node reachable from the document,
/// piercing shadow roots and same-origin iframes.
async fn deep_contains(driver: &WebDriver, needle: &str) -> Result<bool, String> {
    let script = r#"
        const needle = arguments[0];
        const acc = [];
        function collect(node) {
            if (!node) return;
            if (node.nodeType === 3) { acc.push(node.textContent); return; }
            if (node.shadowRoot) collect(node.shadowRoot);
            if (node.tagName === 'IFRAME') {
                try { collect(node.contentDocument); } catch (e) { /* cross-origin */ }
            }
            const kids = node.childNodes || [];
            for (let i = 0; i < kids.length; i++) collect(kids[i]);
        }
        collect(document);
        return acc.join(' ').indexOf(needle) !== -1;
    "#;
    let ret = driver
        .execute(script, vec![serde_json::json!(needle)])
        .await
        .map_err(|e| e.to_string())?;
    Ok(ret.json().as_bool().unwrap_or(false))
}
