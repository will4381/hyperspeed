'use strict';

const $ = id => document.getElementById(id);
const rpc = (op, args = {}) =>
  fetch('rpc', { method: 'POST', body: JSON.stringify({ op, ...args }) }).then(r => r.json());

const ICONS = ['line/arrow-up', 'line/arrow-down', 'line/arrow-left', 'line/return', 'line/plus', 'line/minus',
  'line/refresh', 'line/cloud', 'line/copy', 'line/external-link', 'line/search', 'line/terminal',
  'line/trash', 'fill/play', 'fill/stop', 'fill/cloud'];
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
const fmtAgo = ms => {
  const m = Math.floor((Date.now() - ms) / 60000);
  if (m < 1) return 'JUST NOW';
  if (m < 60) return `${m}M AGO`;
  if (m < 1440) return `${Math.floor(m / 60)}H AGO`;
  return `${Math.floor(m / 1440)}D AGO`;
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
