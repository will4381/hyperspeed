'use strict';

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

/* Removed services and deleted simulators shouldn't linger as filters or in the console. */
function pruneGoneSources() {
  const live = new Set([...jobs.map(j => j.name), ...sims.map(s => `sim:${s.name}`)]);
  // An empty simulator list only means "none" when simctl answered.
  const gone = new Set([...logSources].filter(n => !live.has(n) && (simctlOk || !n.startsWith('sim:'))));
  if (!gone.size) return;
  gone.forEach(n => logSources.delete(n));
  if (gone.has(filterJob)) filterJob = '';
  for (let i = allLines.length - 1; i >= 0; i--) if (gone.has(allLines[i].job)) allLines.splice(i, 1);
  consoleView.rebuild();
  tailView.rebuild();
}

let chipInView = null;

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
  if (chipInView !== filterJob) {
    chipInView = filterJob;
    [...root.children].find(b => b.dataset.job === filterJob)?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }
  document.querySelectorAll('#level-filter button').forEach(b => b.classList.toggle('on', b.dataset.level === filterLevel));
  $('follow').classList.toggle('on', follow);
}
