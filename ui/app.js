'use strict';

const $ = id => document.getElementById(id);
const rpc = (op, args = {}) =>
  fetch('rpc', { method: 'POST', body: JSON.stringify({ op, ...args }) }).then(r => r.json());

const ICONS = ['line/arrow-up', 'line/arrow-down', 'line/arrow-left', 'line/return', 'line/plus', 'line/minus',
  'line/refresh', 'line/cloud', 'line/copy', 'line/external-link', 'line/search', 'line/terminal',
  'fill/play', 'fill/stop', 'fill/cloud'];
const iconSvg = {};
let iconId = 0;

/* Each icon draws through a mask with a fixed id. Duplicates resolve to the first in the
   document, which paints a solid block when that one sits in a hidden view. */
function iconMarkup(name) {
  const n = ++iconId;
  return (iconSvg[name] || '').replace(/(id="|url\(#)(ic-[^")]+)/g, `$1$2-${n}`);
}

function ic(name) {
  const s = document.createElement('i');
  s.dataset.ic = name;
  s.innerHTML = iconMarkup(name);
  return s;
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function setText(e, v) {
  if (e.textContent !== v) e.textContent = v;
}

const fmtBytes = n => {
  if (!n) return '0MB';
  const mb = n / 1048576;
  return mb < 1024 ? `${mb.toFixed(0)}MB` : `${(mb / 1024).toFixed(2)}GB`;
};
const gb = n => `${((n || 0) / 1073741824).toFixed(2)} GB`;
const fmtUp = s => {
  if (s === null || s === undefined) return '-';
  const d = Math.floor(s / 86400), h = Math.floor(s / 3600) % 24, m = Math.floor(s / 60) % 60;
  if (d) return `${d}D:${h}H:${m}M`;
  if (h) return `${h}H:${m}M`;
  return `${m}M:${s % 60}S`;
};
const clockOf = ts => new Date(ts).toTimeString().slice(0, 8);
const fmtCpu = v => (v > 0 && v < .1 ? '<0.1' : v.toFixed(1));
const portOf = j => j.port ?? j.ports[0] ?? null;

function hue(name) {
  const palette = ['#7fd1ff', '#ffb26b', '#b6e36a', '#d7a6ff', '#ff9ec4', '#7fe3c8', '#f2d675', '#a6b8ff'];
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return palette[h % palette.length];
}

/* ---- meters ---- */

function makeSeg(root, count) {
  root.replaceChildren(...Array.from({ length: count }, () => el('i')));
}

function paintSeg(root, frac, colorAt) {
  const cells = root.children;
  const lit = frac > 0 ? Math.max(1, Math.round(frac * cells.length)) : 0;
  for (let i = 0; i < cells.length; i++) {
    const c = i < lit ? colorAt(i / cells.length) : '';
    if (cells[i].style.background !== c) cells[i].style.background = c;
  }
}

const heat = t => (t < .34 ? 'var(--green)' : t < .67 ? 'var(--yellow)' : 'var(--red)');

function dots(color) {
  const d = el('i', 'dots');
  d.style.setProperty('--c', color);
  return d;
}
const setDots = (d, pct) => d.style.setProperty('--v', `${Math.min(100, pct).toFixed(1)}%`);

function spark(canvas, values, color, maxHint = 0) {
  const dpr = devicePixelRatio || 1;
  const w = canvas.clientWidth, h = canvas.clientHeight;
  if (!w || !h) return;
  if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
    canvas.width = w * dpr;
    canvas.height = h * dpr;
  }
  const g = canvas.getContext('2d');
  g.setTransform(dpr, 0, 0, dpr, 0, 0);
  g.clearRect(0, 0, w, h);
  const step = 3, n = Math.floor(w / step);
  const data = (values || []).slice(-n);
  const max = Math.max(maxHint, ...data, 1e-6);
  const x0 = w - data.length * step;
  g.fillStyle = '#242424';
  for (let i = 0; i < n; i++) g.fillRect(w - (i + 1) * step, h - 1, 2, 1);
  g.fillStyle = color;
  data.forEach((v, i) => {
    const bar = Math.round((v / max) * h);
    if (bar > 0) g.fillRect(x0 + i * step, h - bar, 2, bar);
  });
}

/* ---- state ---- */

let view = 'overview', selected = null, armed = null, armedTimer = 0;
let sims = [], simSel = null, simctlOk = true;
const logSources = new Set();
let poll = 1000, jobs = [], host = {}, lastSeq = 0, follow = true;
let filterJob = '', filterLevel = 'all', filterText = '';
const POLL_STEPS = [250, 500, 1000, 2000, 5000];
const allLines = [];
const MAX_LINES = 20000;

function toast(msg, bad = false) {
  for (const t of [$('toast'), $('toast-t'), $('toast-s')]) {
    t.textContent = msg;
    t.classList.toggle('bad', bad);
  }
}

async function act(op, args) {
  const r = await rpc(op, args).catch(e => ({ ok: false, error: String(e) }));
  if (!r.ok) toast(r.error.toUpperCase(), true);
  else toast('');
  tick();
  return r;
}

function setView(v) {
  view = v;
  for (const id of ['overview', 'console', 'tunnels', 'sims']) $(id).hidden = id !== v;
  document.querySelectorAll('.tabs button').forEach(b => {
    const on = b.dataset.view === v;
    b.classList.toggle('on', on);
    b.setAttribute('aria-selected', on);
  });
  if (v === 'console') {
    consoleView.rebuild();
  } else if (v === 'overview') {
    tailView.rebuild();
  }
  render();
}

const selJob = () => jobs.find(j => j.name === selected);

function select(name) {
  if (selected === name) return;
  selected = name;
  tailView.rebuild();
  render();
}

function moveSel(delta) {
  if (!jobs.length) return;
  const i = jobs.findIndex(j => j.name === selected);
  const next = jobs[Math.max(0, Math.min(jobs.length - 1, i + delta))];
  select(next.name);
}

const isUp = j => j.status === 'running' || j.status === 'stopping';

function toggleRun(j) {
  act(isUp(j) ? 'stop' : 'start', { name: j.name });
}

/* Publishing a local server to the internet always takes a second press. */
function toggleTunnel(j) {
  const t = j.tunnel.status;
  if (t === 'live' || t === 'starting') {
    act('untunnel', { name: j.name });
    return;
  }
  if (armed === j.name) {
    disarm();
    toast(`OPENING TUNNEL FOR ${j.name.toUpperCase()}`);
    act('tunnel', { name: j.name }).then(r => r.ok && toast(`OPENING TUNNEL FOR ${j.name.toUpperCase()}`));
    return;
  }
  armed = j.name;
  clearTimeout(armedTimer);
  armedTimer = setTimeout(disarm, 4000);
  toast(`THIS MAKES ${j.name.toUpperCase()} PUBLIC. PRESS AGAIN TO CONFIRM`);
  render();
}

function disarm() {
  armed = null;
  clearTimeout(armedTimer);
  toast('');
  render();
}

function openConsole(name) {
  filterJob = name || '';
  setView('console');
  renderChips();
}

/* ---- log views ---- */

class LogView {
  constructor(root, accept, cap) {
    this.root = root;
    this.accept = accept;
    this.cap = cap;
  }
  node(l) {
    const row = el('div', `ln ${l.stream === 'sys' || l.stream === 'tunnel' ? l.stream : ''} ${l.level}`);
    const name = el('b', '', l.job);
    name.style.color = hue(l.job);
    row.append(el('time', '', clockOf(l.ts)), name, el('span', '', l.text));
    return row;
  }
  stuck() {
    const r = this.root;
    return r.scrollHeight - r.scrollTop - r.clientHeight < 24;
  }
  append(lines) {
    const hits = lines.filter(this.accept);
    if (!hits.length) return;
    const stick = this.isFollowing();
    const frag = document.createDocumentFragment();
    hits.slice(-this.cap).forEach(l => frag.append(this.node(l)));
    this.root.append(frag);
    while (this.root.childElementCount > this.cap) this.root.firstElementChild.remove();
    if (stick) this.root.scrollTop = this.root.scrollHeight;
  }
  isFollowing() {
    return this === consoleView ? follow : this.stuck();
  }
  rebuild() {
    this.root.replaceChildren();
    const hits = allLines.filter(this.accept).slice(-this.cap);
    const frag = document.createDocumentFragment();
    hits.forEach(l => frag.append(this.node(l)));
    this.root.append(frag);
    this.root.scrollTop = this.root.scrollHeight;
  }
}

const levelOk = l => filterLevel === 'all' || l.level === 'err' || (filterLevel === 'warn' && l.level === 'warn');
const consoleView = new LogView($('console-log'), l =>
  (!filterJob || l.job === filterJob) && levelOk(l) &&
  (!filterText || `${l.job} ${l.text}`.toLowerCase().includes(filterText)), 3000);
const tailView = new LogView($('tail'), l => l.job === selected, 300);

function ingest(lines) {
  allLines.push(...lines);
  lines.forEach(l => logSources.add(l.job));
  if (allLines.length > MAX_LINES) allLines.splice(0, allLines.length - MAX_LINES);
  if (view === 'console') consoleView.append(lines);
  if (view === 'overview') tailView.append(lines);
}

function renderChips() {
  const root = $('job-filter');
  const names = ['', ...new Set([...jobs.map(j => j.name), ...logSources, filterJob].filter(Boolean))];
  const have = [...root.children].map(b => b.dataset.job);
  if (have.join() !== names.join()) {
    root.replaceChildren(...names.map(n => {
      const b = el('button', '', n ? n.toUpperCase() : 'ALL JOBS');
      b.dataset.job = n;
      b.onclick = () => {
        filterJob = n;
        renderChips();
        consoleView.rebuild();
      };
      return b;
    }));
  }
  [...root.children].forEach(b => b.classList.toggle('on', b.dataset.job === filterJob));
  document.querySelectorAll('#level-filter button').forEach(b => b.classList.toggle('on', b.dataset.level === filterLevel));
  $('follow').classList.toggle('on', follow);
}

/* ---- rendering ---- */

const rowCache = new Map();

function button(icon, title, onclick) {
  const b = el('button', 'act ic-trigger');
  b.title = title;
  b.setAttribute('aria-label', title);
  b.append(ic(icon));
  b.onclick = e => {
    e.stopPropagation();
    onclick();
  };
  return b;
}

function setIcon(b, name) {
  if (b.firstChild.dataset.ic !== name) b.replaceChildren(ic(name));
}

function makeRow(name) {
  const r = { root: el('div', 'tr') };
  const j = () => jobs.find(x => x.name === name);
  r.root.onclick = () => select(name);
  r.root.ondblclick = () => openConsole(name);
  r.name = el('span', 'name', name);
  r.port = el('span');
  r.pid = el('span');
  r.state = el('span');
  r.up = el('span');
  r.mem = el('span');
  r.memD = el('span');
  r.memDots = dots('var(--blue)');
  r.memD.append(r.memDots);
  r.cpu = el('span');
  r.cpuD = el('span');
  r.cpuDots = dots('var(--orange)');
  r.cpuD.append(r.cpuDots);
  r.rt = el('span');
  r.err = el('span');
  const acts = el('span', 'acts');
  r.toggle = button('fill/play', 'Start or end', () => toggleRun(j()));
  r.restart = button('line/refresh', 'Restart', () => act('restart', { name }));
  r.cloud = button('line/cloud', 'Toggle public tunnel', () => toggleTunnel(j()));
  r.logs = button('line/terminal', 'Open console', () => openConsole(name));
  acts.append(r.toggle, r.restart, r.cloud, r.logs);
  r.root.append(r.name, r.port, r.pid, r.state, r.up, r.mem, r.memD, r.cpu, r.cpuD, r.rt, r.err, acts);
  return r;
}

function cloudClass(j) {
  if (armed === j.name) return 'act ic-trigger armed';
  return `act ic-trigger ${j.tunnel.status === 'live' ? 'public' : ''}`;
}

// Moves a row only when its position changed, so re-renders don't interrupt hover animations.
function place(root, row, i) {
  if (root.children[i] !== row) root.insertBefore(row, root.children[i] || null);
}

function renderServices() {
  const root = $('rows');
  if (!jobs.length) {
    if (!root.querySelector('.empty')) {
      rowCache.clear();
      const e = el('div', 'empty', 'NO SERVICES REGISTERED. FROM A PROJECT FOLDER, RUN:');
      e.append(el('code', '', 'hyperspeed add web --port 3000 -- npm run dev'));
      root.replaceChildren(e);
    }
    return;
  }
  root.querySelector('.empty')?.remove();
  const seen = new Set(jobs.map(j => j.name));
  for (const [n, r] of rowCache) if (!seen.has(n)) { r.root.remove(); rowCache.delete(n); }
  const memMax = Math.max(...jobs.map(j => j.mem), 1);
  jobs.forEach((j, i) => {
    let r = rowCache.get(j.name);
    if (!r) {
      r = makeRow(j.name);
      rowCache.set(j.name, r);
    }
    place(root, r.root, i);
    r.root.classList.toggle('sel', j.name === selected);
    const port = portOf(j);
    setText(r.port, port ? String(port) : '-');
    setText(r.pid, j.pid ? String(j.pid) : '-');
    setText(r.state, j.status === 'crashed' && j.exit_code !== null ? `CRASH:${j.exit_code}` : j.status.toUpperCase());
    r.state.className = `st-${j.status}`;
    setText(r.up, fmtUp(j.uptime));
    setText(r.mem, j.mem ? fmtBytes(j.mem) : '-');
    setDots(r.memDots, j.mem / memMax * 100);
    setText(r.cpu, j.pid ? fmtCpu(j.cpu) : '-');
    setDots(r.cpuDots, j.cpu);
    setText(r.rt, j.latency !== null ? `${Math.round(j.latency)}MS` : j.ports.length ? 'OPEN' : '-');
    setText(r.err, String(j.errors));
    r.err.className = j.errors ? 'has-err' : '';
    const up = isUp(j);
    setIcon(r.toggle, up ? 'fill/stop' : 'fill/play');
    r.toggle.className = `act ic-trigger ${up ? 'halt' : 'go'}`;
    r.toggle.title = up ? 'End' : 'Start';
    r.toggle.setAttribute('aria-label', r.toggle.title);
    setIcon(r.cloud, j.tunnel.status === 'live' ? 'fill/cloud' : 'line/cloud');
    r.cloud.className = cloudClass(j);
  });
}

function renderDetail() {
  const j = selJob();
  setText($('detail-title'), j ? `METRICS: ${j.name.toUpperCase()}` : 'METRICS');
  setText($('tail-title'), j ? `CONSOLE: ${j.name.toUpperCase()}` : 'CONSOLE');
  const h = j?.hist;
  spark($('d-cpu'), h?.cpu, '#f0682a', 5); // auto-scales above 5%, so idle servers still show a shape
  spark($('d-mem'), h?.mem, '#3f8ae6');
  spark($('d-rt'), h?.latency, '#e9d73c');
  spark($('d-log'), h?.rate, '#22f566');
  setText($('d-cpu-v'), j?.pid ? `${fmtCpu(j.cpu)}%` : '-');
  setText($('d-mem-v'), j?.mem ? fmtBytes(j.mem) : '-');
  setText($('d-rt-v'), !j?.health ? 'NO PROBE' : j.latency !== null ? `${Math.round(j.latency)}MS ${j.http}` : 'DOWN');
  setText($('d-log-v'), j ? `${h.rate.at(-1) ?? 0}` : '-');
  const kv = j ? [
    ['CMD', j.cmd], ['CWD', j.cwd.replace(/^\/Users\/[^/]+/, '~')],
    ['PROCS', String(j.procs)], ['DISK I/O', `${fmtBytes(j.io)}/S`],
    ['RESTARTS', String(j.restarts)], ['LINES', `${j.lines}  W:${j.warns}  E:${j.errors}`],
  ] : [];
  fillKv($('detail-kv'), kv);
}

function fillKv(root, pairs) {
  if (root.childElementCount !== pairs.length * 2) {
    root.replaceChildren(...pairs.flatMap(() => [el('dt'), el('dd')]));
  }
  pairs.forEach(([k, v], i) => {
    setText(root.children[i * 2], k);
    setText(root.children[i * 2 + 1], v);
    root.children[i * 2 + 1].title = v;
  });
}

let coreEls = [];

function renderHost() {
  if (!host.name && host.name !== '') return;
  fillKv($('sys'), [
    ['NAME:', (host.name || '').replace(/\.local$/, '').toUpperCase()],
    ['PROCESSOR:', (host.cpu_name || '').toUpperCase()],
    ['RAM:', `${Math.round(host.mem_total / 1073741824)} GB`],
    ['OS:', (host.os || '').toUpperCase()],
    ['UPTIME:', fmtUp(host.uptime)],
    ['PROCESSES:', String(host.processes)],
    ['SIMULATORS:', `${sims.filter(simUp).length} BOOTED / ${sims.length}`],
    ['TUNNEL:', host.cloudflared ? 'CLOUDFLARED READY' : 'CLOUDFLARED MISSING'],
  ]);
  paintSeg($('cpu-bar'), host.cpu / 100, heat);
  setText($('cpu-pct'), `${Math.round(host.cpu)}%`);
  spark($('cpu-spark'), host.cpu_hist, '#22f566', 100);
  const cores = $('cores');
  if (coreEls.length !== host.cores.length) {
    cores.style.gridTemplateRows = `repeat(${Math.ceil(host.cores.length / 2)}, auto)`;
    coreEls = host.cores.map((_, i) => {
      const row = el('div', 'core');
      const d = dots('var(--orange)'), pct = el('span', 'num');
      row.append(el('span', '', `C${i}`), el('span', 'sp'), d, pct);
      cores.append(row);
      return { d, pct };
    });
  }
  host.cores.forEach((v, i) => {
    setDots(coreEls[i].d, v);
    setText(coreEls[i].pct, `${Math.round(v)}%`);
  });
  setText($('load'), host.load.map(n => n.toFixed(2)).join(' '));
  setText($('procs'), `${jobs.reduce((n, j) => n + j.procs, 0)} MANAGED PROCS`);
  renderMem();
}

let memEls;

function renderMem() {
  const rows = [
    ['USED', host.mem_used, 'var(--red)'],
    ['AVAIL', host.mem_avail, 'var(--yellow)'],
    ['FREE', host.mem_free, 'var(--blue)'],
    ['SWAP', host.swap_used, 'var(--mid)'],
    ['SERVICES', jobs.reduce((n, j) => n + j.mem, 0), 'var(--green)'],
  ];
  const root = $('mem');
  if (!memEls) {
    const total = el('div', 'meter');
    total.append(el('span', '', 'TOTAL:'), el('span', 'num', gb(host.mem_total)));
    root.append(total);
    memEls = rows.map(([label, , color]) => {
      const row = el('div', 'meter');
      const d = dots(color), num = el('span', 'num');
      row.append(el('span', '', label), d, num);
      root.append(row);
      return { d, num };
    });
  }
  rows.forEach(([, value], i) => {
    setDots(memEls[i].d, value / host.mem_total * 100);
    setText(memEls[i].num, gb(value));
  });
}

function renderPublic() {
  const root = $('pub');
  const open = jobs.filter(j => j.tunnel.status === 'live' || j.tunnel.status === 'starting');
  const key = open.map(j => `${j.name}|${j.tunnel.status}|${j.tunnel.url}`).join();
  if (root.dataset.key === key && root.childElementCount) return;
  root.dataset.key = key;
  if (!open.length) {
    root.replaceChildren(el('div', 'empty-note', 'NOTHING PUBLIC'));
    return;
  }
  root.replaceChildren(...open.map(j => {
    const row = el('div', 'pub-row');
    row.append(el('b', '', j.name.toUpperCase()));
    if (j.tunnel.url) {
      const a = el('a', '', j.tunnel.url.replace('https://', ''));
      a.title = 'Open in browser';
      a.onclick = () => act('open_url', { url: j.tunnel.url });
      row.append(a, button('line/copy', 'Copy URL', () => copy(j.tunnel.url)));
    } else {
      row.append(el('span', 'starting', 'STARTING'));
    }
    return row;
  }));
}

function copy(text) {
  act('copy', { text }).then(r => r.ok && toast('COPIED'));
}

const tunnelCache = new Map();

function renderTunnels() {
  const root = $('tunnel-rows');
  setText($('cf-state'), host.cloudflared ? 'CLOUDFLARED READY' : 'CLOUDFLARED MISSING: brew install cloudflared');
  const servers = jobs.filter(j => portOf(j) || j.tunnel.status);
  if (!servers.length) {
    if (!root.querySelector('.empty')) {
      tunnelCache.clear();
      root.replaceChildren(el('div', 'empty', 'NO SERVERS LISTENING YET. START ONE, OR REGISTER IT WITH --port.'));
    }
    return;
  }
  root.querySelector('.empty')?.remove();
  const seen = new Set(servers.map(j => j.name));
  for (const [n, r] of tunnelCache) if (!seen.has(n)) { r.root.remove(); tunnelCache.delete(n); }
  servers.forEach((j, i) => {
    let r = tunnelCache.get(j.name);
    if (!r) {
      const name = j.name;
      const cur = () => jobs.find(x => x.name === name);
      r = { root: el('div', 'tr'), name: el('span', 'name', name), local: el('span'), mode: el('span'), st: el('span'), url: el('span') };
      const acts = el('span', 'acts');
      r.cloud = button('line/cloud', 'Toggle public tunnel', () => toggleTunnel(cur()));
      r.copy = button('line/copy', 'Copy URL', () => cur().tunnel.url && copy(cur().tunnel.url));
      r.open = button('line/external-link', 'Open in browser', () => cur().tunnel.url && act('open_url', { url: cur().tunnel.url }));
      acts.append(r.cloud, r.copy, r.open);
      r.root.append(r.name, r.local, r.mode, r.st, r.url, acts);
      tunnelCache.set(name, r);
    }
    place(root, r.root, i);
    setText(r.local, `localhost:${portOf(j) ?? '-'}`);
    setText(r.mode, j.tunnel.host ? 'NAMED' : 'QUICK');
    setText(r.st, (j.tunnel.status || 'off').toUpperCase());
    r.st.className = `st-${j.tunnel.status}`;
    setText(r.url, j.tunnel.url || (j.tunnel.host ? `https://${j.tunnel.host}` : '-'));
    setIcon(r.cloud, j.tunnel.status === 'live' ? 'fill/cloud' : 'line/cloud');
    r.cloud.className = cloudClass(j);
    for (const b of [r.copy, r.open]) b.classList.toggle('off', !j.tunnel.url);
  });
}

/* ---- simulators ---- */

const simUp = s => s.state === 'Booted';
const simStarting = s => s.busy === 'booting' || s.state === 'Booting';
const simStopping = s => s.busy === 'stopping' || s.state === 'Shutting Down';
const curSim = () => sims.find(s => s.udid === simSel);
const simAct = (action, s) => act('sim', { action, name: s.udid });
const simCache = new Map();

function toggleSimLogs(s) {
  simAct('logs', s).then(r => {
    if (r.ok && !s.logging) openConsole(`sim:${s.name}`);
  });
}

function moveSim(delta) {
  if (!sims.length) return;
  const i = sims.findIndex(s => s.udid === simSel);
  simSel = sims[Math.max(0, Math.min(sims.length - 1, i + delta))].udid;
  renderSims();
  simCache.get(simSel)?.root.scrollIntoView({ block: 'nearest' });
}

function makeSimRow(udid) {
  const cur = () => sims.find(s => s.udid === udid);
  const r = { root: el('div', 'tr') };
  r.root.onclick = () => { simSel = udid; renderSims(); };
  r.name = el('span', 'name');
  r.runtime = el('span');
  r.state = el('span');
  r.udid = el('span', '', udid);
  r.procs = el('span');
  r.mem = el('span');
  r.memD = el('span');
  r.memDots = dots('var(--blue)');
  r.memD.append(r.memDots);
  r.cpu = el('span');
  r.cpuD = el('span');
  r.cpuDots = dots('var(--orange)');
  r.cpuD.append(r.cpuDots);
  const acts = el('span', 'acts');
  r.toggle = button('fill/play', 'Boot', () => {
    const s = cur();
    simAct(simUp(s) || simStarting(s) ? 'shutdown' : 'boot', s);
  });
  r.restart = button('line/refresh', 'Restart', () => simAct('restart', cur()));
  r.open = button('line/external-link', 'Show in Simulator', () => simAct('open', cur()));
  r.logs = button('line/terminal', 'Stream app logs', () => toggleSimLogs(cur()));
  acts.append(r.toggle, r.restart, r.open, r.logs);
  r.root.append(r.name, r.runtime, r.state, r.udid, r.procs, r.mem, r.memD, r.cpu, r.cpuD, acts);
  return r;
}

function renderSims() {
  const root = $('sim-rows');
  if (!sims.length) {
    if (!root.querySelector('.empty')) {
      simCache.clear();
      root.replaceChildren(el('div', 'empty', simctlOk === null ? 'LOADING SIMULATORS' : simctlOk ? 'NO SIMULATORS FOUND.' : 'SIMCTL UNAVAILABLE. INSTALL XCODE TO MANAGE SIMULATORS.'));
    }
    return;
  }
  root.querySelector('.empty')?.remove();
  const seen = new Set(sims.map(s => s.udid));
  for (const [id, r] of simCache) if (!seen.has(id)) { r.root.remove(); simCache.delete(id); }
  if (!curSim()) simSel = sims[0].udid;
  const memMax = Math.max(...sims.map(s => s.mem), 1);
  sims.forEach((s, i) => {
    let r = simCache.get(s.udid);
    if (!r) {
      r = makeSimRow(s.udid);
      simCache.set(s.udid, r);
    }
    place(root, r.root, i);
    r.root.classList.toggle('sel', s.udid === simSel);
    setText(r.name, s.name);
    setText(r.runtime, s.runtime.toUpperCase());
    const state = simStopping(s) ? 'stopping' : simStarting(s) ? 'booting' : s.state.toLowerCase();
    setText(r.state, state.toUpperCase());
    r.state.className = `st-${state}`;
    const up = simUp(s);
    setText(r.procs, up ? String(s.procs) : '-');
    setText(r.mem, s.mem ? fmtBytes(s.mem) : '-');
    setDots(r.memDots, s.mem / memMax * 100);
    setText(r.cpu, up ? s.cpu.toFixed(1) : '-');
    setDots(r.cpuDots, s.cpu);
    const stoppable = up || simStarting(s);
    setIcon(r.toggle, stoppable ? 'fill/stop' : 'fill/play');
    r.toggle.className = `act ic-trigger ${simStopping(s) ? 'off' : stoppable ? 'halt' : 'go'}`;
    r.toggle.title = simStarting(s) ? 'Cancel boot' : up ? 'Shut down' : 'Boot';
    r.toggle.setAttribute('aria-label', r.toggle.title);
    r.logs.className = `act ic-trigger ${s.logging ? 'public' : ''}`;
  });
}

function renderTop() {
  const up = jobs.filter(j => j.status === 'running').length;
  setText($('up-label'), `UP:${up}/${jobs.length}`);
  paintSeg($('up-bar'), jobs.length ? up / jobs.length : 0, t => ['#ec2f2f', '#f0682a', '#f59a2e', '#f2c53a', '#e9e23c', '#c7ee3e', '#7cf04e', '#22f566'][Math.floor(t * 8)]);
  setText($('clock'), clockOf(Date.now()));
  setText($('interval'), `${poll}ms`);
}

function render() {
  renderTop();
  if (view === 'overview') {
    renderHost();
    renderServices();
    renderDetail();
    renderPublic();
  } else if (view === 'console') {
    renderChips();
    const shown = $('console-log').childElementCount;
    setText($('console-count'), `${shown} LINES${allLines.length > shown ? ` OF ${allLines.length}` : ''}`);
  } else if (view === 'sims') {
    renderSims();
  } else {
    renderTunnels();
  }
}

/* ---- polling ---- */

let timer = 0, inFlight = false;

async function tick() {
  if (inFlight) return;
  inFlight = true;
  try {
    const s = await rpc('state');
    if (!s.ok) throw new Error(s.error);
    jobs = s.jobs;
    host = s.host;
    sims = s.sims;
    simctlOk = s.simctl;
    if (!selJob()) select(jobs[0]?.name ?? null);
    if (s.seq < lastSeq) {
      // Daemon restarted, so its sequence started over.
      lastSeq = 0;
      allLines.length = 0;
      consoleView.rebuild();
      tailView.rebuild();
    }
    if (s.seq > lastSeq) {
      const l = await rpc('logs', { after: lastSeq });
      lastSeq = l.seq;
      ingest(l.lines);
    }
    render();
  } catch (e) {
    toast('DAEMON NOT RESPONDING. RELAUNCH HYPERSPEED.', true);
  } finally {
    inFlight = false;
    clearTimeout(timer);
    timer = setTimeout(tick, poll);
  }
}

function stepPoll(d) {
  const i = POLL_STEPS.indexOf(poll);
  poll = POLL_STEPS[Math.max(0, Math.min(POLL_STEPS.length - 1, i + d))];
  try { localStorage.setItem('poll', poll); } catch {}
  renderTop();
}

/* ---- input ---- */

document.addEventListener('keydown', e => {
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  const typing = e.target.tagName === 'INPUT';
  if (e.key === 'Escape') {
    if (armed) disarm();
    if (typing) e.target.blur();
    else if (view !== 'overview') setView('overview');
    return;
  }
  if (typing) return;
  const key = e.key.toLowerCase();
  const j = selJob();
  const on = (fn) => { e.preventDefault(); fn(); };
  if (key === '1') on(() => setView('overview'));
  else if (key === '2') on(() => openConsole(filterJob));
  else if (key === '3') on(() => setView('tunnels'));
  else if (key === '4') on(() => setView('sims'));
  else if (key === '+' || key === '=') on(() => stepPoll(-1));
  else if (key === '-') on(() => stepPoll(1));
  else if (key === '/') on(() => { if (view !== 'console') openConsole(filterJob); $('search').focus(); });
  else if (key === 'arrowleft' && view !== 'overview') on(() => setView('overview'));
  else if (view === 'sims') {
    const s = curSim();
    if (key === 'arrowup' || key === 'k') on(() => moveSim(-1));
    else if (key === 'arrowdown' || key === 'j') on(() => moveSim(1));
    else if (!s) return;
    else if (key === 's') on(() => simAct('boot', s));
    else if (key === 'x') on(() => simAct('shutdown', s));
    else if (key === 'r') on(() => simAct('restart', s));
    else if (key === 'o') on(() => simAct('open', s));
    else if (key === 'l') on(() => toggleSimLogs(s));
  }
  else if (view === 'overview') {
    if (key === 'arrowup' || key === 'k') on(() => moveSel(-1));
    else if (key === 'arrowdown' || key === 'j') on(() => moveSel(1));
    else if (!j) return;
    else if (key === 's') on(() => !isUp(j) && act('start', { name: j.name }));
    else if (key === 'x') on(() => isUp(j) && act('stop', { name: j.name }));
    else if (key === 'r') on(() => act('restart', { name: j.name }));
    else if (key === 't') on(() => toggleTunnel(j));
    else if (key === 'enter') on(() => openConsole(j.name));
  }
});

document.querySelectorAll('.tabs button').forEach(b => {
  b.onclick = () => (b.dataset.view === 'console' ? openConsole(filterJob) : setView(b.dataset.view));
});
$('slower').onclick = () => stepPoll(1);
$('faster').onclick = () => stepPoll(-1);
$('follow').onclick = () => {
  follow = !follow;
  renderChips();
  if (follow) $('console-log').scrollTop = $('console-log').scrollHeight;
};
$('clear').onclick = () => {
  allLines.length = 0;
  consoleView.rebuild();
  tailView.rebuild();
  render();
};
$('search').oninput = e => {
  filterText = e.target.value.toLowerCase();
  consoleView.rebuild();
  render();
};
document.querySelectorAll('#level-filter button').forEach(b => {
  b.onclick = () => {
    filterLevel = b.dataset.level;
    renderChips();
    consoleView.rebuild();
  };
});
$('console-log').addEventListener('wheel', e => {
  // Scrolling up pauses follow; reaching the bottom resumes it.
  if (e.deltaY < 0 && follow) { follow = false; renderChips(); }
  else if (e.deltaY > 0 && !follow && consoleView.stuck()) { follow = true; renderChips(); }
}, { passive: true });

$('top').addEventListener('mousedown', e => {
  if (e.button === 0 && !e.target.closest('button')) window.ipc?.postMessage('drag');
});
addEventListener('resize', render);

(async function init() {
  try {
    const saved = Number(localStorage.getItem('poll'));
    if (POLL_STEPS.includes(saved)) poll = saved;
  } catch {}
  await Promise.all(ICONS.map(async n => {
    iconSvg[n] = await fetch(`icons/${n}.svg`).then(r => r.text());
  }));
  document.querySelectorAll('[data-ic]').forEach(e => { e.innerHTML = iconMarkup(e.dataset.ic); });
  makeSeg($('cpu-bar'), 48);
  makeSeg($('up-bar'), 8);
  setView('overview');
  tick();
  setInterval(renderTop, 1000);
})();
