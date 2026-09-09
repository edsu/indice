// Progressive enhancement for the accession desk (`/manage/add`): source tabs,
// the add-archive submit (upload / path-URL / Browsertrix / Archive-It), the
// Browsertrix and Archive-It browse wizards, and the shared SSE progress
// stream. Small and dependency-free, served from /assets/manage.js.
//
// Loaded by a plain <script src> at the end of the form (see
// `views::accession_desk`), so the elements it looks up already exist.

const f = document.getElementById('add-archive-form');
const out = document.getElementById('add-progress');

// Stream a job's SSE progress into the status area (shared by every source).
function stream(job, collectionName) {
  const es = new EventSource('/api/archives/' + job + '/events');
  const lines = [];
  const show = (m) => { lines.push(m); out.textContent = lines.join('\n'); };
  es.addEventListener('begin', (ev) => show('reading ' + JSON.parse(ev.data).label));
  es.addEventListener('phase', (ev) => show('… ' + JSON.parse(ev.data).phase));
  es.addEventListener('total', (ev) => show('records to index: ' + JSON.parse(ev.data).total));
  es.addEventListener('wacz_indexed', (ev) => {
    const d = JSON.parse(ev.data);
    show('indexed ' + d.label + ' (' + d.pages + ' pages)');
  });
  es.addEventListener('done', (ev) => {
    show('Done ✓');
    let d = {};
    try { d = JSON.parse(ev.data) || {}; } catch (e) {}
    const crawls = Array.isArray(d.crawls) ? d.crawls : [];
    const link = (href, text) => {
      const a = document.createElement('a'); a.href = href; a.textContent = text; return a;
    };
    const para = (child) => { const p = document.createElement('p'); p.appendChild(child); return p; };
    const collLink = () => link('/collection/' + encodeURIComponent(d.collection),
      'View ' + (collectionName ? '“' + collectionName + '”' : 'collection') + ' →');
    const wrap = document.createElement('div'); wrap.className = 'add-done-link';
    if (crawls.length === 1) {
      wrap.appendChild(para(link('/crawl/' + encodeURIComponent(crawls[0].id), 'View crawl →')));
    } else if (crawls.length > 1) {
      const head = document.createElement('p'); head.textContent = 'Added ' + crawls.length + ' crawls:';
      wrap.appendChild(head);
      const ul = document.createElement('ul'); ul.className = 'add-done-crawls';
      for (const c of crawls) {
        const li = document.createElement('li');
        li.appendChild(link('/crawl/' + encodeURIComponent(c.id), c.name || c.id));
        ul.appendChild(li);
      }
      wrap.appendChild(ul);
      if (d.collection) wrap.appendChild(para(collLink()));
    } else if (d.collection) {
      wrap.appendChild(para(collLink()));
    }
    if (wrap.childNodes.length) out.appendChild(wrap);
    es.close();
  });
  es.addEventListener('error', (ev) => { if (ev.data) show('Error: ' + JSON.parse(ev.data).message); es.close(); });
}

// Source tabs.
document.querySelectorAll('.src-tab').forEach(tab => tab.addEventListener('click', (e) => {
  e.preventDefault();
  document.querySelectorAll('.src-tab').forEach(t => t.setAttribute('aria-selected', t === tab));
  document.querySelectorAll('.src-panel').forEach(p => p.classList.toggle('active', p.id === 'src-' + tab.dataset.src));
  // Opening an import tab reaches out to the configured instance on its own.
  if (tab.dataset.src === 'bx' && !bxConnected) bxConnect();
  if (tab.dataset.src === 'ait' && !aitConnected) aitConnect();
}));

// ── Browsertrix browse (orgs → collections → crawls), using server creds. ──
async function bxGet(path) {
  const r = await fetch(path);
  if (!r.ok) throw new Error(await r.text());
  return r.json();
}
function fillSelect(sel, items, placeholder) {
  sel.innerHTML = '';
  if (placeholder != null) { const o = document.createElement('option'); o.value = ''; o.textContent = placeholder; sel.appendChild(o); }
  for (const it of items) { const o = document.createElement('option'); o.value = it.id; o.textContent = it.name; sel.appendChild(o); }
}
async function bxLoadCollections() {
  const org = document.getElementById('bx-org').value;
  if (!org) return;
  try {
    const colls = await bxGet('/api/browsertrix/collections?org=' + encodeURIComponent(org));
    fillSelect(document.getElementById('bx-collection'), colls, 'All crawls');
  } catch (e) { out.textContent = 'Error: ' + e.message; }
}
// Crawls load reactively (on connect / org / collection change), so a later
// request can overtake an earlier one — bxSeq drops any stale response.
let bxSeq = 0;
let bxItems = [];
async function bxLoadItems() {
  const org = document.getElementById('bx-org').value;
  if (!org) return;
  const coll = document.getElementById('bx-collection').value;
  const seq = ++bxSeq;
  out.textContent = 'Loading crawls…';
  try {
    const items = await bxGet('/api/browsertrix/items?org=' + encodeURIComponent(org)
      + '&collection=' + encodeURIComponent(coll));
    if (seq !== bxSeq) return;
    bxItems = items;
    out.textContent = '';
    bxRender();
  } catch (e) { if (seq === bxSeq) out.textContent = 'Error: ' + e.message; }
}
// Render bxItems into the list, applying the client-side QA-status filter.
function bxRender() {
  const box = document.getElementById('bx-items');
  const mode = (document.getElementById('bx-qa-filter') || {}).value || 'all';
  const hideImported = !!(document.getElementById('bx-hide-imported') || {}).checked;
  box.innerHTML = '';
  if (!bxItems.length) { box.textContent = 'No crawls found.'; return; }
  const items = bxItems.filter(it => (mode === 'reviewed' ? it.reviewed : mode === 'unreviewed' ? !it.reviewed : true)
    && !(hideImported && it.imported));
  if (!items.length) { box.textContent = 'No crawls match this filter.'; return; }
  for (const it of items) {
    const label = document.createElement('label');
    label.className = 'bx-item' + (it.imported ? ' imported' : '');
    const cb = document.createElement('input');
    cb.type = 'checkbox'; cb.dataset.id = it.id; cb.dataset.name = it.name || '';
    // Already in the library — can't be re-imported, so it's shown disabled.
    if (it.imported) cb.disabled = true;
    const nm = document.createElement('span'); nm.className = 'bx-name'; nm.textContent = it.name || it.id;
    const date = document.createElement('span'); date.className = 'bx-date'; date.textContent = it.date || '';
    const qa = document.createElement('span');
    qa.className = 'bx-qa' + (it.reviewed ? ' yes' : '');
    qa.textContent = it.reviewed ? ('QA’d' + (it.review_status ? ' ' + it.review_status + '/5' : '')) : 'not QA’d';
    const size = document.createElement('span'); size.className = 'bx-size';
    size.textContent = it.size_h || '';
    label.append(cb, nm);
    if (it.imported) { const b = document.createElement('span'); b.className = 'bx-badge'; b.textContent = 'in library'; label.append(b); }
    label.append(date, qa, size);
    box.appendChild(label);
  }
}
let bxConnected = false;
async function bxConnect() {
  out.textContent = 'Connecting to Browsertrix…';
  try {
    const orgs = await bxGet('/api/browsertrix/orgs');
    fillSelect(document.getElementById('bx-org'), orgs, null);
    document.getElementById('bx-browse').hidden = false;
    bxConnected = true;
    if (!orgs.length) { out.textContent = 'No organizations visible for these credentials.'; return; }
    out.textContent = '';
    await bxLoadCollections();
    bxLoadItems();
  } catch (e) { out.textContent = 'Error: ' + e.message; }
}
const bxRefresh = document.getElementById('bx-refresh');
if (bxRefresh) bxRefresh.addEventListener('click', bxLoadItems);
const bxQaFilter = document.getElementById('bx-qa-filter');
if (bxQaFilter) bxQaFilter.addEventListener('change', bxRender);
const bxHideImported = document.getElementById('bx-hide-imported');
if (bxHideImported) bxHideImported.addEventListener('change', bxRender);
const bxOrg = document.getElementById('bx-org');
if (bxOrg) bxOrg.addEventListener('change', async () => { await bxLoadCollections(); bxLoadItems(); });
const bxColl = document.getElementById('bx-collection');
if (bxColl) bxColl.addEventListener('change', bxLoadItems);

// ── Archive-It browse (collections → crawls), using server creds. ──
let aitConnected = false;
let aitCrawls = [];
// Crawls load reactively (on connect / collection change); aitSeq drops a stale
// response so a slower earlier request can't overwrite a newer list.
let aitSeq = 0;
async function aitConnect() {
  out.textContent = 'Connecting to Archive-It…';
  try {
    const colls = await bxGet('/api/archiveit/collections');
    const sel = document.getElementById('ait-collection');
    sel.innerHTML = '';
    for (const c of colls) {
      const o = document.createElement('option');
      o.value = c.id;
      o.textContent = c.name + (c.state && c.state !== 'ACTIVE' ? ' (' + c.state.toLowerCase() + ')' : '');
      sel.appendChild(o);
    }
    document.getElementById('ait-browse').hidden = false;
    aitConnected = true;
    if (!colls.length) { out.textContent = 'No Archive-It collections visible for these credentials.'; return; }
    out.textContent = '';
    aitLoadCrawls();
  } catch (e) { out.textContent = 'Error: ' + e.message; }
}
async function aitLoadCrawls() {
  const coll = document.getElementById('ait-collection').value;
  if (!coll) return;
  const seq = ++aitSeq;
  out.textContent = 'Loading crawls…';
  try {
    const crawls = await bxGet('/api/archiveit/crawls?collection=' + encodeURIComponent(coll));
    if (seq !== aitSeq) return;
    aitCrawls = crawls;
    out.textContent = '';
    aitRender();
  } catch (e) { if (seq === aitSeq) out.textContent = 'Error: ' + e.message; }
}
function aitRender() {
  const box = document.getElementById('ait-crawls');
  const hideImported = !!(document.getElementById('ait-hide-imported') || {}).checked;
  box.innerHTML = '';
  if (!aitCrawls.length) { box.textContent = 'No importable crawls found.'; return; }
  const crawls = aitCrawls.filter(c => !(hideImported && c.imported));
  if (!crawls.length) { box.textContent = 'No crawls match this filter.'; return; }
  for (const c of crawls) {
    const label = document.createElement('label');
    label.className = 'bx-item' + (c.imported ? ' imported' : '');
    const cb = document.createElement('input');
    cb.type = 'checkbox'; cb.dataset.id = c.id;
    // Already in the library — can't be re-imported, so it's shown disabled.
    if (c.imported) cb.disabled = true;
    const nm = document.createElement('span'); nm.className = 'bx-name'; nm.textContent = 'crawl ' + c.id;
    // A single (start) date keeps the column within its width; the crawl page
    // shows the full capture-date range.
    const date = document.createElement('span'); date.className = 'bx-date';
    date.textContent = (c.start || c.end || '').slice(0, 10);
    const size = document.createElement('span'); size.className = 'bx-size'; size.textContent = c.size_h || '';
    label.append(cb, nm);
    if (c.imported) { const b = document.createElement('span'); b.className = 'bx-badge'; b.textContent = 'in library'; label.append(b); }
    label.append(date, size);
    box.appendChild(label);
  }
}
const aitColl = document.getElementById('ait-collection');
if (aitColl) aitColl.addEventListener('change', aitLoadCrawls);
const aitRefresh = document.getElementById('ait-refresh');
if (aitRefresh) aitRefresh.addEventListener('click', aitLoadCrawls);
const aitHideImported = document.getElementById('ait-hide-imported');
if (aitHideImported) aitHideImported.addEventListener('change', aitRender);

// Submit: dispatch on the active source tab.
f.addEventListener('submit', async (e) => {
  e.preventDefault();
  const collection = f.collection.value.trim();
  const name = f.name.value.trim();
  if (!collection) { out.textContent = 'Please name the collection.'; return; }
  const active = document.querySelector('.src-tab[aria-selected="true"]');
  const src = active ? active.dataset.src : 'upload';
  let res;
  try {
    if (src === 'upload') {
      const file = f.file.files[0];
      if (!file) { out.textContent = 'Choose a .wacz file to upload.'; return; }
      out.textContent = 'Uploading…';
      const fd = new FormData();
      fd.append('collection', collection);
      if (name) fd.append('name', name);
      fd.append('file', file);
      res = await fetch('/api/archives/upload', { method: 'POST', body: fd });
    } else if (src === 'url') {
      const location = f.location.value.trim();
      if (!location) { out.textContent = 'Enter a path or an http(s):// URL.'; return; }
      out.textContent = 'Starting…';
      const body = { path: location, collection };
      if (name) body.name = name;
      res = await fetch('/api/archives', {
        method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
      });
    } else if (src === 'bx') {
      const checked = [...document.querySelectorAll('#bx-items input:checked')];
      if (!checked.length) { out.textContent = 'Select at least one crawl to import.'; return; }
      out.textContent = 'Importing…';
      const mode = (document.querySelector('input[name="bx-mode"]:checked') || {}).value;
      const body = {
        org: document.getElementById('bx-org').value,
        collection,
        download: mode !== 'stream',
        items: checked.map(cb => {
          const m = bxItems.find(x => x.id === cb.dataset.id) || {};
          return { id: cb.dataset.id, name: cb.dataset.name, review_status: m.review_status ?? null };
        }),
      };
      res = await fetch('/api/browsertrix/import', {
        method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
      });
    } else if (src === 'ait') {
      const checked = [...document.querySelectorAll('#ait-crawls input:checked')];
      if (!checked.length) { out.textContent = 'Select at least one crawl to import.'; return; }
      out.textContent = 'Importing…';
      const body = {
        collection_id: parseInt(document.getElementById('ait-collection').value, 10),
        collection,
        crawls: checked.map(cb => parseInt(cb.dataset.id, 10)),
      };
      res = await fetch('/api/archiveit/import', {
        method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
      });
    } else {
      out.textContent = 'That source isn’t available yet.';
      return;
    }
  } catch (err) { out.textContent = 'Request failed: ' + err; return; }
  if (!res.ok) { out.textContent = 'Error: ' + (await res.text()); return; }
  const { job } = await res.json();
  stream(job, collection);
});
