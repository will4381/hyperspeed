'use strict';

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
    renderBrowsers();
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
    browsers = s.browsers ?? []; // an older daemon may still be running after an update
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
    pruneGoneSources();
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
    else if (key === 'd') on(() => deleteSim(s));
  }
  else if (view === 'overview') {
    if (key === 'arrowup' || key === 'k') on(() => moveSel(-1));
    else if (key === 'arrowdown' || key === 'j') on(() => moveSel(1));
    else if (!j) return;
    else if (key === 's') on(() => !isUp(j) && act('start', { name: j.name }));
    else if (key === 'x') on(() => isUp(j) && act('stop', { name: j.name }));
    else if (key === 'r') on(() => act('restart', { name: j.name }));
    else if (key === 't') on(() => toggleTunnel(j));
    else if (key === 'd') on(() => removeJob(j));
    else if (key === 'enter') on(() => openConsole(j.name));
  }
});

document.querySelectorAll('.tabs button').forEach(b => {
  b.onclick = () => (b.dataset.view === 'console' ? openConsole(filterJob) : setView(b.dataset.view));
});
$('stop-all').onclick = stopAllBrowsers;
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
$('job-filter').addEventListener('wheel', e => {
  // Lets a regular mouse wheel scroll the row sideways.
  if (e.deltaX === 0) {
    e.currentTarget.scrollLeft += e.deltaY;
    e.preventDefault();
  }
}, { passive: false });
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
