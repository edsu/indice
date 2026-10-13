// @ts-check
//
// indice replay frame agent: the half of page annotations that has to touch
// the replayed document. It runs in /replay-frame beside <replay-web-page>,
// finds the replayed page's document, reports the curator's text selection
// and the current URL to the viewer, and draws the highlights the viewer
// sends. It never talks to the API; the viewer does that.
//
// Why split: the replayed page's own JavaScript runs in this frame's origin.
// Anything this agent can do, archived script can do too, including sending
// the viewer any message this agent could send. So the viewer treats every
// message from here as a suggestion, and nothing this agent receives grants
// anything. See DESIGN.md, "Threat Model".
//
// Highlights use the CSS Custom Highlight API so the replayed page's DOM is
// never mutated: Ranges are registered over its content document, which is
// reached through ReplayWeb.page's nested same-origin frames (the gnqf.1
// spike). Reaching RWP's private frame structure is unsupported, so the
// traversal is deliberately defensive and must be re-checked on ReplayWeb.page
// upgrades.

(function () {
  "use strict";

  const HL = "indice-anno";
  const HL_ACTIVE = "indice-anno-active";

  /** @typedef {{exact:string, prefix?:string, suffix?:string}} Selector */
  /** @typedef {{id:string, selector:Selector}} Note */

  const params = new URLSearchParams(window.location.search);
  // Where to send messages. Taken from our own URL rather than from any
  // message: the viewer that framed us put it there. Not a security boundary
  // in this direction (nothing we receive is trusted with anything), only
  // the address to write to.
  const parentOrigin = params.get("parent") || window.location.origin;

  const state = {
    /** @type {Document|null} */ doc: null,
    styleInjected: false,
    /** @type {Note[]} */ notes: [],
    /** @type {Map<string, Range>} */ ranges: new Map(),
  };

  // ── Messages ──────────────────────────────────────────────────────────────
  function send(type, payload) {
    if (window.parent === window) return;
    window.parent.postMessage(Object.assign({ indice: "replay", type }, payload), parentOrigin);
  }

  window.addEventListener("message", (e) => {
    if (e.source !== window.parent || e.origin !== parentOrigin) return;
    const d = e.data;
    if (!d || d.indice !== "replay" || typeof d.type !== "string") return;
    // Only a viewer with a notes panel talks to us, so that is when the
    // document is worth looking for. A plain replay never pays for the walk.
    watchReplayDoc();
    if (d.type === "highlight" && Array.isArray(d.notes)) {
      state.notes = d.notes.filter(
        (n) => n && typeof n.id === "string" && n.selector && typeof n.selector.exact === "string"
      );
      applyHighlights();
    } else if (d.type === "focus" && typeof d.id === "string") {
      focusRange(d.id);
    } else if (d.type === "clear-selection") {
      clearSelection();
    }
  });

  // ── In-place highlighting (CSS Custom Highlight API) ──────────────────────
  function highlightSupported(doc) {
    const w = doc && doc.defaultView;
    return !!(w && w.CSS && w.CSS.highlights && w.Highlight);
  }

  function injectHighlightStyle(doc) {
    if (state.styleInjected || !doc.head) return;
    const s = doc.createElement("style");
    s.textContent =
      "::highlight(" + HL + "){background:rgba(255,214,10,.40);}" +
      "::highlight(" + HL_ACTIVE + "){background:rgba(255,171,0,.85);}";
    doc.head.appendChild(s);
    state.styleInjected = true;
  }

  function applyHighlights() {
    state.ranges.clear();
    const doc = state.doc;
    if (!doc || !highlightSupported(doc)) return;
    injectHighlightStyle(doc);
    const w = /** @type {any} */ (doc.defaultView);
    const ranges = [];
    for (const n of state.notes) {
      const r = rangeForQuote(doc, n.selector);
      if (r) {
        state.ranges.set(n.id, r);
        ranges.push(r);
      }
    }
    try {
      // Also clear any focused (click-emphasised) highlight, so a stale one
      // from the previous page doesn't linger over unrelated content.
      w.CSS.highlights.delete(HL_ACTIVE);
      w.CSS.highlights.delete(HL);
      if (ranges.length) w.CSS.highlights.set(HL, new w.Highlight(...ranges));
    } catch (e) {
      /* ignore */
    }
  }

  function focusRange(id) {
    const doc = state.doc;
    const r = state.ranges.get(id);
    if (!doc || !r || !highlightSupported(doc)) return;
    const w = /** @type {any} */ (doc.defaultView);
    try {
      w.CSS.highlights.delete(HL_ACTIVE);
      w.CSS.highlights.set(HL_ACTIVE, new w.Highlight(r.cloneRange()));
    } catch (e) {}
    const node = r.startContainer;
    const el = node.nodeType === 3 ? node.parentElement : /** @type {Element} */ (node);
    if (el && el.scrollIntoView) el.scrollIntoView({ block: "center", behavior: "smooth" });
  }

  /**
   * Find the DOM Range for a TextQuoteSelector by scanning the document's text
   * nodes. Prefers a match where prefix/suffix line up (to disambiguate repeats).
   * @param {Document} doc @param {Selector} sel @returns {Range|null}
   */
  function rangeForQuote(doc, sel) {
    if (!sel.exact) return null;
    const walker = doc.createTreeWalker(doc.body, NodeFilter.SHOW_TEXT);
    /** @type {{node:Text, start:number}[]} */
    const map = [];
    let text = "";
    let n;
    while ((n = walker.nextNode())) {
      const t = /** @type {Text} */ (n);
      map.push({ node: t, start: text.length });
      text += t.data;
    }
    const want = (sel.prefix || "") + sel.exact + (sel.suffix || "");
    let at = want ? text.indexOf(want) : -1;
    let exactStart;
    if (at >= 0) {
      exactStart = at + (sel.prefix || "").length;
    } else {
      exactStart = text.indexOf(sel.exact);
      if (exactStart < 0) return null;
    }
    const exactEnd = exactStart + sel.exact.length;
    const startLoc = locate(map, exactStart);
    const endLoc = locate(map, exactEnd);
    if (!startLoc || !endLoc) return null;
    try {
      const r = doc.createRange();
      r.setStart(startLoc.node, startLoc.offset);
      r.setEnd(endLoc.node, endLoc.offset);
      return r;
    } catch (e) {
      return null;
    }
  }

  /** Map a global text offset to (text node, node-local offset). */
  function locate(map, offset) {
    for (let i = map.length - 1; i >= 0; i--) {
      if (offset >= map[i].start) {
        return { node: map[i].node, offset: offset - map[i].start };
      }
    }
    return null;
  }

  // ── Selection → selector ──────────────────────────────────────────────────
  /** @returns {Selector|null} */
  function currentSelection() {
    const doc = state.doc;
    if (!doc) return null;
    const w = /** @type {any} */ (doc.defaultView);
    const s = w.getSelection && w.getSelection();
    if (!s || s.isCollapsed || !s.rangeCount) return null;
    const exact = s.toString().trim();
    if (!exact) return null;
    const r = s.getRangeAt(0);
    const prefix = textBefore(r.startContainer, r.startOffset, 40);
    const suffix = textAfter(r.endContainer, r.endOffset, 40);
    return { exact, prefix, suffix };
  }
  function textBefore(node, offset, len) {
    const t = node.nodeType === 3 ? node.data.slice(0, offset) : "";
    return t.slice(Math.max(0, t.length - len));
  }
  function textAfter(node, offset, len) {
    const t = node.nodeType === 3 ? node.data.slice(offset) : "";
    return t.slice(0, len);
  }
  function clearSelection() {
    const doc = state.doc;
    const w = doc && /** @type {any} */ (doc.defaultView);
    if (w && w.getSelection) w.getSelection().removeAllRanges();
    send("selection", { selector: null });
  }

  // ── Reaching the replay content document ──────────────────────────────────
  /** Depth-first search through shadow roots + same-origin iframes for the
   *  deepest document whose URL is a replayed page (/replay/w/…mp_/…). */
  function findReplayDoc(root, depth) {
    if (depth > 8) return null;
    let best = null;
    const iframes = root.querySelectorAll ? root.querySelectorAll("iframe") : [];
    for (const f of iframes) {
      let d = null;
      try {
        d = f.contentDocument;
      } catch (e) {
        continue; // cross-origin; not ours
      }
      if (!d) continue;
      const href = (d.location && d.location.href) || "";
      if (href.indexOf("/replay/w/") !== -1 && href.indexOf("mp_/") !== -1 && d.body) {
        best = d;
      }
      const deeper = findReplayDoc(d, depth + 1);
      if (deeper) best = deeper;
    }
    if (root.querySelectorAll) {
      for (const el of root.querySelectorAll("*")) {
        if (el.shadowRoot) {
          const deeper = findReplayDoc(el.shadowRoot, depth + 1);
          if (deeper) best = deeper;
        }
      }
    }
    return best;
  }

  let watching = false;
  function watchReplayDoc() {
    if (watching) return;
    watching = true;
    let lastHref = "";
    const check = () => {
      const doc = findReplayDoc(document, 0);
      if (!doc) return;
      const href = doc.location.href;
      if (doc !== state.doc) {
        // A new document needs its own listener and style. A same-document
        // navigation (pushState, a hash) keeps both, so they are not added
        // again, or each selection would be reported once per navigation.
        state.doc = doc;
        state.styleInjected = false;
        doc.addEventListener("selectionchange", () => {
          send("selection", { selector: currentSelection() });
        });
      } else if (href === lastHref) {
        return;
      }
      lastHref = href;
      applyHighlights();
    };
    check();
    window.setInterval(check, 1000);
  }

  // ── Init ──────────────────────────────────────────────────────────────────
  function init() {
    const rp = document.querySelector("replay-web-page");
    if (rp) {
      // ReplayWeb.page dispatches rwp-url-change on the element itself, and it
      // does not bubble, so the listener is on the element.
      let lastUrl = "";
      rp.addEventListener("rwp-url-change", (e) => {
        const d = /** @type {any} */ (e).detail || {};
        if (typeof d.url !== "string" || !d.url) return;
        // The event also fires when only the title or view changes. The
        // notes the viewer sent are for this URL, so only a new one drops them.
        if (d.url !== lastUrl) {
          lastUrl = d.url;
          state.notes = [];
          applyHighlights();
        }
        send("url", { url: d.url, ts: typeof d.ts === "string" ? d.ts : "" });
      });
    }
    send("ready", {});
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
