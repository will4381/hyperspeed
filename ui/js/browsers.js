'use strict';

/* Playwright, Puppeteer, Cypress and WebDriver processes, including ones left behind by a dead run. */

const browserCache = new Map();

function makeBrowserRow(pid) {
  const r = { root: el('div', 'tr') };
  r.kind = el('span', 'name');
  r.project = el('span');
  r.state = el('span');
  r.pid = el('span', '', String(pid));
  r.age = el('span');
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
  r.stop = button('fill/stop', 'Stop', () => act('browser_stop', { pid }));
  r.stop.classList.add('halt');
  acts.append(r.stop);
  r.root.append(r.kind, r.project, r.state, r.pid, r.age, r.procs, r.mem, r.memD, r.cpu, r.cpuD, acts);
  return r;
}

function stopAllBrowsers() {
  if (!browsers.length) return;
  if (armed === 'browsers:all') {
    disarm();
    act('browser_stop');
    return;
  }
  arm('browsers:all', `STOPS ALL ${browsers.length} TEST PROCESSES, INCLUDING ACTIVE RUNS. PRESS AGAIN TO CONFIRM`);
}

function renderBrowsers() {
  const root = $('browser-rows');
  const orphans = browsers.filter(b => b.orphan).length;
  setText($('browser-title'), orphans ? `TEST BROWSERS: ${orphans} ORPHANED` : 'TEST BROWSERS');
  const all = $('stop-all');
  all.classList.toggle('armed', armed === 'browsers:all');
  all.hidden = !browsers.length;
  if (!browsers.length) {
    if (!root.querySelector('.empty')) {
      browserCache.clear();
      root.replaceChildren(el('div', 'empty', 'NO TEST BROWSERS RUNNING.'));
    }
    return;
  }
  root.querySelector('.empty')?.remove();
  const seen = new Set(browsers.map(b => b.pid));
  for (const [pid, r] of browserCache) if (!seen.has(pid)) { r.root.remove(); browserCache.delete(pid); }
  const memMax = Math.max(...browsers.map(b => b.mem), 1);
  browsers.forEach((b, i) => {
    let r = browserCache.get(b.pid);
    if (!r) {
      r = makeBrowserRow(b.pid);
      browserCache.set(b.pid, r);
    }
    place(root, r.root, i);
    setText(r.kind, b.kind);
    setText(r.project, b.project);
    r.project.title = b.project;
    setText(r.state, b.orphan ? 'ORPHANED' : 'RUNNING');
    r.state.className = b.orphan ? 'st-stopping' : 'st-running';
    setText(r.age, fmtUp(b.age));
    setText(r.procs, String(b.procs));
    setText(r.mem, fmtBytes(b.mem));
    setDots(r.memDots, b.mem / memMax * 100);
    setText(r.cpu, fmtCpu(b.cpu));
    setDots(r.cpuDots, b.cpu);
  });
}
