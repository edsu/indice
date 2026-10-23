// @ts-check
//
// indice page annotations: the notes panel beside the replay. Vanilla JS, no
// build step (see DESIGN.md, "Annotations as provenance").
//
// Display is public; the create/edit affordances appear only when the API says
// this request may annotate (can_annotate). This half runs in the viewer and
// makes every API call. The other half, agent.js, runs in the replay frame and
// is the only part that touches the replayed document: it reports the current
// URL and the reader's selection, and draws the highlights sent to it. The two
// talk through `window.indiceReplay`, the channel viewer.html sets up.
//
// Everything that arrives from the frame is a suggestion. Archived pages run
// their own JavaScript in the frame's origin and can send anything the agent
// can, so a message may move the banner or pre-fill a draft, and nothing more:
// a note is only written when someone clicks Save here, after the composer has
// shown the URL and passage it will be attached to.

(function () {
  "use strict";

  // The longest quote a note may carry. A longer selection is refused rather
  // than cut, so a saved quote is always the passage the curator saw.
  const MAX_QUOTE = 2000;

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
    /** The reader's selection, as the frame last reported it. */
    lastSelection: /** @type {Selector|null} */ (null),
  };

  // ── The frame ─────────────────────────────────────────────────────────────
  function toFrame(type, payload) {
    const ch = /** @type {any} */ (window).indiceReplay;
    if (ch) ch.send(type, payload || {});
  }

  /** A selector from the frame, kept only if it is the shape we expect. It is
   *  still only a suggestion. Prefix and suffix only disambiguate, so they are
   *  cut to 64 characters; Array.from counts code points, so the cut never
   *  splits a surrogate pair into JSON the server would reject.
   *  @returns {Selector|null} */
  function cleanSelector(sel) {
    if (!sel || typeof sel.exact !== "string" || !sel.exact.trim()) return null;
    if (sel.exact.length > MAX_QUOTE) return null;
    /** @type {Selector} */
    const out = { exact: sel.exact };
    if (typeof sel.prefix === "string") out.prefix = Array.from(sel.prefix).slice(-64).join("");
    if (typeof sel.suffix === "string") out.suffix = Array.from(sel.suffix).slice(0, 64).join("");
    return out;
  }

  /** A URL as the browser's parser writes it. Parsing percent-encodes control
   *  and bidi characters in the path and punycodes the host, so a URL that
   *  reads one way cannot be stored as another. */
  function displayUrl(u) {
    try {
      return new URL(u).href;
    } catch (e) {
      return u;
    }
  }

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
    // The banner's toggle says how many notes there are, since on a phone the
    // panel starts closed and would otherwise give no sign of them.
    const toggle = document.getElementById("anno-toggle");
    if (toggle) toggle.textContent = forUrl.length ? "Notes (" + forUrl.length + ")" : "Notes";
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
    when.textContent = fmtDate(a.modified || a.created) + (a.modified ? ", edited" : "");
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
      else await reportFailure("save", r);
    });
    body.after(form);
    form.querySelector("textarea").focus();
  }

  function openComposer(selector) {
    els.composerHost.innerHTML = "";
    // The target is fixed when the composer opens. The reader may navigate,
    // or archived script may report a new URL, while the note is being
    // written; either way the note goes where the hint said it would.
    const target = { url: state.url, ts: state.ts };
    // Say what the note will be attached to, in full. Both the URL and the
    // quote come from the frame, which archived script can speak for, so this
    // is where a person sees the target before saving it.
    const hint = document.createElement("div");
    hint.className = "anno-hint";
    const where = document.createElement("p");
    const url = document.createElement("bdi");
    url.textContent = displayUrl(target.url);
    where.append(selector ? "New note on this passage at " : "New note about ", url);
    hint.append(where);
    if (selector) {
      const quote = document.createElement("blockquote");
      quote.className = "anno-quote";
      const q = document.createElement("bdi");
      q.textContent = selector.exact;
      quote.append(q);
      hint.append(quote);
    }
    const form = composer("", "Add note", async (text) => {
      const body = {
        collection: state.collection,
        url: target.url,
        timestamp: target.ts,
        note: text,
      };
      if (selector) body.selector = selector;
      const r = await postJson("", body);
      if (r.ok) {
        els.composerHost.innerHTML = "";
        clearSelection();
        await reload();
      } else {
        await reportFailure("add", r);
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
    // Notes are stored in the clear and shown publicly (public read, gated
    // write); say so at the point of authoring so a "note" isn't mistaken for a
    // private one. See rustyweb-annotation-public-warning-yrxe.
    const pub = document.createElement("p");
    pub.className = "anno-public-note";
    pub.textContent = "Notes are public, so anyone who can read this archive can read them.";
    form.append(ta, pub, row);
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      const text = ta.value.trim();
      if (!text) return;
      onSave(text);
    });
    return form;
  }

  // Report a failed write. A 503 from indice explains itself in the body —
  // which operation holds the search index, how long it has been running, and
  // that nothing was saved — so show that rather than a bare status code.
  //
  // Only for 503, deliberately. Other statuses can carry a proxy's HTML error
  // page or an internal error string with absolute paths in it, neither of
  // which belongs in an alert().
  async function reportFailure(verb, r) {
    let detail = "";
    if (r.status === 503) {
      try {
        detail = (await r.text()).trim();
      } catch (e) {
        /* fall back to the status alone */
      }
    }
    alert(detail || "Could not " + verb + " note (" + r.status + ")");
  }

  async function remove(id) {
    if (!confirm("Delete this note?")) return;
    const r = await postJson("/" + encodeURIComponent(id) + "/delete", {
      collection: state.collection,
    });
    if (r.ok) await reload();
    else await reportFailure("delete", r);
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

  // ── Highlights, drawn by the frame ────────────────────────────────────────
  function applyHighlights() {
    const notes = state.items
      .filter((a) => a.url === state.url && a.selector)
      .map((a) => ({ id: a.id, selector: a.selector }));
    toFrame("highlight", { notes });
  }

  function focusRange(id) {
    toFrame("focus", { id });
  }

  function clearSelection() {
    state.lastSelection = null;
    if (els.selBtn) /** @type {HTMLButtonElement} */ (els.selBtn).disabled = true;
    toFrame("clear-selection");
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
      if (state.lastSelection) openComposer(state.lastSelection);
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
    if (!state.collection) {
      // No collection context to scope notes to: hide the panel + toggle so a
      // plain replay isn't left with an empty 340px strip and an inert button.
      const panel = document.getElementById("anno-panel");
      if (panel) panel.hidden = true;
      const toggle = document.getElementById("anno-toggle");
      if (toggle) toggle.hidden = true;
      return;
    }
    buildPanel();
    // Where the panel would cover the page rather than sit beside it (see the
    // max-width rule in annotations.css), start with the page in view; the
    // banner's Notes button opens it.
    if (window.matchMedia("(max-width: 720px)").matches) togglePanel(false);
    reload();
    window.addEventListener("indice-replay", (e) => {
      const d = /** @type {any} */ (e).detail;
      if (d.type === "ready") {
        applyHighlights();
      } else if (d.type === "url" && typeof d.url === "string" && d.url) {
        // ReplayWeb.page reports a change of title or view as well, with the
        // same URL; only a new URL is a new page. An event without a
        // timestamp keeps the one we have, since a note needs one.
        if (typeof d.ts === "string" && d.ts) state.ts = d.ts;
        if (d.url === state.url) return;
        state.url = d.url;
        state.lastSelection = null;
        /** @type {HTMLButtonElement} */ (els.selBtn).disabled = true;
        reload();
      } else if (d.type === "selection") {
        state.lastSelection = cleanSelector(d.selector);
        const btn = /** @type {HTMLButtonElement} */ (els.selBtn);
        btn.disabled = !state.lastSelection;
        const tooLong = !!d.selector && typeof d.selector.exact === "string" &&
          d.selector.exact.length > MAX_QUOTE;
        btn.title = tooLong
          ? "The selection is too long to quote (at most " + MAX_QUOTE + " characters)."
          : "";
      }
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
