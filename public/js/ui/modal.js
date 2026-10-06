// Modal：替代原生 confirm/alert。Esc / 点遮罩关闭，返回可关闭句柄
'use strict';
import { el, $ } from '../core/dom.js';
import { icon } from '../core/icons.js';

let openStack = [];

function mount({ title, body, actions = [], wide, onClose }) {
  const mask = el('div.mask');
  const close = reason => {
    if (!mask.isConnected) return;
    mask.remove();
    openStack = openStack.filter(m => m !== api);
    document.removeEventListener('keydown', onKey, true);
    onClose?.(reason);
    if (!openStack.length) document.body.style.removeProperty('overflow');
  };
  const api = { close, node: mask };

  const btnRefs = [];                 // 与 actions 一一对应：Enter 要看焦点落在哪个按钮上
  const dialog = el('div.modal', { class: `modal${wide ? ' modal--wide' : ''}`, role: 'dialog', 'aria-modal': 'true', 'aria-label': title },
    el('div.modal__hd', {},
      el('h2', { text: title }),
      el('button.btn.btn--ghost.btn--icon.btn--sm.modal__x', {
        type: 'button', 'aria-label': '关闭', html: icon('close', { cls: 'icon icon--sm' }), onclick: () => close('x') })),
    el('div.modal__bd', {}, body),
    actions.length ? el('div.modal__ft', {}, actions.map(a => {
      const b = el('button.btn', {
        class: `btn ${a.kind ? 'btn--' + a.kind : 'btn--ghost'}`,
        type: 'button', text: a.label,
        onclick: () => { const keep = a.run?.(api) === false; if (!keep) close('action:' + a.label); } });
      btnRefs.push({ a, b });
      return b;
    })) : null,
  );

  const onKey = e => {
    if (e.key === 'Escape') { e.stopPropagation(); close('esc'); }
    else if (e.key === 'Enter' && actions.length) {
      const primary = actions.find(a => a.kind === 'primary' || a.kind === 'accent' || a.kind === 'danger');
      /* 焦点停在别的按钮上（最容易是「取消」）时，回车就该激活那个按钮本身：
         越过它去跑主按钮，等于在删除框里按回车就删了。多行文本同理交给浏览器换行。 */
      const hit = btnRefs.find(x => x.b === e.target || x.b.contains(e.target));
      if (hit && hit.a !== primary) return;
      if (primary && !(e.target.closest && e.target.closest('textarea'))) {
        /* 顺序与按钮的 onclick 一致：先 run 再 close，否则 onClose 抢先把 Promise 落成「取消」 */
        e.preventDefault(); if (primary.run?.(api) !== false) close('enter');
      }
    }
  };

  mask.append(dialog);
  mask.addEventListener('pointerdown', e => { if (e.target === mask) close('backdrop'); });
  document.addEventListener('keydown', onKey, true);
  document.body.style.setProperty('overflow', 'hidden');
  document.body.append(mask);
  openStack.push(api);
  requestAnimationFrame(() => (dialog.querySelector('.modal__ft .btn--primary, .modal__ft .btn--accent, .modal__ft .btn--danger')
    || dialog.querySelector('button'))?.focus());
  return api;
}

export { mount as modal };

/** Promise 化的确认框 */
export function confirm({ title = '确认操作', text, html, danger, okLabel = '确定', cancelLabel = '取消' }) {
  return new Promise(resolve => {
    let ok = false;
    mount({
      title,
      body: html || el('p', { text: text || '' }),
      onClose: () => resolve(ok),
      actions: [
        { label: cancelLabel, kind: 'ghost' },
        { label: okLabel, kind: danger ? 'danger' : 'primary', run: () => { ok = true; } },
      ],
    });
  });
}

export function info(title, bodyHtml, { wide } = {}) {
  return mount({ title, wide, body: el('div', { html: bodyHtml }), actions: [{ label: '知道了', kind: 'primary' }] });
}

/**
 * 收一行必填文本：回车绑在输入框上（空值时不能把弹窗关掉）。
 * 预设 / 短语 / 项目名 / 画布名共用这一个，maxlength 与"空值不给提交"的规矩只此一份。
 * @returns {Promise<string|null>} 确认返回 trim 后的文本，取消返回 null
 */
export function askName(title, initial = '', { maxlength = 40, placeholder = '', hint = '', okLabel = '保存' } = {}) {
  return new Promise(res => {
    let settled = false;
    const done = v => { if (!settled) { settled = true; res(v); } };
    const inp = el('input.input', { type: 'text', maxlength: String(maxlength), placeholder, value: initial });
    const save = () => { const v = inp.value.trim(); if (v) { done(v); m.close('save'); } else inp.focus(); };
    inp.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); save(); } });
    const box = el('div', { style: { display: 'grid', gap: '10px' } }, inp,
      hint ? el('p.muted', { text: hint }) : null,
      el('div', { style: { display: 'flex', justifyContent: 'flex-end', gap: '8px' } },
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '取消', onclick: () => { done(null); m.close('cancel'); } }),
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: okLabel, onclick: save })));
    const m = mount({ title, body: box, onClose: () => done(null) });
    requestAnimationFrame(() => { inp.focus(); inp.select(); });
  });
}

export const closeAll = () => openStack.slice().forEach(m => m.close('all'));
