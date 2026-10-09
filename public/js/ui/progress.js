// 阶段进度：提交后的「提交 → 排队 → 采样 → 回传成图」可视化
// 后端只提供 running/done/error 三态，这里按已知事实推进，不伪造细粒度百分比
'use strict';
import { el, fill } from '../core/dom.js';
import { t } from '../core/i18n.js';

// key 是契约（后端与视图都按它推进），文字走字典 `progress.<key>`
export const STAGES = [
  { key: 'submit' },
  { key: 'queue' },
  { key: 'sample' },
  { key: 'stitch' },
];

const label = d => t(`progress.${d.key}`);

export function makeSteps(defs = STAGES) {
  const items = new Map();
  const node = el('div.steps', { role: 'group', 'aria-label': t('progress.aria') });

  const paint = () => fill(node, defs.flatMap((d, i) => {
    const st = items.get(d.key) || 'idle';
    const mark = st === 'done' ? '✓' : st === 'err' ? '!' : st === 'run' ? '●' : '';
    const sep = i < defs.length - 1 ? el('span.step__sep', { text: '›' }) : null;
    return [
      el('span.step', { class: `step is-${st}`, 'aria-label': `${label(d)} ${st}` },
        el('span.step__k', { text: mark }), el('span', { text: label(d) })),
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
