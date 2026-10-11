// API 节点画布：布局、缩放、平移和节点选择，不修改工作流连线。
'use strict';
import { el, fill, raf, clamp } from '../core/dom.js';
import { t } from '../core/i18n.js';

export function createWorkflowCanvas(onSelect) {
  const viewport = el('div.wf-graph__viewport', { tabindex: '0', 'aria-label': t('wfGraph.canvas') });
  const world = el('div.wf-graph__world');
  const lines = el('canvas.wf-graph__lines', { 'aria-hidden': 'true' });
  const search = el('input.input.wf-graph__search', { type: 'search', placeholder: t('wfGraph.search'), oninput: filter });
  const scaleLabel = el('span.muted');
  const countLabel = el('span.muted');
  let nodes = [], edges = [], cards = new Map(), selected = '', scale = 1, x = 20, y = 20, width = 1, height = 1, drag = null;
  const paint = raf(() => { world.style.transform = `translate(${x}px, ${y}px) scale(${scale})`; scaleLabel.textContent = `${Math.round(scale * 100)}%`; draw(); });
  const draw = raf(() => {
    const ctx = lines.getContext('2d');
    // 连线只分配视口大小的缓冲，长链布局不会创建数十万像素宽的 canvas。
    const dpr = window.devicePixelRatio || 1;
    lines.width = Math.max(1, Math.round(viewport.clientWidth * dpr)); lines.height = Math.max(1, Math.round(viewport.clientHeight * dpr));
    ctx.setTransform(dpr * scale, 0, 0, dpr * scale, dpr * x, dpr * y);
    const css = getComputedStyle(viewport);
    ctx.lineWidth = 2 / scale;
    for (const edge of edges) {
      const a = nodes.find(n => n.id === edge.source), b = nodes.find(n => n.id === edge.target);
      if (!a || !b) continue;
      ctx.strokeStyle = css.getPropertyValue(edge.source === selected || edge.target === selected ? '--spot' : '--line-strong').trim();
      const sx = a.x + 220, sy = a.y + 48, tx = b.x, ty = b.y + 48;
      const bend = Math.max(50, Math.abs(tx - sx) / 2);
      ctx.beginPath(); ctx.moveTo(sx, sy); ctx.bezierCurveTo(sx + bend, sy, tx - bend, ty, tx, ty); ctx.stroke();
      ctx.fillStyle = ctx.strokeStyle;
      ctx.beginPath(); ctx.moveTo(tx, ty); ctx.lineTo(tx - 8, ty - 5); ctx.lineTo(tx - 8, ty + 5); ctx.fill();
    }
  });
  function fit() {
    const rect = viewport.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    scale = clamp(Math.min((rect.width - 40) / width, (rect.height - 40) / height), .12, 1);
    x = (rect.width - width * scale) / 2; y = (rect.height - height * scale) / 2; paint();
  }
  function zoom(factor, cx = viewport.clientWidth / 2, cy = viewport.clientHeight / 2) {
    const next = clamp(scale * factor, .12, 2);
    x = cx - (cx - x) * next / scale; y = cy - (cy - y) * next / scale; scale = next; paint();
  }
  function filter() {
    const query = search.value.trim().toLowerCase();
    for (const n of nodes) cards.get(n.id).classList.toggle('is-dim', !!query && !`${n.id} ${n.title} ${n.classType}`.toLowerCase().includes(query));
  }
  function select(id, focus = false) {
    selected = id;
    for (const [key, card] of cards) { card.classList.toggle('is-selected', key === id); card.setAttribute('aria-pressed', String(key === id)); }
    if (focus) {
      const n = nodes.find(n => n.id === id);
      if (n) { x = viewport.clientWidth / 2 - (n.x + 110) * scale; y = viewport.clientHeight / 2 - (n.y + 48) * scale; paint(); }
    }
    draw(); onSelect(id);
  }
  function render(graph, links) {
    selected = ''; cards = new Map(); edges = links;
    // Kahn 层级布局；有环的节点放在末层，仍可离线检查。
    const ids = Object.keys(graph), degrees = new Map(ids.map(id => [id, 0])), depth = new Map(ids.map(id => [id, 0])), children = new Map();
    for (const e of edges) { degrees.set(e.target, degrees.get(e.target) + 1); if (!children.has(e.source)) children.set(e.source, []); children.get(e.source).push(e.target); }
    const queue = ids.filter(id => !degrees.get(id)), done = new Set();
    for (let i = 0; i < queue.length; i++) {
      const id = queue[i]; done.add(id);
      for (const child of children.get(id) || []) {
        depth.set(child, Math.max(depth.get(child), depth.get(id) + 1)); degrees.set(child, degrees.get(child) - 1);
        if (!degrees.get(child)) queue.push(child);
      }
    }
    const last = Math.max(0, ...depth.values()) + 1, rows = new Map();
    nodes = ids.map(id => {
      const level = done.has(id) ? depth.get(id) : last, row = rows.get(level) || 0; rows.set(level, row + 1);
      return { id, title: graph[id]._meta?.title || graph[id].class_type, classType: graph[id].class_type, x: level * 300 + 24, y: row * 132 + 24 };
    });
    width = Math.max(1, ...nodes.map(n => n.x + 244)); height = Math.max(1, ...nodes.map(n => n.y + 128));
    world.style.width = `${width}px`; world.style.height = `${height}px`;
    fill(world, ...nodes.map(n => {
      const card = el('button.wf-graph__node', { type: 'button', dataset: { id: n.id }, style: { left: `${n.x}px`, top: `${n.y}px` }, onclick: () => select(n.id) },
        el('span.wf-graph__node-id', { text: `#${n.id}` }), el('strong', { text: n.title }),
        el('span.wf-graph__node-class', { text: n.classType }), el('span.wf-graph__badge'));
      cards.set(n.id, card); return card;
    }));
    countLabel.textContent = t('wfGraph.count', { nodes: nodes.length, edges: edges.length }); filter(); draw(); requestAnimationFrame(fit);
  }
  function badges(profile) {
    for (const n of nodes) {
      const roles = [...new Set(profile.bindings.filter(b => b.node_id === n.id).map(b => t(`wfGraph.roles.${b.role}`)))];
      if (profile.output_node === n.id) roles.push(t('wfGraph.output'));
      cards.get(n.id).querySelector('.wf-graph__badge').textContent = roles.join(' · ');
      cards.get(n.id).classList.toggle('is-bound', !!roles.length);
    }
  }
  viewport.append(lines, world);
  viewport.addEventListener('wheel', e => { e.preventDefault(); const rect = viewport.getBoundingClientRect(); zoom(Math.exp(-e.deltaY * .001), e.clientX - rect.left, e.clientY - rect.top); }, { passive: false });
  viewport.addEventListener('pointerdown', e => {
    if (e.button !== 0 && e.button !== 1) return;
    const card = e.target.closest('.wf-graph__node'), n = e.button === 0 && card ? nodes.find(n => n.id === card.dataset.id) : null;
    if (n) select(n.id);
    drag = { px: e.clientX, py: e.clientY, x: n ? n.x : x, y: n ? n.y : y, n };
    viewport.setPointerCapture(e.pointerId); viewport.classList.add('is-dragging'); e.preventDefault();
  });
  viewport.addEventListener('pointermove', e => {
    if (!drag) return;
    const dx = e.clientX - drag.px, dy = e.clientY - drag.py;
    if (drag.n) {
      drag.n.x = clamp(drag.x + dx / scale, 0, width - 220); drag.n.y = clamp(drag.y + dy / scale, 0, height - 100);
      const card = cards.get(drag.n.id); card.style.left = `${drag.n.x}px`; card.style.top = `${drag.n.y}px`; draw();
    } else { x = drag.x + dx; y = drag.y + dy; paint(); }
  });
  const end = () => { drag = null; viewport.classList.remove('is-dragging'); };
  viewport.addEventListener('pointerup', end); viewport.addEventListener('pointercancel', end);
  const button = (key, run) => el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t(key), onclick: run });
  const node = el('div.wf-graph', {}, el('div.wf-graph__toolbar', {}, search, countLabel,
    button('wfGraph.fit', fit), button('wfGraph.zoomOut', () => zoom(.8)), scaleLabel, button('wfGraph.zoomIn', () => zoom(1.25))), viewport);
  return { node, render, badges, select, fit };
}
