// The replay viewer: the banner, the frame, and the one channel to it.
//
// A file rather than an inline <script>, so the app origin's
// Content-Security-Policy can refuse inline script (server/headers.rs). It is
// loaded at the end of <body>, after annotations.js, and runs before
// DOMContentLoaded, which is when the notes panel starts and first uses the
// channel set up here.

(function () {
  // What to replay, worked out by the server from this page's address
  // (/crawl/{id}/{ts}/{url} or /collection/{id}/{ts}/{url}).
  const ctx = JSON.parse(document.getElementById('replay-context').textContent);
  const source = ctx.source; // e.g. /files/a926c132
  const url    = ctx.url;
  const name   = ctx.name;
  const collection   = ctx.collection;
  const collectionId = ctx.collectionId;
  const crawlId      = ctx.crawl;

  // Breadcrumb: indice › <collection> › <crawl name> › <url>. The
  // collection crumb links to its collection page, and the crawl crumb to the
  // crawl detail page, when their ids are known.
  const collEl = document.getElementById('collection');
  if (collection) {
    collEl.textContent = collection;
    if (collectionId) collEl.href = '/collection/' + encodeURIComponent(collectionId);
  } else {
    collEl.style.display = 'none';
    document.getElementById('collection-sep').style.display = 'none';
  }
  const nameEl = document.getElementById('collection-name');
  // Without a name, the id in the source's path: the crawl's for /files/{id}
  // and its manifest, the collection's for its manifest.
  const fromPath = source.match(/^\/(?:files|crawl|collection)\/([^/]+)/);
  nameEl.textContent = name || (fromPath ? fromPath[1] : source);
  if (crawlId) nameEl.href = '/crawl/' + encodeURIComponent(crawlId);
  // A collection's replay has no one crawl to name, and is given the
  // collection's own name; saying it twice tells the reader nothing.
  if (!crawlId && collection && nameEl.textContent === collection) {
    nameEl.style.display = 'none';
    document.getElementById('crawl-sep').style.display = 'none';
  }
  const currentUrl = document.getElementById('current-url');
  currentUrl.textContent = url;

  // When the page in view was captured: the provenance a reader most needs
  // while reading it, so the banner leads with it. From the link's `ts`, then
  // from the frame as the reader moves between pages.
  const captured = document.getElementById('captured');
  const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July',
    'August', 'September', 'October', 'November', 'December'];
  function showCaptured(ts) {
    const d = String(ts || '').replace(/\D/g, '');
    const month = MONTHS[Number(d.slice(4, 6)) - 1];
    if (d.length < 8 || !month) {
      captured.textContent = '';
      return;
    }
    let text = 'Captured ' + Number(d.slice(6, 8)) + ' ' + month + ' ' + d.slice(0, 4);
    if (d.length >= 12) text += ', ' + d.slice(8, 10) + ':' + d.slice(10, 12) + ' UTC';
    captured.textContent = text;
  }
  showCaptured(ctx.ts);

  // The frame's origin, from the server's configuration and never from a
  // message, because the check below is what everything trusting a
  // message depends on. It is a different origin from this page's, so
  // archived script in the frame can neither reach this page nor make a
  // request to indice that indice will take as this page's.
  const frameOrigin =
    document.querySelector('meta[name="indice-replay-origin"]').content;

  const frameParams = new URLSearchParams({
    source: source,
    url: url,
    ts: ctx.ts,
    parent: window.location.origin,
  });
  const frame = document.createElement('iframe');
  frame.id = 'replay-frame';
  frame.title = 'Archived page';
  frame.allow = 'autoplay; fullscreen';
  frame.src = frameOrigin + '/replay-frame?' + frameParams.toString();
  document.getElementById('replay-container').appendChild(frame);

  // The channel to the frame, and the one place its messages are checked.
  // A message counts only if it comes from our own frame and from the
  // origin we put it on; valid ones are re-dispatched as `indice-replay`
  // events for the banner and the notes panel. Even a valid one is only a
  // suggestion: archived script shares the frame's origin and can send
  // anything the frame's agent can.
  window.indiceReplay = {
    send: function (type, payload) {
      if (!frame.contentWindow) return;
      frame.contentWindow.postMessage(
        Object.assign({ indice: 'replay', type: type }, payload),
        frameOrigin
      );
    },
  };
  window.addEventListener('message', function (e) {
    if (e.source !== frame.contentWindow || e.origin !== frameOrigin) return;
    const d = e.data;
    if (!d || d.indice !== 'replay' || typeof d.type !== 'string') return;
    window.dispatchEvent(new CustomEvent('indice-replay', { detail: d }));
  });

  // The archived URL as the tail of this page's address, as the server
  // writes it (`url_in_path` in server/util.rs): parsed by the URL standard,
  // which resolves `.` and `..` segments, and without its fragment.
  function inPath(u) {
    let parsed;
    try {
      parsed = new URL(u);
    } catch (_) {
      return null;
    }
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return null;
    parsed.hash = '';
    return parsed.href;
  }

  // Keep the banner, and the address, in step as the reader navigates the
  // archive, so the address in the bar is always one that opens the page in
  // view. The frame's report is only a suggestion (see above), so the new
  // address must still be a page of this crawl or collection once the browser
  // has resolved it: archived script can choose which page, never somewhere
  // else on the site.
  window.addEventListener('indice-replay', function (e) {
    const d = e.detail;
    if (d.type !== 'url' || typeof d.url !== 'string') return;
    const tail = inPath(d.url);
    if (!tail) return;
    currentUrl.textContent = d.url;
    if (typeof d.ts === 'string') showCaptured(d.ts);
    const ts = typeof d.ts === 'string' ? d.ts.replace(/\D/g, '').slice(0, 14) : '';
    const next = new URL(ctx.base + '/' + (ts ? ts + '/' : '') + tail, window.location.origin);
    if (next.origin !== window.location.origin || !next.pathname.startsWith(ctx.base + '/')) {
      return;
    }
    const path = next.pathname + next.search;
    if (path !== window.location.pathname + window.location.search) {
      history.replaceState(null, '', path + window.location.hash);
    }
  });
})();
