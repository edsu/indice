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
  // Without a name, the same fallback the frame uses for wabac's collection
  // id: a collection's manifest ends in the shared "replay.json", so show
  // the collection id rather than that.
  nameEl.textContent = name || params.get('coll') ||
    (source.endsWith('/replay.json') ? (collectionId || 'collection') : source.split('/').pop());
  if (crawlId) nameEl.href = '/crawl/' + encodeURIComponent(crawlId);
  const currentUrl = document.getElementById('current-url');
  currentUrl.textContent = url;

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
    }
  });
})();
