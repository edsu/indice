//! Headless-browser replay smoke test.
//!
//! This is the only test that exercises the part of indice that truly matters
//! but can't be checked without a browser: whether ReplayWeb.page / wabac.js
//! actually renders an archived page from a WACZ we serve. It drives real
//! Chrome via WebDriver, so it's `#[ignore]`d by default.
//!
//! To run it, first get a version-matched Chrome + chromedriver
//! (`./scripts/fetch-browser.sh` installs a Chrome for Testing / chromedriver
//! pair and prints the exact commands), then:
//!
//! ```sh
//! "<path>/chromedriver" --port=9515 &          # or any WebDriver server
//! CHROME_BIN="<path>/Google Chrome for Testing" WEBDRIVER_URL=http://localhost:9515 \
//!   cargo test -p indice-lib --test browser -- --ignored
//! ```
//!
//! `WEBDRIVER_URL` overrides the WebDriver endpoint (default
//! `http://localhost:9515`); `CHROME_BIN` selects the Chrome binary (omit to use
//! whatever chromedriver finds). See `scripts/fetch-browser.sh`.

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

/// The workstation shape: every caller is the operator.
fn workstation() -> indice_lib::server::ServerConfig {
    indice_lib::server::ServerConfig::new(
        indice_lib::server::Access::local("127.0.0.1:0".parse().unwrap()).unwrap(),
    )
}

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(FIXTURES).join(name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running WebDriver (chromedriver) and browser; run with --ignored"]
async fn browser_renders_archived_page() {
    // 1. Index a.wacz - a real Browsertrix capture of an ArcGIS StoryMaps page
    //    ("2Tone: The Sound of Britain"). We use a real WACZ, not a hand-rolled
    //    fixture, because wabac.js requires a standard CDXJ index; the minimal
    //    simple.wacz fixture uses a non-standard CDX that wabac can't read.
    let tmp = tempfile::TempDir::new().unwrap();
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, "test").unwrap();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();

    // 2. Serve it on an ephemeral port (localhost is a secure context, so the
    //    service worker is allowed to register).
    let (addr, server) = serve(tmp.path(), public_config(tmp.path())).await;

    // 3. Connect to WebDriver (headless Chrome).
    let wd = std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://localhost:9515".into());
    let mut caps = DesiredCapabilities::chrome();
    caps.add_arg("--headless=new").unwrap();
    caps.add_arg("--no-sandbox").unwrap();
    caps.add_arg("--disable-gpu").unwrap();
    caps.add_arg("--disable-dev-shm-usage").unwrap();
    // The header says CHROME_BIN selects the browser; without this it did
    // nothing, and chromedriver picked up whatever Chrome was installed, which
    // fails when that version is not the one chromedriver was built for.
    if let Ok(bin) = std::env::var("CHROME_BIN") {
        caps.set_binary(&bin).unwrap();
    }
    let driver = WebDriver::new(&wd, caps)
        .await
        .expect("connect to WebDriver - is `chromedriver --port=9515` running?");

    // 4. Drive the viewer and assert the archived page rendered. Always tear
    //    down the browser session and server, even on failure.
    let result = drive_and_check(&driver, addr, &id).await;
    let _ = driver.quit().await;
    server.abort();
    result.unwrap();
}

async fn drive_and_check(driver: &WebDriver, addr: SocketAddr, id: &str) -> Result<(), String> {
    // The 200 HTML capture of the story page (the arcg.is seed is a 301 to it).
    let page_url = "https://storymaps.arcgis.com/stories/278e1b5c18a3474082e583e889705179";
    let url = format!(
        "http://{addr}/replay/viewer?source=/files/{id}&url={page_url}&ts=20260609213407&name=2Tone"
    );
    driver.goto(&url).await.map_err(|e| e.to_string())?;

    // The banner is rendered by our own viewer JS immediately - a quick sanity
    // check that the viewer page itself loaded (it shows the current URL).
    if !deep_contains(driver, "storymaps.arcgis.com").await? {
        return Err("viewer banner (current URL) did not appear".into());
    }

    // wabac must install its service worker, read the WACZ over byte-range,
    // load its CDX, and render the page inside the viewer's replay frame. Poll
    // for the archived page's title text ("2Tone", present in the served HTML
    // <title>) to appear anywhere in that frame's tree.
    enter_replay_frame(driver).await?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut rendered = false;
    while Instant::now() <= deadline {
        if deep_contains(driver, "2Tone").await? {
            rendered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if !rendered {
        let diag = diagnostics(driver).await.unwrap_or_else(|e| e);
        return Err(format!(
            "archived page title '2Tone' never rendered.\nDIAG: {diag}"
        ));
    }
    driver
        .enter_default_frame()
        .await
        .map_err(|e| e.to_string())?;

    check_banner_tracks_navigation(driver).await
}

/// Verify the banner updates when ReplayWeb.page fires `rwp-url-change`. wabac
/// dispatches this (non-bubbling) event on the <replay-web-page> element as the
/// user navigates; our viewer listens on the element and updates the banner. We
/// dispatch a synthetic event so the check is deterministic (no reliance on
/// clicking links inside the SPA).
async fn check_banner_tracks_navigation(driver: &WebDriver) -> Result<(), String> {
    let new_url = "https://storymaps.arcgis.com/stories/navigation-check";
    // Fire it where ReplayWeb.page fires it, inside the replay frame. The frame's
    // agent passes it to the viewer by `postMessage`, and the viewer updates the
    // banner, so this exercises that whole path.
    enter_replay_frame(driver).await?;
    driver
        .execute(
            "document.querySelector('replay-web-page')\
               .dispatchEvent(new CustomEvent('rwp-url-change', {detail: {url: arguments[0]}}));",
            vec![serde_json::json!(new_url)],
        )
        .await
        .map_err(|e| e.to_string())?;
    driver
        .enter_default_frame()
        .await
        .map_err(|e| e.to_string())?;

    // Polled, not read once after a fixed pause: the message crosses a frame
    // boundary, and ReplayWeb.page may later report the page's real URL itself.
    // What this checks is that the banner follows the event, so seeing the URL
    // once is the pass.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = String::new();
    while Instant::now() <= deadline {
        let banner = driver
            .execute(
                "return document.getElementById('current-url').textContent;",
                vec![],
            )
            .await
            .map_err(|e| e.to_string())?;
        last = banner.json().as_str().unwrap_or("").to_string();
        if last == new_url {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "banner did not track rwp-url-change: expected {new_url}, got {last:?}"
    ))
}

/// Switch into the viewer's replay frame, where ReplayWeb.page lives.
///
/// Through WebDriver rather than `contentDocument`, so it works whether the
/// frame shares the viewer's origin or, as replay is meant to, runs on its own.
async fn enter_replay_frame(driver: &WebDriver) -> Result<(), String> {
    driver
        .find(By::Id("replay-frame"))
        .await
        .map_err(|e| format!("the viewer has no replay frame: {e}"))?
        .enter_frame()
        .await
        .map_err(|e| e.to_string())
}

async fn diagnostics(driver: &WebDriver) -> Result<String, String> {
    let script = r#"
        const out = {};
        out.loc = location.href;
        out.swController = !!(navigator.serviceWorker && navigator.serviceWorker.controller);
        const rwp = document.querySelector('replay-web-page');
        out.rwp = !!rwp;
        out.rwpShadow = !!(rwp && rwp.shadowRoot);
        const frames = [];
        function walk(node, depth) {
            if (!node) return;
            if (node.shadowRoot) walk(node.shadowRoot, depth);
            if (node.tagName === 'IFRAME') {
                let info = { src: node.getAttribute('src'), same: false, sample: null };
                try { if (node.contentDocument) { info.same = true;
                    info.sample = (node.contentDocument.body ? node.contentDocument.body.innerText : '').slice(0,120); } }
                catch (e) { info.err = String(e); }
                frames.push(info);
                try { if (node.contentDocument) walk(node.contentDocument, depth+1); } catch(e){}
            }
            const kids = node.childNodes || [];
            for (let i=0;i<kids.length;i++) walk(kids[i], depth);
        }
        walk(document, 0);
        out.frames = frames;
        return JSON.stringify(out);
    "#;
    let ret = driver
        .execute(script, vec![])
        .await
        .map_err(|e| e.to_string())?;
    Ok(ret.json().as_str().unwrap_or("<no diag>").to_string())
}

/// Return true if `needle` appears in any text node reachable from the document,
/// piercing shadow roots and same-origin iframes. ReplayWeb.page renders into a
/// shadow DOM and a replay iframe served from our own origin, so both are
/// reachable from page JS.
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

/// A note made through the viewer, on a passage selected inside the replay.
///
/// The notes panel and the replayed page live in different frames, and this is
/// the round trip between them: the curator's selection is read inside the
/// replay frame and reported to the viewer, the viewer writes the note, and the
/// viewer sends the highlight back into the frame to be drawn. Every hop is a
/// `postMessage`, so this is the test that the split works, and the one that
/// keeps working once the frame runs on its own origin.
///
/// As a workstation, so the viewer may annotate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running WebDriver (chromedriver) and browser; run with --ignored"]
async fn a_note_is_made_and_highlighted_through_the_viewer() {
    let tmp = tempfile::TempDir::new().unwrap();
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, "notes").unwrap();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();
    let (addr, server) = serve(tmp.path(), workstation()).await;

    let wd = std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://localhost:9515".into());
    let mut caps = DesiredCapabilities::chrome();
    if let Ok(bin) = std::env::var("CHROME_BIN") {
        caps.set_binary(&bin).unwrap();
    }
    caps.add_arg("--headless=new").unwrap();
    caps.add_arg("--no-sandbox").unwrap();
    caps.add_arg("--disable-gpu").unwrap();
    caps.add_arg("--disable-dev-shm-usage").unwrap();
    let driver = WebDriver::new(&wd, caps)
        .await
        .expect("connect to WebDriver - is `chromedriver --port=9515` running?");

    let result = annotate_through_viewer(&driver, addr, &id).await;
    let _ = driver.quit().await;
    server.abort();
    result.unwrap();
}

/// Find the replayed page's document from inside the replay frame, through
/// ReplayWeb.page's shadow root and nested frames, the way the agent does.
const FIND_REPLAY_DOC: &str = r#"
    function findReplayDoc(root, depth) {
        if (depth > 8) return null;
        let best = null;
        const frames = root.querySelectorAll ? root.querySelectorAll('iframe') : [];
        for (const f of frames) {
            let d = null;
            try { d = f.contentDocument; } catch (e) { continue; }
            if (!d) continue;
            const href = (d.location && d.location.href) || '';
            if (href.indexOf('/replay/w/') !== -1 && href.indexOf('mp_/') !== -1 && d.body) best = d;
            const deeper = findReplayDoc(d, depth + 1);
            if (deeper) best = deeper;
        }
        if (root.querySelectorAll) {
            for (const el of root.querySelectorAll('*')) {
                if (el.shadowRoot) { const deeper = findReplayDoc(el.shadowRoot, depth + 1); if (deeper) best = deeper; }
            }
        }
        return best;
    }
"#;

async fn annotate_through_viewer(
    driver: &WebDriver,
    addr: SocketAddr,
    id: &str,
) -> Result<(), String> {
    let page_url = "https://storymaps.arcgis.com/stories/278e1b5c18a3474082e583e889705179";
    let viewer = format!(
        "http://{addr}/replay/viewer?source=/files/{id}&url={page_url}&ts=20260609213407\
         &name=2Tone&collection=notes&collection_id=notes"
    );
    driver.goto(&viewer).await.map_err(|e| e.to_string())?;

    // Wait for the archived page, then select a passage in it. The agent only
    // listens for selections once it has found the document, which it looks for
    // once a second, so keep selecting until the viewer reports one.
    enter_replay_frame(driver).await?;
    let deadline = Instant::now() + Duration::from_secs(45);
    while !deep_contains(driver, "2Tone").await? {
        if Instant::now() > deadline {
            let diag = diagnostics(driver).await.unwrap_or_else(|e| e);
            return Err(format!("the archived page never rendered.\nDIAG: {diag}"));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // Back to the viewer: each round below enters the frame from there.
    driver
        .enter_default_frame()
        .await
        .map_err(|e| e.to_string())?;
    let select = format!(
        "{FIND_REPLAY_DOC}
        const doc = findReplayDoc(document, 0);
        if (!doc) return '';
        const walker = doc.createTreeWalker(doc.body, NodeFilter.SHOW_TEXT);
        let n;
        while ((n = walker.nextNode())) {{
            const at = n.data.search(/\\S/);
            if (at < 0 || n.data.trim().length < 24) continue;
            const r = doc.createRange();
            r.setStart(n, at);
            r.setEnd(n, at + 16);
            const s = doc.defaultView.getSelection();
            s.removeAllRanges();
            s.addRange(r);
            return s.toString().trim();
        }}
        return '';"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let selected = loop {
        enter_replay_frame(driver).await?;
        let selected = driver
            .execute(&select, vec![])
            .await
            .map_err(|e| e.to_string())?
            .json()
            .as_str()
            .unwrap_or("")
            .to_string();
        driver
            .enter_default_frame()
            .await
            .map_err(|e| e.to_string())?;
        let enabled = driver
            .execute(
                "const b = document.getElementById('anno-sel'); return !!b && !b.disabled;",
                vec![],
            )
            .await
            .map_err(|e| e.to_string())?
            .json()
            .as_bool()
            .unwrap_or(false);
        if enabled && !selected.is_empty() {
            break selected;
        }
        if Instant::now() > deadline {
            return Err(format!(
                "the viewer never learned of the selection (selected {selected:?})"
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    // Add the note from the viewer. The composer has to say what the note will
    // be attached to, because that came from the frame.
    driver
        .find(By::Id("anno-sel"))
        .await
        .map_err(|e| e.to_string())?
        .click()
        .await
        .map_err(|e| e.to_string())?;
    let hint = driver
        .find(By::ClassName("anno-hint"))
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    if !hint.contains(page_url) || !hint.contains(&selected) {
        return Err(format!(
            "the composer should name the target URL and quote {selected:?}, got {hint:?}"
        ));
    }
    driver
        .find(By::Css(".anno-composer textarea"))
        .await
        .map_err(|e| e.to_string())?
        .send_keys("a note from the browser test")
        .await
        .map_err(|e| e.to_string())?;
    driver
        .find(By::Css(".anno-composer button[type=submit]"))
        .await
        .map_err(|e| e.to_string())?
        .click()
        .await
        .map_err(|e| e.to_string())?;

    // It was written, with the passage selected in the frame.
    let url = format!("http://{addr}/api/annotations?collection=notes");
    let deadline = Instant::now() + Duration::from_secs(10);
    let saved = loop {
        let body = tokio::task::spawn_blocking({
            let url = url.clone();
            move || {
                ureq::get(&url)
                    .call()
                    .ok()
                    .and_then(|mut r| r.body_mut().read_to_string().ok())
            }
        })
        .await
        .unwrap()
        .unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        if let Some(a) = json["annotations"]
            .as_array()
            .and_then(|a| a.first())
            .cloned()
        {
            break a;
        }
        if Instant::now() > deadline {
            return Err(format!("no note was saved: {body}"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    if saved["selector"]["exact"].as_str() != Some(selected.as_str()) {
        return Err(format!(
            "the note should quote the selected passage {selected:?}, saved {saved}"
        ));
    }

    // And the viewer had the frame draw it.
    let deadline = Instant::now() + Duration::from_secs(10);
    let check = format!(
        "{FIND_REPLAY_DOC}
        const doc = findReplayDoc(document, 0);
        const w = doc && doc.defaultView;
        return !!(w && w.CSS && w.CSS.highlights && w.CSS.highlights.has('indice-anno'));"
    );
    loop {
        enter_replay_frame(driver).await?;
        let drawn = driver
            .execute(&check, vec![])
            .await
            .map_err(|e| e.to_string())?
            .json()
            .as_bool()
            .unwrap_or(false);
        driver
            .enter_default_frame()
            .await
            .map_err(|e| e.to_string())?;
        if drawn {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("the note was saved but never highlighted in the replay".into());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The page a hostile capture serves. Its script does what an attacker who got
/// a page crawled would: climb out of the archived document to ReplayWeb.page's
/// own window, whose `fetch` wombat has not rewritten, and from there try to
/// create a collection two ways. A relative URL goes to whatever origin replay
/// runs on; an absolute one goes to the viewer's origin, which the frame's own
/// URL names in `parent=`. It also tries to reach into the viewer's DOM. What
/// it managed is left on `body[data-hostile]` for the test to read.
const HOSTILE_PAGE: &str = r#"<!DOCTYPE html>
<html><head><title>Hostile capture</title></head>
<body><h1>Hostile capture</h1>
<script>
(async function () {
  var out = { climbed: [] };
  var host = null;
  // Wombat makes `parent`, `top` and `location` describe the archived site,
  // but keeps the real outer window in a global any page can read. Climb
  // from there to ReplayWeb.page's own page.
  var w = window.__WB_top_frame || window;
  for (var i = 0; i < 12; i++) {
    try {
      out.climbed.push(w.location.pathname);
      if (w.location.pathname === '/replay-frame') { host = w; break; }
    } catch (e) { out.climbed.push('blocked: ' + e.name); break; }
    var p = null;
    try { p = w.parent; } catch (e) { break; }
    if (!p || p === w) break;
    w = p;
  }
  function post(url, name) {
    return host.fetch(url, {
      method: 'POST',
      credentials: 'include',
      headers: { 'content-type': 'application/x-www-form-urlencoded' },
      body: 'name=' + name
    }).then(function (r) { return r.status; }, function (e) { return 'error: ' + e.message; });
  }
  if (host) {
    out.relative = await post('/api/collections', 'pwned-relative');
    out.app = new URLSearchParams(host.location.search).get('parent');
    if (out.app) out.absolute = await post(out.app + '/api/collections', 'pwned-absolute');
    try {
      out.viewer = host.parent.document.title;
    } catch (e) {
      out.viewer = 'blocked: ' + e.name;
    }
  }
  document.body.setAttribute('data-hostile', JSON.stringify(out));
})();
</script></body></html>"#;

/// A one-page WACZ of [`HOSTILE_PAGE`], built from a WARC written here, so no
/// opaque binary is committed and the attack is readable in this file.
fn hostile_wacz(dir: &Path) -> std::path::PathBuf {
    let body = HOSTILE_PAGE.as_bytes();
    let mut http = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    http.extend_from_slice(body);
    let mut warc = format!(
        "WARC/1.0\r\n\
         WARC-Type: response\r\n\
         WARC-Record-ID: <urn:uuid:6f1c2a52-3d0e-4c39-9a51-2f4b8a7c0d11>\r\n\
         WARC-Date: 2026-01-01T00:00:00Z\r\n\
         WARC-Target-URI: https://hostile.example/\r\n\
         Content-Type: application/http; msgtype=response\r\n\
         Content-Length: {}\r\n\r\n",
        http.len()
    )
    .into_bytes();
    warc.extend_from_slice(&http);
    warc.extend_from_slice(b"\r\n\r\n");
    let warc_path = dir.join("hostile.warc");
    std::fs::write(&warc_path, warc).unwrap();
    indice_lib::wacz_build::build_wacz(
        &[warc_path],
        &indice_lib::wacz_build::WaczBuildMeta::default(),
        dir,
        "hostile",
    )
    .unwrap()
    .path
}

/// Archived JavaScript cannot act as the person replaying it.
///
/// As a workstation, where every caller is the operator, so a write that gets
/// through needs no credentials at all: this is the shape with the least
/// between a capture and the write API. The assertion is on the side effect,
/// the manifest, not on anything the page reports, and the page's own report
/// is checked first so that a script that never ran cannot pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running WebDriver (chromedriver) and browser; run with --ignored"]
async fn a_hostile_capture_cannot_write_as_the_reader() {
    let tmp = tempfile::TempDir::new().unwrap();
    let build = tempfile::TempDir::new().unwrap();
    let wacz = hostile_wacz(build.path());
    indice_lib::index::index_path(&wacz, tmp.path(), None, "hostile").unwrap();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();
    let before = manifest.collections.len();

    let (addr, server) = serve(tmp.path(), workstation()).await;

    let driver = connect().await;
    let result = replay_hostile(&driver, addr, &id).await;
    let _ = driver.quit().await;
    server.abort();
    let report = result.unwrap();

    // The attack has to have been made for its failure to mean anything: the
    // script reached ReplayWeb.page's own window and tried both writes.
    let r: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert!(
        r["relative"].is_number() || r["relative"].is_string(),
        "the relative write was never attempted: {report}"
    );
    assert!(
        r["absolute"].is_number() || r["absolute"].is_string(),
        "the write to the app origin was never attempted: {report}"
    );
    assert!(
        r["viewer"]
            .as_str()
            .is_some_and(|v| v.starts_with("blocked")),
        "archived script reached into the viewer: {report}"
    );

    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let pwned: Vec<_> = manifest
        .collections
        .iter()
        .map(|c| c.name.as_str())
        .filter(|n| n.starts_with("pwned"))
        .collect();
    assert!(
        pwned.is_empty(),
        "archived script created {pwned:?} as the reader.\nThe page reported: {report}"
    );
    assert_eq!(manifest.collections.len(), before, "{report}");
}

/// Replay the hostile page and return what its script reported doing.
async fn replay_hostile(driver: &WebDriver, addr: SocketAddr, id: &str) -> Result<String, String> {
    let viewer = format!(
        "http://{addr}/replay/viewer?source=/files/{id}&url=https://hostile.example/\
         &ts=20260101000000&name=Hostile"
    );
    driver.goto(&viewer).await.map_err(|e| e.to_string())?;
    enter_replay_frame(driver).await?;
    let read = format!(
        "{FIND_REPLAY_DOC}
        const doc = findReplayDoc(document, 0);
        return doc && doc.body ? doc.body.getAttribute('data-hostile') : null;"
    );
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let report = driver
            .execute(&read, vec![])
            .await
            .map_err(|e| e.to_string())?;
        if let Some(r) = report.json().as_str() {
            // The script awaits each write before reporting, so by now the
            // server has answered them.
            return Ok(r.to_string());
        }
        if Instant::now() > deadline {
            let diag = diagnostics(driver).await.unwrap_or_else(|e| e);
            return Err(format!("the hostile page never ran.\nDIAG: {diag}"));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// A headless Chrome session, honouring `WEBDRIVER_URL` and `CHROME_BIN`.
async fn connect() -> WebDriver {
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
        .expect("connect to WebDriver - is `chromedriver --port=9515` running?")
}

/// Opened on its own, as a top-level page on the replay origin, the frame
/// refuses to replay. ReplayWeb.page's `requireSubdomainIframe` does that;
/// this is the test that the attribute `frame.html` sets is the one it reads.
///
/// Framed, replay works, which every other test here shows. That says nothing
/// about the flag, since an ignored flag would pass them too; this does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running WebDriver (chromedriver) and browser; run with --ignored"]
async fn the_replay_frame_refuses_to_replay_unframed() {
    let tmp = tempfile::TempDir::new().unwrap();
    indice_lib::index::index_path(&fixture("a.wacz"), tmp.path(), None, "test").unwrap();
    let manifest = indice_lib::collections::Manifest::open(&tmp.path().join("index")).unwrap();
    let id = manifest.waczs[0].id.clone();
    let (addr, server) = serve(tmp.path(), public_config(tmp.path())).await;

    let driver = connect().await;
    let result = open_frame_unframed(&driver, addr, &id).await;
    let _ = driver.quit().await;
    server.abort();
    result.unwrap();
}

async fn open_frame_unframed(driver: &WebDriver, addr: SocketAddr, id: &str) -> Result<(), String> {
    // The replay origin, as the viewer is told it.
    driver
        .goto(&format!("http://{addr}/replay/viewer?source=/files/{id}"))
        .await
        .map_err(|e| e.to_string())?;
    let replay = driver
        .execute(
            "return document.querySelector('meta[name=\"indice-replay-origin\"]').content;",
            vec![],
        )
        .await
        .map_err(|e| e.to_string())?
        .json()
        .as_str()
        .unwrap_or("")
        .to_string();
    if replay.is_empty() {
        return Err("the viewer names no replay origin".into());
    }

    let page_url = "https://storymaps.arcgis.com/stories/278e1b5c18a3474082e583e889705179";
    driver
        .goto(&format!(
            "{replay}/replay-frame?source=/files/{id}&url={page_url}&ts=20260609213407"
        ))
        .await
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() <= deadline {
        if deep_contains(driver, "2Tone").await? {
            return Err("the frame replayed the archive with nothing framing it".into());
        }
        if deep_contains(driver, "subdomain iframe").await? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let diag = diagnostics(driver).await.unwrap_or_else(|e| e);
    Err(format!(
        "the unframed frame neither refused nor replayed.\nDIAG: {diag}"
    ))
}
