'use strict';

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
  r.remove = button('line/trash', 'Remove from services', () => removeJob(j()));
  acts.append(r.toggle, r.restart, r.cloud, r.logs, r.remove);
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
    const up = isUp(j);
    setText(r.up, j.uptime !== null ? fmtUp(j.uptime) : j.last_started ? fmtAgo(j.last_started) : '-');
    r.remove.className = `act ic-trigger ${armed === `rm:${j.name}` ? 'armed' : up ? 'off' : ''}`;
    setText(r.mem, j.mem ? fmtBytes(j.mem) : '-');
    setDots(r.memDots, j.mem / memMax * 100);
    setText(r.cpu, j.pid ? fmtCpu(j.cpu) : '-');
    setDots(r.cpuDots, j.cpu);
    setText(r.rt, j.latency !== null ? `${Math.round(j.latency)}MS` : j.ports.length ? 'OPEN' : '-');
    setText(r.err, String(j.errors));
    r.err.className = j.errors ? 'has-err' : '';
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
      row.append(el('span', 'core-id', `C${i}`), d, pct);
      cores.append(row);
      return { d, pct };
    });
  }
  host.cores.forEach((v, i) => {
    setDots(coreEls[i].d, v);
    coreEls[i].d.style.setProperty('--c', heat(v / 100));
    setText(coreEls[i].pct, `${Math.round(v)}%`);
  });
  setText($('load'), host.load.map(n => n.toFixed(2)).join(' '));
  setText($('procs'), `${jobs.reduce((n, j) => n + j.procs, 0)} MANAGED PROCS`);
  renderMem();
}

let memEls;

function renderMem() {
  const groups = [
    { title: 'TOTAL:', total: host.mem_total, rows: [
      ['USED', host.mem_used, 'var(--red)'],
      ['AVAIL', host.mem_avail, 'var(--yellow)'],
      ['FREE', host.mem_free, 'var(--blue)'],
      ['SWAP', host.swap_used, 'var(--mid)'],
      ['SERVICES', jobs.reduce((n, j) => n + j.mem, 0), 'var(--green)'],
    ] },
    { title: 'STORAGE:', total: host.disk_total, rows: [
      ['USED', host.disk_used, 'var(--red)'],
      ['FREE', host.disk_free, 'var(--green)'],
      ['SIMULATORS', sims.reduce((n, s) => n + s.size, 0), 'var(--blue)'],
    ] },
  ];
  const root = $('mem');
  if (!memEls) {
    memEls = groups.map((g, i) => {
      const head = el('div', `meter${i ? ' group' : ''}`);
      const total = el('span', 'num');
      head.append(el('span', '', g.title), total);
      root.append(head);
      return { total, rows: g.rows.map(([label, , color]) => {
        const row = el('div', 'meter');
        const d = dots(color), num = el('span', 'num');
        row.append(el('span', '', label), d, num);
        root.append(row);
        return { d, num };
      }) };
    });
  }
  groups.forEach((g, i) => {
    setText(memEls[i].total, gb(g.total));
    g.rows.forEach(([, value], k) => {
      setDots(memEls[i].rows[k].d, value / g.total * 100);
      setText(memEls[i].rows[k].num, gb(value));
    });
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
