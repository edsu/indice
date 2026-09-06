// @ts-check
//
// indice page annotations — the replay-side panel plus in-place highlighting.
// Vanilla JS, no build step (see DESIGN.md, "Annotations as provenance").
//
// Display is public; the create/edit affordances appear only when the API says
// this request may annotate (can_annotate). Highlights use the CSS Custom
// Highlight API so we never mutate the replayed page's DOM — we just register
// Ranges over its same-origin content document (reachable through nested
// iframes, as the gnqf.1 spike proved). Reaching RWP's private frame structure
// is unsupported, so the traversal is deliberately defensive and must be
// re-checked on ReplayWeb.page upgrades.

(function () {
  "use strict";

  const HL = "indice-anno";
  const HL_ACTIVE = "indice-anno-active";

  /** @typedef {{exact:string, prefix?:string, suffix?:string}} Selector */
  /** @typedef {{id:string, created:string, modified?:string, author?:string,
   *             url:string, timestamp:string, selector?:Selector,
   *             note_md:string, note_html:string, editable:boolean}} Anno */

  const state = {
    collection: "",
    url: "",
    ts: "",
    canAnnotate: false,
    /** @type {Anno[]} */ items: [],
    /** @type {Map<string, Range>} */ ranges: new Map(),
    /** @type {Document|null} */ doc: null,
    styleInjected: false,
    pendingSelector: /** @type {Selector|null} */ (null),
  };

  /** @type {Record<string, HTMLElement>} */
  const els = {};

  // ── API ────────────────────────────────────────────────────────────────
  const API = "/api/annotations";
  async function listAll() {
    const r = await fetch(API + "?collection=" + encodeURIComponent(state.collection));
    if (!r.ok) throw new Error("list " + r.status);
    return r.json();
  }
  function postJson(path, body) {
    return fetch(API + path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  }

  // ── Panel rendering ──────────────────────────────────────────────────────
  function fmtDate(iso) {
    return (iso || "").slice(0, 10);
  }

  function render() {
    const forUrl = state.items.filter((a) => a.url === state.url);
    els.count.textContent = String(forUrl.length);
    els.list.innerHTML = "";
    if (!forUrl.length) {
      const empty = document.createElement("p");
      empty.className = "anno-empty";
      empty.textContent = state.canAnnotate
        ? "No notes on this page yet. Select text in the page, or add a note about the whole page."
        : "No notes on this page.";
      els.list.appendChild(empty);
    }
    for (const a of forUrl) {
      els.list.appendChild(renderItem(a));
    }
    els.addBtn.hidden = !state.canAnnotate;
    els.selBtn.hidden = !state.canAnnotate;
    applyHighlights();
  }

  /** @param {Anno} a */
  function renderItem(a) {
    const li = document.createElement("li");
    li.className = "anno-item";
    li.dataset.id = a.id;

    const meta = document.createElement("div");
    meta.className = "anno-meta";
    const who = document.createElement("span");
    who.className = "anno-author";
    who.textContent = a.author || "anonymous";
    const when = document.createElement("span");
    when.className = "anno-date";
    when.textContent = fmtDate(a.modified || a.created) + (a.modified ? " · edited" : "");
    meta.append(who, when);

    const body = document.createElement("div");
    body.className = "anno-body";
    body.innerHTML = a.note_html; // server-rendered + sanitized Markdown

    li.append(meta, body);

    if (a.selector) {
      li.classList.add("has-region");
      li.addEventListener("click", (e) => {
        if (/** @type {HTMLElement} */ (e.target).closest("button")) return;
        focusRange(a.id);
      });
    }

    if (a.editable) {
      const actions = document.createElement("div");
      actions.className = "anno-actions";
      const edit = button("Edit", () => beginEdit(a, li, body));
      const del = button("Delete", () => remove(a.id));
      del.classList.add("danger");
      actions.append(edit, del);
      li.append(actions);
    }
    return li;
  }

  function button(label, onClick) {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = label;
    b.addEventListener("click", onClick);
    return b;
  }

  // ── Composer (create / edit) ────────────────────────────────────────────
  /** @param {Anno} a */
  function beginEdit(a, li, body) {
    if (li.querySelector(".anno-composer")) return;
    const form = composer(a.note_md, "Save", async (text) => {
      const r = await postJson("/" + encodeURIComponent(a.id), {
        collection: state.collection,
        note: text,
      });
      if (r.ok) await reload();
      else alert("Could not save note (" + r.status + ")");
    });
    body.after(form);
    form.querySelector("textarea").focus();
  }

  function openComposer(selector) {
    state.pendingSelector = selector || null;
    els.composerHost.innerHTML = "";
    const hint = document.createElement("p");
    hint.className = "anno-hint";
    hint.textContent = selector
      ? "New note on: “" + truncate(selector.exact, 80) + "”"
      : "New note about this whole page.";
    const form = composer("", "Add note", async (text) => {
      const body = {
        collection: state.collection,
        url: state.url,
        timestamp: state.ts,
        note: text,
      };
      if (selector) body.selector = selector;
      const r = await postJson("", body);
      if (r.ok) {
        els.composerHost.innerHTML = "";
        clearSelection();
        await reload();
      } else {
        alert("Could not add note (" + r.status + ")");
      }
    });
    els.composerHost.append(hint, form);
    form.querySelector("textarea").focus();
  }

  function composer(initial, saveLabel, onSave) {
    const form = document.createElement("form");
    form.className = "anno-composer";
    const ta = document.createElement("textarea");
    ta.value = initial;
    ta.rows = 4;
    ta.placeholder = "Write a note… (Markdown)";
    const row = document.createElement("div");
    row.className = "anno-composer-row";
    const save = button(saveLabel, () => {});
    save.type = "submit";
    save.className = "primary";
    const cancel = button("Cancel", () => form.remove());
    row.append(save, cancel);
    form.append(ta, row);
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      const text = ta.value.trim();
      if (!text) return;
      onSave(text);
    });
    return form;
  }

  async function remove(id) {
    if (!confirm("Delete this note?")) return;
    const r = await postJson("/" + encodeURIComponent(id) + "/delete", {
      collection: state.collection,
    });
    if (r.ok) await reload();
    else alert("Could not delete note (" + r.status + ")");
  }

  // ── Data flow ─────────────────────────────────────────────────────────────
  async function reload() {
    try {
      const data = await listAll();
      state.canAnnotate = !!data.can_annotate;
      state.items = data.annotations || [];
    } catch (e) {
      state.items = [];
    }
    render();
  }

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
    const forUrl = state.items.filter((a) => a.url === state.url && a.selector);
    const ranges = [];
    for (const a of forUrl) {
      const r = rangeForQuote(doc, /** @type {Selector} */ (a.selector));
      if (r) {
        state.ranges.set(a.id, r);
        ranges.push(r);
      }
    }
    try {
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
  }

  function truncate(s, n) {
    return s.length > n ? s.slice(0, n - 1) + "…" : s;
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

  let pollTimer = 0;
  function watchReplayDoc() {
    if (pollTimer) clearInterval(pollTimer);
    let lastHref = "";
    pollTimer = window.setInterval(() => {
      const doc = findReplayDoc(document, 0);
      if (!doc) return;
      const href = doc.location.href;
      if (doc !== state.doc || href !== lastHref) {
        lastHref = href;
        state.doc = doc;
        state.styleInjected = false;
        wireSelection(doc);
        applyHighlights();
      }
    }, 1000);
  }

  function wireSelection(doc) {
    doc.addEventListener("selectionchange", () => {
      const sel = currentSelection();
      els.selBtn.disabled = !sel;
    });
  }

  // ── Panel scaffolding ─────────────────────────────────────────────────────
  function buildPanel() {
    const panel = /** @type {HTMLElement} */ (document.getElementById("anno-panel"));
    if (!panel) return;
    els.panel = panel;
    panel.innerHTML =
      '<div class="anno-head"><strong>Notes</strong> <span class="anno-count" id="anno-count">0</span>' +
      '<button type="button" class="anno-close" id="anno-close" aria-label="Close notes">×</button></div>' +
      '<div class="anno-tools">' +
      '<button type="button" id="anno-add">Add a note about this page</button>' +
      '<button type="button" id="anno-sel" disabled>Add note on selection</button>' +
      "</div>" +
      '<div id="anno-composer-host"></div>' +
      '<ul class="anno-list" id="anno-list"></ul>';
    els.count = /** @type {HTMLElement} */ (document.getElementById("anno-count"));
    els.list = /** @type {HTMLElement} */ (document.getElementById("anno-list"));
    els.addBtn = /** @type {HTMLElement} */ (document.getElementById("anno-add"));
    els.selBtn = /** @type {HTMLElement} */ (document.getElementById("anno-sel"));
    els.composerHost = /** @type {HTMLElement} */ (document.getElementById("anno-composer-host"));
    els.addBtn.addEventListener("click", () => openComposer(null));
    els.selBtn.addEventListener("click", () => {
      const sel = currentSelection();
      if (sel) openComposer(sel);
    });
    const close = document.getElementById("anno-close");
    if (close) close.addEventListener("click", () => togglePanel(false));
    const toggle = document.getElementById("anno-toggle");
    if (toggle) toggle.addEventListener("click", () => togglePanel());
  }

  function togglePanel(force) {
    const open = typeof force === "boolean" ? force : els.panel.hidden;
    els.panel.hidden = !open;
    const toggle = document.getElementById("anno-toggle");
    if (toggle) toggle.setAttribute("aria-expanded", String(open));
  }

  // ── Init ──────────────────────────────────────────────────────────────────
  function init() {
    const p = new URLSearchParams(window.location.search);
    state.collection = p.get("collection_id") || p.get("collection") || "";
    state.url = p.get("url") || "";
    state.ts = p.get("ts") || "";
    if (!state.collection) return; // nothing to scope annotations to
    buildPanel();
    reload();
    watchReplayDoc();
    const rp = document.querySelector("replay-web-page");
    if (rp) {
      rp.addEventListener("rwp-url-change", (e) => {
        const d = /** @type {any} */ (e).detail || {};
        if (d.url) state.url = d.url;
        if (d.ts) state.ts = d.ts;
        reload();
      });
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
