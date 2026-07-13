// Kumiho Brain — HUD + live-graph application logic.
// All numbers on screen are computed from real graph data; nothing is mocked.

'use strict';

import { BrainGL } from '/static/gl.js';

const EDGE_TYPES = ['REFERENCED', 'DERIVED_FROM', 'SUPERSEDES', 'DEPENDS_ON', 'ABOUT', 'IMPLEMENTED_IN', 'MOTIVATED_BY'];
const EDGE_HEX = ['#8594b3', '#9e7aff', '#ff5c4d', '#33c79e', '#ebc757', '#529eff', '#ff8cd9', '#6b7588'];
const typeIndex = (ty) => {
  const i = EDGE_TYPES.indexOf(ty);
  return i >= 0 ? i : 7;
};

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

// ------------------------------------------------------------------ state

const state = {
  nodes: [],        // by id (holes possible after removal)
  edges: [],        // {src, dst, type, t}
  edgeKeys: new Set(),
  spaces: [],
  degree: [],
  tenant: null,
  endpoint: '',
  core: false,
  live: false,
  linked: false,
  filter: { kind: null, source: null, space: null },
  query: '',
  feed: [],         // node ids, newest first
  todayCut: Date.now() - 24 * 3600 * 1000,
};

let gl;
try {
  gl = new BrainGL($('brain'));
} catch (e) {
  // renderer failure must never take the terminal down — HUD panels and the
  // live feed still work without the 3D view
  console.error('[brain] renderer init failed', e);
  gl = { ok: false, mode: 'unified' };
}
if (!gl.ok) {
  $('glfail').hidden = false;
  console.error('[brain] WebGL2 unavailable');
} else {
  gl.start();
  gl.onfps = (fps) => { $('fps').textContent = `${fps} FPS`; };
  gl.onclickNode = (id) => { if (id >= 0) openDetail(id); else { closeDetail(); gl.select(-1); } };
}

// ------------------------------------------------------------------ websocket

let ws = null;
let wsAttempts = 0;
let lastMsg = Date.now();

function connect() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  ws = new WebSocket(`${proto}://${location.host}/ws`);
  ws.onopen = () => {
    wsAttempts = 0;
    state.linked = true;
    lights();
  };
  ws.onmessage = (m) => {
    lastMsg = Date.now();
    let ev;
    try { ev = JSON.parse(m.data); } catch { return; }
    dispatch(ev);
  };
  ws.onclose = () => {
    state.linked = false;
    lights();
    const delay = Math.min(15000, 1000 * Math.pow(1.6, wsAttempts++));
    setTimeout(connect, delay);
  };
  ws.onerror = () => ws.close();
}
connect();

setInterval(() => {
  // heartbeat watchdog: server pulses every 20 s; a silent link is a dead link
  if (state.linked && Date.now() - lastMsg > 50000) {
    try { ws.close(); } catch { /* noop */ }
  }
}, 10000);

function dispatch(ev) {
  switch (ev.t) {
    case 'hello': break;
    case 'status':
      state.core = ev.core;
      state.live = ev.live;
      if (!ev.core) $('roSub').textContent = (ev.info || 'SYNCING…').toUpperCase();
      lights();
      break;
    case 'snapshot': applySnapshot(ev); break;
    case 'node_added': upsertNode(ev.node, true); break;
    case 'node_updated': upsertNode(ev.node, false); break;
    case 'edge_added': addEdge(ev.edge, true); break;
    case 'node_removed': removeNode(ev.id); break;
    case 'heartbeat': break;
  }
}

// ------------------------------------------------------------------ graph state

function applySnapshot(ev) {
  state.nodes = [];
  for (const n of ev.nodes) state.nodes[n.id] = n;
  state.edges = [];
  state.edgeKeys = new Set();
  for (const e of ev.edges) {
    const key = `${e.src}|${e.dst}|${e.type}`;
    if (state.edgeKeys.has(key)) continue;
    state.edgeKeys.add(key);
    state.edges.push({ ...e, t: typeIndex(e.type) });
  }
  state.spaces = ev.spaces;
  state.tenant = ev.tenant || null;
  state.endpoint = ev.endpoint || '';
  state.core = true;
  recomputeDegrees();

  if (gl.ok) {
    gl.setGraph(state.nodes.filter(Boolean), state.edges, state.spaces.length);
  }
  state.feed = state.nodes.filter(Boolean)
    .sort((a, b) => ts(b.updated_at) - ts(a.updated_at))
    .slice(0, 14)
    .map((n) => n.id);
  renderAll();
  lights();
  console.log(`[brain] snapshot: ${state.nodes.filter(Boolean).length} memories, ${state.edges.length} interlinks, ${state.spaces.length} spaces`);
}

function upsertNode(node, added) {
  const isNew = !state.nodes[node.id];
  state.nodes[node.id] = node;
  if (state.degree[node.id] == null) state.degree[node.id] = 0;
  if (gl.ok) {
    if (added || isNew) gl.addNode(node); else gl.updateNode(node);
  }
  state.feed = [node.id, ...state.feed.filter((i) => i !== node.id)].slice(0, 30);
  renderFeed(true);
  renderVitals();
  renderSpaces();
  renderSources();
  renderFilters();
  updateReadout();
  applyMatch(); // keep an active filter/search correct for the newcomer
}

function addEdge(edge, live) {
  const key = `${edge.src}|${edge.dst}|${edge.type}`;
  if (state.edgeKeys.has(key)) return;
  state.edgeKeys.add(key);
  const e = { ...edge, t: typeIndex(edge.type) };
  state.edges.push(e);
  state.degree[e.src] = (state.degree[e.src] || 0) + 1;
  state.degree[e.dst] = (state.degree[e.dst] || 0) + 1;
  if (gl.ok) {
    gl.bumpDegree(e.src);
    gl.bumpDegree(e.dst);
    gl.addEdge(e);
    if (live) gl.pulse(e.src);
  }
  renderVitals();
  renderLegend();
  renderHubs();
  updateReadout();
}

function removeNode(id) {
  if (!state.nodes[id]) return;
  delete state.nodes[id];
  state.edges = state.edges.filter((e) => {
    const keep = e.src !== id && e.dst !== id;
    if (!keep) state.edgeKeys.delete(`${e.src}|${e.dst}|${e.type}`);
    return keep;
  });
  recomputeDegrees();
  state.feed = state.feed.filter((i) => i !== id);
  if (gl.ok) gl.removeNode(id, state.edges);
  renderAll();
}

function recomputeDegrees() {
  state.degree = [];
  for (const e of state.edges) {
    state.degree[e.src] = (state.degree[e.src] || 0) + 1;
    state.degree[e.dst] = (state.degree[e.dst] || 0) + 1;
  }
}

const ts = (iso) => (iso ? Date.parse(iso) || 0 : 0);
const liveNodes = () => state.nodes.filter(Boolean);

// ------------------------------------------------------------------ HUD panels

function lights() {
  const set = (el, on, warn) => {
    el.classList.toggle('ok', !!on);
    el.classList.toggle('warn', !on && !!warn);
    el.classList.toggle('err', !on && !warn);
  };
  set($('li-core'), state.core, true);
  set($('li-link'), state.linked, false);
  set($('li-live'), state.live && state.linked, true);
}

function renderAll() {
  renderVitals();
  renderSpaces();
  renderFilters();
  renderLegend();
  renderFeed(false);
  renderSources();
  renderHubs();
  updateReadout();
  renderTenantLine();
}

function renderTenantLine() {
  const t = state.tenant;
  const tid = t ? shortSource(t.tenant_id) : '';
  $('tenantLine').textContent = t
    ? `TENANT ${tid} · ${t.node_count.toLocaleString()} / ${t.node_limit > 0 ? t.node_limit.toLocaleString() : '∞'} NODES`
    : 'KUMIHO.IO · SECOND BRAIN';
}

function renderVitals() {
  const ns = liveNodes();
  const conv = ns.filter((n) => n.kind === 'conversation').length;
  const code = ns.filter((n) => n.kind === 'code').length;
  const revs = ns.reduce((s, n) => s + (n.revs || 1), 0);
  const total = ns.length || 1;
  $('vitals').innerHTML = `
    <div class="kv"><span class="k">Total memories</span><span class="v">${ns.length.toLocaleString()}</span></div>
    <div class="kv"><span class="k"><span class="chip c">conversation</span></span><span class="v">${conv.toLocaleString()}</span></div>
    <div class="kv"><span class="k"><span class="chip k">code · decision</span></span><span class="v">${code.toLocaleString()}</span></div>
    <div class="kv"><span class="k">Interlinks</span><span class="v">${state.edges.length.toLocaleString()}</span></div>
    <div class="kv"><span class="k">Revisions</span><span class="v">${revs.toLocaleString()}</span></div>
    <div class="mini"><i style="width:${Math.round((conv / total) * 100)}%;background:var(--conv)"></i></div>
    <div class="mini" style="margin-top:4px"><i style="width:${Math.round((code / total) * 100)}%;background:var(--code)"></i></div>`;
}

function spaceCounts() {
  const counts = new Map();
  for (const n of liveNodes()) counts.set(n.space, (counts.get(n.space) || 0) + 1);
  return [...counts.entries()].sort((a, b) => b[1] - a[1]);
}

function renderSpaces() {
  const ranked = spaceCounts();
  $('spacesLbl').textContent = `Spaces · ${ranked.length}`;
  const top = ranked.slice(0, 6);
  const more = ranked.length - top.length;
  $('spaces').innerHTML = top.map(([sid, c]) => {
    const path = state.spaces[sid]?.path || '?';
    const short = path.split('/').filter(Boolean).slice(-2).join('/');
    const active = state.filter.space === sid ? ' active' : '';
    return `<div class="kv clickable${active}" data-space="${sid}" title="${esc(path)}">
      <span class="k">${esc(short)}</span><span class="v">${c}</span></div>`;
  }).join('') + (more > 0 ? `<div class="kv"><span class="k">… ${more} more</span><span class="v"></span></div>` : '');
  for (const el of $('spaces').querySelectorAll('[data-space]')) {
    const sid = +el.dataset.space;
    el.addEventListener('mouseenter', () => gl.ok && gl.setSpaceHighlight(sid));
    el.addEventListener('mouseleave', () => gl.ok && gl.setSpaceHighlight(state.filter.space ?? -1));
    el.addEventListener('click', () => {
      state.filter.space = state.filter.space === sid ? null : sid;
      gl.ok && gl.setSpaceHighlight(state.filter.space ?? -1);
      renderSpaces();
      renderFilters();
      applyMatch();
    });
  }
}

function renderFilters() {
  const ns = liveNodes();
  const conv = ns.filter((n) => n.kind === 'conversation').length;
  const code = ns.filter((n) => n.kind === 'code').length;
  const kinds = [
    ['all', `ALL ${ns.length}`],
    ['conversation', `CONV ${conv}`],
    ['code', `CODE ${code}`],
  ];
  $('kindFilters').innerHTML = kinds.map(([k, label]) => {
    const on = (k === 'all' && !state.filter.kind) || state.filter.kind === k;
    return `<span class="fchip${on ? ' on' : ''}" data-kind="${k}">${label}</span>`;
  }).join('');
  for (const el of $('kindFilters').querySelectorAll('[data-kind]')) {
    el.addEventListener('click', () => {
      const k = el.dataset.kind;
      state.filter.kind = k === 'all' ? null : k;
      renderFilters();
      applyMatch();
    });
  }
  // active narrow filters as removable chips
  const extra = [];
  if (state.filter.space != null) {
    extra.push(`<span class="fchip on" data-clear="space">◨ ${esc((state.spaces[state.filter.space]?.path || '?').split('/').filter(Boolean).pop())} ✕</span>`);
  }
  if (state.filter.source != null) {
    extra.push(`<span class="fchip on" data-clear="source">◍ ${esc(shortSource(state.filter.source))} ✕</span>`);
  }
  $('srcFilters').innerHTML = extra.join('');
  for (const el of $('srcFilters').querySelectorAll('[data-clear]')) {
    el.addEventListener('click', () => {
      state.filter[el.dataset.clear] = null;
      if (el.dataset.clear === 'space' && gl.ok) gl.setSpaceHighlight(-1);
      renderFilters();
      renderSpaces();
      renderSources();
      applyMatch();
    });
  }
}

function renderLegend() {
  const counts = new Map(); // by actual type name — the audit shows what exists
  for (const e of state.edges) counts.set(e.type, (counts.get(e.type) || 0) + 1);
  const rows = [...counts.entries()].sort((a, b) => b[1] - a[1]).slice(0, 9);
  $('legend').innerHTML = rows.length
    ? rows.map(([ty, c]) => `<div class="legend-row"><span class="sw" style="background:${EDGE_HEX[typeIndex(ty)]}"></span>
        <span class="lt" title="${esc(ty)}">${esc(ty)}</span><span class="lc">${c}</span></div>`).join('')
    : '<div class="legend-row"><span class="lt">no interlinks yet</span></div>';
}

function relTime(iso) {
  const d = Date.now() - ts(iso);
  if (d < 90 * 1000) return 'NOW';
  if (d < 3600 * 1000) return `${Math.floor(d / 60000)}M`;
  if (d < 86400 * 1000) return `${Math.floor(d / 3600000)}H`;
  return `${Math.floor(d / 86400000)}D`;
}

function renderFeed(fresh) {
  $('feed').innerHTML = state.feed.map((id, i) => {
    const n = state.nodes[id];
    if (!n) return '';
    const chip = n.kind === 'code' ? '<span class="chip k">code</span>' : '<span class="chip c">conv</span>';
    const cls = fresh && i === 0 ? ' fresh' : '';
    return `<div class="feedrow${cls}" data-node="${id}">
      <div class="ft">${esc(n.title)}</div>
      <div class="fm">${chip}<span>${esc(shortSource(n.source))}</span><span>· ${relTime(n.updated_at)}</span>${n.revs > 1 ? `<span>· R${n.revs}</span>` : ''}</div>
    </div>`;
  }).join('');
  for (const el of $('feed').querySelectorAll('[data-node]')) {
    el.addEventListener('click', () => openDetail(+el.dataset.node));
  }
}

function shortSource(src) {
  if (!src) return '—';
  if (/^[0-9a-f-]{20,}$/i.test(src)) return src.slice(0, 8).toUpperCase();
  return src.length > 16 ? src.slice(0, 15).toUpperCase() + '…' : src.toUpperCase();
}

function renderSources() {
  const counts = new Map();
  for (const n of liveNodes()) counts.set(n.source || '', (counts.get(n.source || '') || 0) + 1);
  const rows = [...counts.entries()].sort((a, b) => b[1] - a[1]).slice(0, 6);
  const max = rows.length ? rows[0][1] : 1;
  $('sources').innerHTML = rows.map(([src, c]) => `
    <div class="srcrow${state.filter.source === src ? ' active' : ''}" data-src="${esc(src)}">
      <span>${esc(shortSource(src))}</span>
      <span class="b"><i style="width:${Math.round((c / max) * 100)}%"></i></span>
      <span class="n">${c}</span>
    </div>`).join('');
  for (const el of $('sources').querySelectorAll('[data-src]')) {
    el.addEventListener('click', () => {
      const src = el.dataset.src;
      state.filter.source = state.filter.source === src ? null : src;
      renderSources();
      renderFilters();
      applyMatch();
    });
  }
}

function renderHubs() {
  const ids = liveNodes().map((n) => n.id)
    .sort((a, b) => (state.degree[b] || 0) - (state.degree[a] || 0))
    .slice(0, 6)
    .filter((id) => (state.degree[id] || 0) > 0);
  $('hubs').innerHTML = ids.length
    ? ids.map((id) => `<div class="hub" data-node="${id}">
        <span class="ht">${esc(state.nodes[id].title)}</span>
        <span class="hd">${state.degree[id]} ↔</span></div>`).join('')
    : '<div class="hub"><span class="ht" style="color:var(--faint)">no hubs yet</span></div>';
  for (const el of $('hubs').querySelectorAll('[data-node]')) {
    el.addEventListener('click', () => openDetail(+el.dataset.node));
  }
}

function updateReadout() {
  const ns = liveNodes();
  const today = ns.filter((n) => ts(n.updated_at) > Date.now() - 24 * 3600 * 1000).length;
  $('roBig').textContent = ns.length.toLocaleString();
  $('roSub').textContent = `MEMORIES · ${state.edges.length.toLocaleString()} INTERLINKS · +${today} TODAY`;
}

// ------------------------------------------------------------------ search + filters

function matchesFilter(n) {
  if (state.filter.kind && n.kind !== state.filter.kind) return false;
  if (state.filter.source != null && (n.source || '') !== state.filter.source) return false;
  if (state.filter.space != null && n.space !== state.filter.space) return false;
  return true;
}

function applyMatch() {
  if (!gl.ok) return;
  const q = state.query.trim().toLowerCase();
  const filtering = q || state.filter.kind || state.filter.source != null || state.filter.space != null;
  if (!filtering) {
    gl.setMatch(null);
    $('qc').textContent = '';
    return;
  }
  const flags = new Float32Array(state.nodes.length);
  let hits = 0;
  for (const n of liveNodes()) {
    let ok = matchesFilter(n);
    if (ok && q) {
      const hay = `${n.title}\n${state.spaces[n.space]?.path || ''}\n${n.item_kind}\n${n.source}\n${n.memory_type}`.toLowerCase();
      ok = hay.includes(q);
    }
    flags[n.id] = ok ? 1 : 0;
    if (ok) hits++;
  }
  gl.setMatch(flags);
  $('qc').textContent = `${hits} / ${liveNodes().length}`;
}

const qInput = $('q');
qInput.addEventListener('input', () => {
  state.query = qInput.value;
  applyMatch();
  renderSearchResults();
});

function renderSearchResults() {
  const q = state.query.trim().toLowerCase();
  const qr = $('qr');
  if (!q) {
    qr.classList.remove('show');
    qr.innerHTML = '';
    return;
  }
  const hits = liveNodes()
    .filter((n) => matchesFilter(n) && `${n.title}`.toLowerCase().includes(q))
    .sort((a, b) => ts(b.updated_at) - ts(a.updated_at))
    .slice(0, 8);
  qr.innerHTML = hits.map((n) => `<div class="qrow" data-node="${n.id}">
    <span class="chip ${n.kind === 'code' ? 'k' : 'c'}">${n.kind === 'code' ? 'code' : 'conv'}</span>
    <span>${esc(n.title)}</span></div>`).join('');
  qr.classList.toggle('show', hits.length > 0);
  for (const el of qr.querySelectorAll('[data-node]')) {
    el.addEventListener('click', () => {
      openDetail(+el.dataset.node);
      qr.classList.remove('show');
    });
  }
}

// ------------------------------------------------------------------ detail card

let detailId = -1;

async function openDetail(id) {
  const n = state.nodes[id];
  if (!n) return;
  detailId = id;
  if (gl.ok) gl.focus(id);
  const D = $('detail');
  const kindChip = n.kind === 'code'
    ? '<span class="chip k">CODE · DECISION</span>'
    : '<span class="chip c">CONVERSATION</span>';
  const kindNote = (n.item_kind === 'conversation' && n.kind === 'conversation') ? '' : `<span>${esc(n.item_kind)}</span>`;
  $('dk').innerHTML = `${kindChip}${kindNote}${n.memory_type ? `<span>· ${esc(n.memory_type)}</span>` : ''}<span>· ${esc(shortSource(n.source))}</span>`;
  $('dt').textContent = n.title;
  $('ds').textContent = '…';
  $('dl').innerHTML = '';
  $('dmeta').innerHTML = '';
  D.classList.add('show');

  let d = null;
  try {
    const r = await fetch(`/api/node/${id}`);
    if (r.ok) d = await r.json();
  } catch { /* offline — fall back to local fields */ }
  if (detailId !== id) return; // user moved on
  if (!d) {
    $('ds').textContent = '(detail unavailable — backend unreachable)';
    return;
  }
  $('ds').textContent = d.summary || '(no summary recorded)';
  $('dl').innerHTML = (d.links || []).map((l) => {
    const arrow = l.dir === 'out' ? '→' : '←';
    const jump = l.id != null ? ` jump" data-node="${l.id}` : '';
    return `<span class="lk${jump}" title="${esc(l.kref)}">
      <b style="color:${EDGE_HEX[typeIndex(l.type)]}">${esc(l.type)}</b> ${arrow} ${esc(l.title).slice(0, 30)}</span>`;
  }).join('') || '<span class="lk">no interlinks</span>';
  for (const el of $('dl').querySelectorAll('[data-node]')) {
    el.addEventListener('click', () => openDetail(+el.dataset.node));
  }
  const lineage = (d.revisions || []).slice(0, 6).map((r) => `r${r}`).join(' ⟶ ');
  const created = (n.created_at || '').slice(0, 10);
  $('dmeta').innerHTML = [
    `<span>${esc(d.space_path)}</span>`,
    `<span>${d.revisions?.length || n.revs} revision${(d.revisions?.length || n.revs) === 1 ? '' : 's'}${lineage && (d.revisions?.length || 0) > 1 ? ` · ${lineage}${(d.revisions.length > 6) ? ' ⟶ …' : ''}` : ''}</span>`,
    created ? `<span>since ${created}</span>` : '',
    (d.tags || []).length ? `<span>⚑ ${esc(d.tags.join(' '))}</span>` : '',
  ].filter(Boolean).join('');
}

function closeDetail() {
  $('detail').classList.remove('show');
  detailId = -1;
}
$('dx').addEventListener('click', () => { closeDetail(); gl.ok && gl.select(-1); });

// ------------------------------------------------------------------ chrome

function tick() {
  const d = new Date();
  const p = (n) => (n < 10 ? '0' : '') + n;
  $('clk').textContent = `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
tick();
setInterval(tick, 1000);
setInterval(() => renderFeed(false), 30000); // refresh relative times

$('modeToggle').addEventListener('click', () => setMode(gl.mode === 'unified' ? 'spaces' : 'unified'));
function setMode(mode) {
  if (!gl.ok) return;
  gl.setMode(mode);
  for (const el of $('modeToggle').querySelectorAll('[data-mode]')) {
    el.classList.toggle('on', el.dataset.mode === mode);
  }
}

addEventListener('keydown', (e) => {
  if (e.key === '/' && document.activeElement !== qInput) {
    e.preventDefault();
    qInput.focus();
  } else if (e.key === 'Escape') {
    qInput.value = '';
    state.query = '';
    state.filter = { kind: null, source: null, space: null };
    $('qr').classList.remove('show');
    qInput.blur();
    closeDetail();
    if (gl.ok) {
      gl.select(-1);
      gl.setSpaceHighlight(-1);
    }
    applyMatch();
    renderFilters();
    renderSpaces();
    renderSources();
  } else if ((e.key === 'v' || e.key === 'V') && document.activeElement !== qInput) {
    setMode(gl.mode === 'unified' ? 'spaces' : 'unified');
  }
});
