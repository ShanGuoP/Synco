// 阶段进度：提交后的「提交 → 排队 → 采样 → 回传成图」可视化
// 后端只提供 running/done/error 三态，这里按已知事实推进，不伪造细粒度百分比
'use strict';
import { el, fill } from '../core/dom.js';

export const STAGES = [
  { key: 'submit', label: '提交' },
  { key: 'queue',  label: '排队' },
  { key: 'sample', label: '采样' },
  { key: 'stitch', label: '回传成图' },
];

export function makeSteps(defs = STAGES) {
  const items = new Map();
  const node = el('div.steps', { role: 'group', 'aria-label': '生成阶段' });

  const paint = () => fill(node, defs.flatMap((d, i) => {
    const st = items.get(d.key) || 'idle';
    const mark = st === 'done' ? '✓' : st === 'err' ? '!' : st === 'run' ? '●' : '';
    const sep = i < defs.length - 1 ? el('span.step__sep', { text: '›' }) : null;
    return [
      el('span.step', { class: `step is-${st}`, 'aria-label': `${d.label} ${st}` },
        el('span.step__k', { text: mark }), el('span', { text: d.label })),
      sep,
    ];
  }));

  defs.forEach(d => items.set(d.key, 'idle'));
  paint();

  return {
    node,
    set(key, state) { items.set(key, state); paint(); },
    reset() { defs.forEach(d => items.set(d.key, 'idle')); paint(); },
    finish(ok) {
      defs.forEach(d => items.set(d.key, 'done'));
      if (!ok) items.set(defs[defs.length - 1].key, 'idle');
      paint();
    },
  };
}
