'use strict';

/* ---- state ---- */

let view = 'overview', selected = null, armed = null, armedTimer = 0;
let sims = [], browsers = [], simSel = null, simctlOk = true;
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
  arm(j.name, `THIS MAKES ${j.name.toUpperCase()} PUBLIC. PRESS AGAIN TO CONFIRM`);
}

/* Removal loses the saved command and settings, so it takes a second press. */
function removeJob(j) {
  if (isUp(j)) {
    toast(`END ${j.name.toUpperCase()} BEFORE REMOVING IT`, true);
    return;
  }
  const key = `rm:${j.name}`;
  if (armed === key) {
    disarm();
    act('rm', { name: j.name });
    return;
  }
  arm(key, `REMOVES ${j.name.toUpperCase()} FROM SERVICES. PRESS AGAIN TO CONFIRM`);
}

function arm(id, message) {
  armed = id;
  clearTimeout(armedTimer);
  armedTimer = setTimeout(disarm, 4000);
  toast(message);
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
