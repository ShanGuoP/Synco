// Toast：右下角通知，所有操作都有回音。支持「进行中 → 原地更新为结果」
'use strict';
import { el } from '../core/dom.js';
import { icon } from '../core/icons.js';

const ICON = { ok: 'check', err: 'alert', run: 'refresh', info: 'info' };
const store = new Map();
let seq = 0;
let host;

function root() {
  if (!host) {
    host = el('div.toasts', { id: 'toasts', role: 'status', 'aria-live': 'polite' });
    document.body.append(host);
  }
  return host;
}

/**
 * @returns {{id:number, update(patch):void, close():void}}
 */
export function toast(msg, opt = {}) {
  const { type = 'info', sub = '', ms = type === 'err' ? 5200 : 2800, sticky = false, action } = opt;
  const id = ++seq;
  const node = el('div.toast', { class: `toast toast--${type}` },
    el('span.toast__ico', { html: icon(ICON[type] || 'info', { cls: 'icon icon--sm' }) }),
    el('span.toast__msg', {},
      document.createTextNode(msg),
      sub ? el('span.toast__sub', { text: sub }) : null),
    action ? el('button.btn.btn--ghost.btn--sm', { text: action.label, onclick: () => { action.run(); n.close(); } }) : null,
  );
  const n = {
    id,
    update(patch) {
      const cur = store.get(id);
      if (!cur) return toast(msg, patch);
      Object.assign(cur.opt, patch);
      node.className = `toast toast--${cur.opt.type}`;
      node.querySelector('.toast__ico').innerHTML = icon(ICON[cur.opt.type] || 'info', { cls: 'icon icon--sm' });
      node.querySelector('.toast__msg').textContent = cur.opt.msg;
      arm();
    },
    close() {
      if (!store.has(id)) return;
      clearTimeout(timer);
      store.delete(id);
      node.classList.add('is-out');
      node.addEventListener('animationend', () => node.remove(), { once: true });
      setTimeout(() => node.remove(), 400);
    },
  };
  /* ms 必须进 opt：arm() 读的是 n.opt.ms，漏进去就等于所有通知都不自动关 */
  n.opt = { msg, type, sub, ms };
  store.set(id, n);

  let timer = 0;
  const arm = () => {
    clearTimeout(timer);
    if (n.opt.ms > 0) timer = setTimeout(n.close, n.opt.ms);
  };
  if (!sticky) arm(); else n.opt.ms = 0;
  node.addEventListener('click', () => !sticky && n.close());
  root().append(node);
  return n;
}

export const toastOk   = (msg, sub) => toast(msg, { type: 'ok', sub });
export const toastErr  = (msg, sub) => toast(msg, { type: 'err', sub });
/** 常驻一条「进行中」，用于导入 / 批量提交这类长任务，拿到结果后 .close() */
export const toastBusy = (msg, sub) => toast(msg, { type: 'run', sticky: true, sub });
