'use strict';

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

/* Deleting a simulator can't be undone, so like publishing it takes a second press. */
function deleteSim(s) {
  if (s.busy === 'deleting') return;
  if (s.busy || s.state !== 'Shutdown') {
    toast(`SHUT DOWN ${s.name.toUpperCase()} BEFORE DELETING IT`, true);
    return;
  }
  if (armed === s.udid) {
    disarm();
    s.busy = 'deleting'; // show it now, so a repeat press has nothing to act on
    renderSims();
    simAct('delete', s);
    return;
  }
  arm(s.udid, `PERMANENTLY DELETES ${s.name.toUpperCase()} (${fmtBytes(s.size)}). PRESS AGAIN TO CONFIRM`);
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
  r.disk = el('span');
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
  r.del = button('line/trash', 'Delete simulator', () => deleteSim(cur()));
  acts.append(r.toggle, r.restart, r.open, r.logs, r.del);
  r.root.append(r.name, r.runtime, r.state, r.udid, r.disk, r.procs, r.mem, r.memD, r.cpu, r.cpuD, acts);
  return r;
}

function renderSims() {
  const root = $('sim-rows');
  const n = sims.length;
  setText($('sim-total'), `${n} DEVICE${n === 1 ? '' : 'S'}, ${fmtBytes(sims.reduce((t, s) => t + s.size, 0))}`);
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
    const state = s.busy === 'deleting' ? 'deleting' : simStopping(s) ? 'stopping' : simStarting(s) ? 'booting' : s.state.toLowerCase();
    setText(r.state, state.toUpperCase());
    r.state.className = `st-${state}`;
    const up = simUp(s);
    setText(r.disk, fmtBytes(s.size));
    r.del.className = `act ic-trigger ${armed === s.udid ? 'armed' : up || s.busy ? 'off' : ''}`;
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
