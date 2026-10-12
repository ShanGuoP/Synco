// 调整面板的折叠组与开关控件；不持有图像或调整参数。
import { el } from '../../../core/dom.js';
import { icon } from '../../../core/icons.js';
export function grp(title, collapsed, ...body) {
  const caret = el('span.caret', { html: icon('right', { cls: 'icon icon--sm' }) });
  const bd = el('div.grp__bd', {}, ...body);
  const hd = el('button.grp__hd', { type: 'button', 'aria-expanded': String(!collapsed) }, caret, el('span', { text: title }));
  const node = el('div.grp', { class: `grp${collapsed ? ' is-collapsed' : ''}` }, hd, bd);
  hd.addEventListener('click', () => {
    const c = node.classList.toggle('is-collapsed');
    hd.setAttribute('aria-expanded', String(!c));
  });
  return node;
}
export function switchRow(label, tip, onChange) {
  let on = false;
  const sw = el('button.toggle', { type: 'button', role: 'switch', 'aria-checked': 'false', 'aria-label': label });
  sw.addEventListener('click', () => { on = !on; sw.setAttribute('aria-checked', String(on)); onChange?.(on); });
  const node = el('div.prow', { style: { display: 'flex', alignItems: 'center', justifyContent: 'space-between', minHeight: '28px' } },
    el('span.prow__name', { text: label, 'data-tip': tip }), sw);
  return { node, set(v) { on = !!v; sw.setAttribute('aria-checked', String(on)); } };
}
