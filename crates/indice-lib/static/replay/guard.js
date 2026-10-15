// indice's guard for the replay origin, served at the top of /replay/ui.js
// (see `replay_asset` in server/assets.rs), so it runs before ReplayWeb.page
// does, in every page that loads it.
//
// ReplayWeb.page replays whatever WACZ its page's address names. Unchecked,
// anyone could show a WACZ of their own making under an institution's replay
// hostname, or load content into a reader's replay storage under a
// collection name of their choosing. So replay starts only for a source that
// is one of indice's own paths on this origin, and only in the two pages that
// indice itself puts there:
//
// - /replay-frame, which checks its source with `window.indiceSource` before
//   mounting <replay-web-page>;
// - ReplayWeb.page's shell at /replay/, which <replay-web-page> frames. The
//   service worker serves that shell itself once installed, without asking the
//   server, so the check has to be here, in a script both versions load. The
//   shell must be framed by /replay-frame on this origin, and its source and
//   collection must agree with each other.
//
// Refusing means throwing: this guard and ReplayWeb.page are one script, so
// an exception here stops ReplayWeb.page from ever defining its elements.
(function () {
  "use strict";

  /**
   * One of indice's own replay sources, as a path on this origin, with the
   * collection name ReplayWeb.page caches it under. Null for anything else.
   * @param {string} source
   * @returns {{path: string, coll: string} | null}
   */
  function indiceSource(source) {
    if (!source) return null;
    var url;
    try {
      url = new URL(source, window.location.origin);
    } catch (e) {
      return null;
    }
    if (url.origin !== window.location.origin || url.search || url.hash) return null;
    var m =
      url.pathname.match(/^\/files\/([^/]+)$/) ||
      url.pathname.match(/^\/collection\/([^/]+)\/replay\.json$/);
    return m ? { path: url.pathname, coll: decodeURIComponent(m[1]) } : null;
  }
  window.indiceSource = indiceSource;

  var path = window.location.pathname;
  if (path !== "/replay/" && path !== "/replay/index.html") return;

  // The shell. Framed by our own frame, on this origin, or not at all.
  var framedByUs = false;
  try {
    framedByUs =
      window.parent !== window &&
      window.parent.location.origin === window.location.origin &&
      window.parent.location.pathname === "/replay-frame";
  } catch (e) {
    framedByUs = false; // a parent on another origin
  }
  var params = new URLSearchParams(window.location.search);
  var source = indiceSource(params.get("source") || "");
  if (framedByUs && source && params.get("customColl") === source.coll) return;

  throw new Error(
    "indice: replay refused. This page replays only this server's own archives, in indice's replay frame."
  );
})();
