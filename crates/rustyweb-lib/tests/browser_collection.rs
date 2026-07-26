//! Headless-browser smoke test for multi-WACZ *collection* replay.
//!
//! Companion to `browser.rs` (single-WACZ). It proves the thing only a real
//! browser can: that wabac.js consumes our collection manifest
//! (`GET /collection/{id}/replay.json`) and replays pages from *different member
//! WACZs* within one collection — i.e. genuine multi-WACZ replay, not just one
//! WACZ loaded. `#[ignore]`d by default (needs a WebDriver + browser).
//!
//! ```sh
//! chromedriver --port=9515 &
//! cargo test -p rustyweb-lib --test browser_collection -- --ignored
//! ```

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use thirtyfour::prelude::*;

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
    rustyweb_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, coll).unwrap();
    rustyweb_lib::index::index_path(
        &fixture("github-bitcoin-mining.wacz"),
        tmp.path(),
        None,
        coll,
    )
    .unwrap();
    let id = rustyweb_lib::collections::slugify(coll);

    // 2. Serve on an ephemeral port (localhost is a secure context for the SW).
    let app = rustyweb_lib::server::router(tmp.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    // 3. Headless Chrome via WebDriver.
    let wd = std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://localhost:9515".into());
    let mut caps = DesiredCapabilities::chrome();
    caps.add_arg("--headless=new").unwrap();
    caps.add_arg("--no-sandbox").unwrap();
    caps.add_arg("--disable-gpu").unwrap();
    caps.add_arg("--disable-dev-shm-usage").unwrap();
    let driver = WebDriver::new(&wd, caps)
        .await
        .expect("connect to WebDriver - is `chromedriver --port=9515` running?");

    let result = drive_and_check(&driver, addr, &id).await;
    let _ = driver.quit().await;
    server.abort();
    result.unwrap();
}

async fn drive_and_check(driver: &WebDriver, addr: SocketAddr, id: &str) -> Result<(), String> {
    // Both pages are replayed through the SAME collection manifest source; only
    // the target url/ts differ. Each lives in a *different* member WACZ, so
    // rendering both proves wabac resolved them across members via our manifest.
    let source = format!("/collection/{id}/replay.json");

    // Member A (a.wacz): the 200 HTML story page (the arcg.is seed 301s to it).
    render_check(
        driver,
        addr,
        &source,
        "https://storymaps.arcgis.com/stories/278e1b5c18a3474082e583e889705179",
        "20260609213407",
        "2Tone",
    )
    .await?;

    // Member B (github-bitcoin-mining.wacz): a different WACZ entirely.
    render_check(
        driver,
        addr,
        &source,
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
    source: &str,
    url: &str,
    ts: &str,
    needle: &str,
) -> Result<(), String> {
    let viewer = format!(
        "http://{addr}/replay/viewer?source={source}&coll=test-collection&url={url}&ts={ts}&name=Test%20Collection&collection_id=test-collection"
    );
    driver.goto(&viewer).await.map_err(|e| e.to_string())?;

    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() <= deadline {
        if deep_contains(driver, needle).await? {
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
