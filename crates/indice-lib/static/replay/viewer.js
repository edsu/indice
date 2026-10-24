// The replay viewer: the banner, the frame, and the one channel to it.
//
// A file rather than an inline <script>, so the app origin's
// Content-Security-Policy can refuse inline script (server/headers.rs). It is
// loaded at the end of <body>, after annotations.js, and runs before
// DOMContentLoaded, which is when the notes panel starts and first uses the
// channel set up here.

(function () {
  const params = new URLSearchParams(window.location.search);
  const source = params.get('source'); // e.g. /files/a926c132
  const url    = params.get('url') || '';
  const name   = params.get('name') || '';
  const collection   = params.get('collection') || '';
  const collectionId = params.get('collection_id') || '';
  const crawlId      = params.get('crawl') || '';

  if (!source) {
    document.body.innerHTML =
      '<div id="error">Missing <code>source</code> parameter.</div>';
    return;
  }

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
  // Without a name, the id in the source's path, as the frame names its
  // cache: the crawl's for /files/{id}, the collection's for its manifest.
  const fromPath = source.match(/^\/(?:files|collection)\/([^/]+)/);
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
  showCaptured(params.get('ts'));

  // The frame's origin, from the server's configuration and never from a
  // message, because the check below is what everything trusting a
  // message depends on. It is a different origin from this page's, so
  // archived script in the frame can neither reach this page nor make a
  // request to indice that indice will take as this page's.
  const frameOrigin =
    document.querySelector('meta[name="indice-replay-origin"]').content;

  const frameParams = new URLSearchParams(params);
  frameParams.set('parent', window.location.origin);
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

  // Keep the banner URL in step as the reader navigates the archive.
  window.addEventListener('indice-replay', function (e) {
    const d = e.detail;
    if (d.type === 'url' && typeof d.url === 'string' && d.url) {
      currentUrl.textContent = d.url;
      if (typeof d.ts === 'string') showCaptured(d.ts);
    }
  });
})();
