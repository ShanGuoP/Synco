// Editable RGB/channel and cyclic hue curves. Points persist as normalized coordinates.
import { el, fill, clamp } from '../../../core/dom.js';
import { t } from '../../../core/i18n.js';
const CHANNELS = ['rgb', 'red', 'green', 'blue', 'hue_hue', 'hue_sat', 'hue_lum'];
export function makeCurveEditor({ getCurves, onChange }) {
  let channel = 'rgb', selected = -1, dragging = false;
  const canvas = el('canvas.develop-curve', { width: 600, height: 400, tabindex: 0, role: 'application', 'aria-label': t('dev.curveEditor') });
  const tabs = el('div.develop-tabs', {}, ...CHANNELS.map(k => el('button.chip-s', { type: 'button', text: t(`dev.${k}`), dataset: { channel: k }, onclick: () => { channel = k; selected = -1; sync(); } })));
  const empty = () => channel.startsWith('hue_') ? [[0, 0.5], [1, 0.5]] : [[0, 0], [1, 1]];
  const points = () => getCurves()[channel].length ? getCurves()[channel] : empty();
  const read = () => { if (!getCurves()[channel].length) getCurves()[channel] = empty(); return getCurves()[channel]; };
  function emit() { if (channel.startsWith('hue_')) { const p = read(); if (selected === p.length - 1) p[0][1] = p.at(-1)[1]; else p.at(-1)[1] = p[0][1]; } onChange(); sync(); }
  function sync() {
    for (const b of tabs.children) b.classList.toggle('is-on', b.dataset.channel === channel);
    const ctx = canvas.getContext('2d'), w = canvas.width, h = canvas.height;
    ctx.clearRect(0, 0, w, h); ctx.fillStyle = getComputedStyle(canvas).getPropertyValue('--bg-field') || '#fafafa'; ctx.fillRect(0, 0, w, h);
    ctx.strokeStyle = 'rgba(128,128,128,.18)'; ctx.lineWidth = 1;
    for (let i = 0; i <= 4; i++) { ctx.beginPath(); ctx.moveTo(i * w / 4, 0); ctx.lineTo(i * w / 4, h); ctx.moveTo(0, i * h / 4); ctx.lineTo(w, i * h / 4); ctx.stroke(); }
    ctx.setLineDash([5, 7]); ctx.beginPath(); ctx.moveTo(0, h); ctx.lineTo(w, 0); ctx.stroke(); ctx.setLineDash([]);
    const p = points(); ctx.strokeStyle = ({ red: '#d96b6b', green: '#73a987', blue: '#6798cc' })[channel] || '#7b8795'; ctx.lineWidth = 3;
    ctx.beginPath();
    for (let x = 0; x <= w; x++) { const y = curve(p, x / w); if (!x) ctx.moveTo(x, (1 - y) * h); else ctx.lineTo(x, (1 - y) * h); } ctx.stroke();
    for (const [i, [x, y]] of p.entries()) { ctx.beginPath(); ctx.arc(x * w, (1 - y) * h, i === selected ? 8 : 6, 0, Math.PI * 2); ctx.fillStyle = '#fff'; ctx.fill(); ctx.stroke(); }
    const p0 = p[selected]; status.textContent = p0 ? `${Math.round(p0[0] * 255)} → ${Math.round(p0[1] * 255)}` : t('dev.curveHint');
  }
  const status = el('p.muted.develop-hint');
  const at = e => { const r = canvas.getBoundingClientRect(); return [clamp((e.clientX - r.left) / r.width, 0, 1), clamp(1 - (e.clientY - r.top) / r.height, 0, 1)]; };
  canvas.addEventListener('pointerdown', e => {
    if (e.button !== 0) return;
    const pos = at(e), p = read(); selected = p.findIndex(v => Math.hypot(v[0] - pos[0], v[1] - pos[1]) < 0.055);
    if (selected < 0) { if (p.length >= 32) return; pos[0] = clamp(pos[0], 0.005, 0.995); if (p.some(v => Math.abs(v[0] - pos[0]) < 0.005)) return; p.push(pos); p.sort((a, b) => a[0] - b[0]); selected = p.indexOf(pos); }
    dragging = true; canvas.setPointerCapture(e.pointerId); canvas.focus(); emit(); e.preventDefault();
  });
  canvas.addEventListener('pointermove', e => { if (!dragging) return; const p = read(), pos = at(e); pos[0] = selected === 0 ? 0 : selected === p.length - 1 ? 1 : clamp(pos[0], p[selected - 1][0] + 0.005, p[selected + 1][0] - 0.005); p[selected] = pos; emit(); });
  for (const event of ['pointerup', 'pointercancel', 'lostpointercapture']) canvas.addEventListener(event, () => { dragging = false; });
  function remove() { const p = read(); if (selected > 0 && selected < p.length - 1) { p.splice(selected, 1); selected = -1; emit(); } }
  canvas.addEventListener('dblclick', remove);
  canvas.addEventListener('keydown', e => {
    if (e.key === 'Delete' || e.key === 'Backspace') { remove(); e.preventDefault(); }
    else if (e.key === 'Tab') return;
    else if (e.key === 'Home') { selected = 0; sync(); e.preventDefault(); }
    else if (e.key === 'End') { selected = points().length - 1; sync(); e.preventDefault(); }
    else if (e.key.startsWith('Arrow')) { const p = read(); if (selected < 0) selected = Math.floor(p.length / 2); const d = e.shiftKey ? 0.02 : 1 / 255; if (e.key === 'ArrowUp' || e.key === 'ArrowDown') p[selected][1] = clamp(p[selected][1] + (e.key === 'ArrowUp' ? d : -d), 0, 1); else if (selected > 0 && selected < p.length - 1) p[selected][0] = clamp(p[selected][0] + (e.key === 'ArrowRight' ? d : -d), p[selected - 1][0] + 0.005, p[selected + 1][0] - 0.005); emit(); e.preventDefault(); }
  });
  const node = el('div.develop-curves', {}, tabs, canvas, status, el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('dev.resetCurve'), onclick: () => { getCurves()[channel] = []; selected = -1; emit(); } }));
  return { node, sync };
}
// Display interpolation matches the shape-preserving Hermite contract used by Rust.
function curve(p, x) {
  if (x <= p[0][0]) return p[0][1]; if (x >= p.at(-1)[0]) return p.at(-1)[1];
  const i = p.findIndex((v, j) => j < p.length - 1 && x <= p[j + 1][0]);
  const slope = j => (p[j + 1][1] - p[j][1]) / Math.max(0.00001, p[j + 1][0] - p[j][0]);
  const tangent = j => { if (!j) return slope(0); if (j === p.length - 1) return slope(j - 1); const a = slope(j - 1), b = slope(j); return a * b <= 0 ? 0 : 2 * a * b / (a + b); };
  const h = p[i + 1][0] - p[i][0], t = (x - p[i][0]) / h;
  return clamp((2*t**3-3*t*t+1)*p[i][1]+(t**3-2*t*t+t)*h*tangent(i)+(-2*t**3+3*t*t)*p[i+1][1]+(t**3-t*t)*h*tangent(i+1),0,1);
}
