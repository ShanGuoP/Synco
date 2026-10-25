// Histogram HUD is anchored to the viewport; the clipping map follows photo coordinates.
import { el } from '../../core/dom.js';
import { t } from '../../core/i18n.js';

export function createHistogram({ stage, layer }) {
  let data = null, mode = 'rgb', collapsed = false, shadows = false, highlights = false;
  const canvas = el('canvas.histogram__plot', { width: 512, height: 160, role: 'img', 'aria-label': t('histogram.plot') });
  const mask = el('canvas.histogram__clipping', { hidden: true, 'aria-hidden': 'true' }); layer.append(mask);
  const status = el('span.histogram__status', { role: 'status', text: t('histogram.loading') });
  const collapse = el('button.histogram__collapse', { type: 'button', text: '−', 'aria-label': t('histogram.collapse'), 'aria-expanded': 'true', onclick: () => {
    collapsed = !collapsed; body.hidden = collapsed; collapse.textContent = collapsed ? '+' : '−'; collapse.setAttribute('aria-expanded', String(!collapsed)); collapse.setAttribute('aria-label', t(collapsed ? 'histogram.expand' : 'histogram.collapse'));
  } });
  const choices = el('div.histogram__modes', {}, ...['luminance', 'rgb', 'red', 'green', 'blue'].map(k => el('button', { type: 'button', text: t(`histogram.${k}`), dataset: { channel: k }, 'aria-pressed': String(mode === k), onclick: () => { mode = k; paint(); } })));
  const shadowBtn = el('button.histogram__clip.histogram__clip--shadow', { type: 'button', 'aria-label': t('histogram.shadowOverlay'), 'aria-pressed': 'false', onclick: () => { shadows = !shadows; overlay(); } });
  const highBtn = el('button.histogram__clip.histogram__clip--high', { type: 'button', 'aria-label': t('histogram.highlightOverlay'), 'aria-pressed': 'false', onclick: () => { highlights = !highlights; overlay(); } });
  const body = el('div.histogram__body', {}, choices, canvas, el('div.histogram__footer', {}, shadowBtn, status, highBtn));
  const node = el('aside.histogram', { 'aria-label': t('histogram.title') }, el('div.histogram__head', {}, el('span', { text: t('histogram.title') }), el('span.histogram__basis', { text: t('histogram.basis') }), collapse), body);
  stage.append(node);
  for (const event of ['pointerdown', 'pointermove', 'pointerup', 'wheel', 'dblclick', 'keydown']) node.addEventListener(event, e => e.stopPropagation());
  const pct = n => data?.pixels ? `${(n / data.pixels * 100).toFixed(1)}%` : '0.0%';
  function paint() {
    for (const b of choices.children) b.setAttribute('aria-pressed', String(b.dataset.channel === mode));
    const c = canvas.getContext('2d'), w = canvas.width, h = canvas.height; c.clearRect(0, 0, w, h);
    c.strokeStyle = 'rgba(255,255,255,.09)'; c.lineWidth = 1;
    for (let i = 1; i < 4; i++) { c.beginPath(); c.moveTo(w * i / 4, 0); c.lineTo(w * i / 4, h); c.stroke(); c.beginPath(); c.moveTo(0, h * i / 4); c.lineTo(w, h * i / 4); c.stroke(); }
    if (!data) return;
    const keys = mode === 'rgb' ? ['red', 'green', 'blue'] : [mode];
    // A shared log scale retains sparse peaks without letting a flat background dominate the plot.
    const peak = Math.max(1, ...keys.flatMap(k => data[k])); const den = Math.log1p(peak);
    const colors = { red: [223,111,117], green: [124,187,146], blue: [116,158,220], luminance: [208,215,223] };
    for (const k of keys) {
      const bins = data[k], color = colors[k]; c.beginPath(); c.moveTo(0, h);
      for (let i = 0; i < 256; i++) c.lineTo(i / 255 * w, h - Math.log1p(bins[i]) / den * (h - 8));
      c.lineTo(w, h); c.closePath(); c.fillStyle = `rgba(${color.join(',')},.23)`; c.fill(); c.strokeStyle = `rgba(${color.join(',')},.85)`; c.lineWidth = 1.5; c.stroke();
    }
    status.textContent = node.classList.contains('is-pending') ? t('histogram.updating') : t('histogram.samples', { n: data.pixels.toLocaleString() });
    shadowBtn.textContent = `◀ ${pct(data.shadows)}`; highBtn.textContent = `${pct(data.highlights)} ▶`;
    shadowBtn.title = t('histogram.shadowTip'); highBtn.title = t('histogram.highlightTip');
    canvas.setAttribute('aria-label', t('histogram.description', { channel: t(`histogram.${mode}`), shadows: pct(data.shadows), highlights: pct(data.highlights) }));
  }
  function overlay() {
    shadowBtn.setAttribute('aria-pressed', String(shadows)); highBtn.setAttribute('aria-pressed', String(highlights));
    mask.hidden = !data || (!shadows && !highlights);
    if (mask.hidden) return;
    mask.width = data.map_width; mask.height = data.map_height;
    const c = mask.getContext('2d'), pixels = c.createImageData(mask.width, mask.height);
    data.clipping.forEach((value, i) => { const s = shadows && (value & 1), h = highlights && (value & 2); if (!s && !h) return; pixels.data.set(h ? [243,82,76,185] : [82,135,245,185], i * 4); });
    c.putImageData(pixels, 0, 0);
  }
  function reset() { data = null; mask.hidden = true; shadowBtn.disabled = true; highBtn.disabled = true; shadowBtn.textContent = '◀'; highBtn.textContent = '▶'; status.textContent = t('histogram.loading'); paint(); }
  reset();
  return {
    node,
    update(next) {
      if (!next || !['red', 'green', 'blue', 'luminance'].every(k => Array.isArray(next[k]) && next[k].length === 256) || ![next.map_width,next.map_height].every(n => Number.isInteger(n) && n > 0 && n <= 320) || next.clipping?.length !== next.map_width * next.map_height) { reset(); return; }
      data = next; shadowBtn.disabled = false; highBtn.disabled = false; paint(); overlay();
    },
    pending() { status.textContent = t('histogram.updating'); mask.hidden = true; shadowBtn.disabled = true; highBtn.disabled = true; node.classList.add('is-pending'); },
    reset() { node.classList.remove('is-pending'); reset(); },
    unavailable() { node.classList.remove('is-pending'); reset(); status.textContent = t('histogram.unavailable'); },
    confirmed(next) { node.classList.remove('is-pending'); this.update(next); },
  };
}
