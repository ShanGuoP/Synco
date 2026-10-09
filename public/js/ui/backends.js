// ComfyUI 后端管理：扫描本机端口、登记自定义接口、切换当前生效地址
'use strict';
import { el } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api } from '../core/api.js';
import { t as tr } from '../core/i18n.js';
import { store } from '../state.js';
import { toastOk, toastErr, toastBusy } from './toast.js';

const gb = n => (n ? (n / 1024 ** 3).toFixed(1) : '0');

const meta = b => [
  b.version, b.device,
  b.vram_total ? tr('be.vram', { free: gb(b.vram_free), total: gb(b.vram_total) }) : null,
  b.ms != null ? `${b.ms}ms` : null,
].filter(Boolean).join(' · ');

/** 行 = 本次扫到的 ∪ 登记过但这次没应答的 ∪ 当前生效的 */
function mergeRows(found, custom, active) {
  const rows = found.map(f => ({
    ...f, saved: custom.some(c => c.url === f.url),
    label: custom.find(c => c.url === f.url)?.label,
    current: f.url === active,
  }));
  for (const c of custom) {
    if (!rows.some(r => r.url === c.url)) {
      rows.push({ url: c.url, label: c.label, ok: false, saved: true, current: c.url === active, error: tr('be.noAnswer') });
    }
  }
  if (!rows.some(r => r.url === active)) {
    rows.unshift({ url: active, ok: false, current: true, error: tr('be.noAnswer') });
  }
  return rows;
}

/** ComfyUI 后端分区：扫描 / 登记 / 切换。作为设置弹窗的一个 pane 复用 */
export function createBackendsPane() {
  const listBox = el('div.be__list');
  const urlIn = el('input.input', { type: 'text', placeholder: tr('be.addrPh'), maxlength: '120', spellcheck: 'false' });
  const labelIn = el('input.input', { type: 'text', placeholder: tr('be.labelPh'), maxlength: '40', spellcheck: 'false' });
  const hint = el('span.be__hint');
  let rows = [], lastFound = [];

  const syncChip = () => { const cur = rows.find(r => r.current); if (cur) store.set({ comfy: cur.url }); };

  function paint() {
    if (!rows.length) {
      listBox.replaceChildren(el('p.muted', { text: tr('be.emptyList') }));
      return;
    }
    listBox.replaceChildren(...rows.map(r => {
      const acts = [r.current
        ? el('span.badge', { text: tr('be.current') })
        : el('button.btn.btn--sm', { type: 'button', text: tr('be.select'), onclick: () => select(r.url) })];
      if (r.saved) acts.push(el('button.btn.btn--sm.btn--ghost', { type: 'button', text: tr('be.remove'), onclick: () => remove(r.url) }));
      return el('div.be-row', { class: `be-row${r.current ? ' is-on' : ''}${r.ok ? '' : ' is-off'}` },
        el('span.dot', { class: `dot ${r.current ? 'dot--mask' : r.ok ? 'dot--done' : 'dot--err'}` }),
        el('div.be-row__main', {},
          el('b.be-row__url.nowrap', { text: r.url, title: r.url }),
          el('span.be-row__meta', { text: r.ok ? meta(r) : (r.error || tr('be.unreachable')) }),
          r.label ? el('span.be-row__label', { text: r.label }) : null),
        el('div.be-row__acts', {}, ...acts));
    }));
  }

  async function rebuild(active) {
    const b = await api.backends();
    rows = mergeRows(lastFound, b.custom, active || b.active);
    paint(); syncChip();
  }

  async function scan() {
    const busy = toastBusy(tr('be.scanning'));
    try {
      const r = await api.scanBackends();
      busy.close();
      lastFound = r.found || [];
      hint.textContent = tr('be.found', { n: lastFound.length });
      await rebuild(r.active);
    } catch (e) { busy.close(); toastErr(tr('be.scanFail'), e.message); }
  }

  async function select(url) {
    try {
      const r = await api.selectBackend(url);
      await rebuild(r.active);
      hint.textContent = tr('be.switchedTo', { url: r.active });
      toastOk(tr('be.switchOk'), r.active);
    } catch (e) { toastErr(tr('be.switchFail'), e.message); }
  }

  async function add() {
    const url = urlIn.value.trim();
    if (!url) { toastErr(tr('be.needUrl'), tr('be.example', { url: 'http://127.0.0.1:8188' })); return; }
    try {
      const r = await api.addBackend(url, labelIn.value.trim());
      urlIn.value = ''; labelIn.value = '';
      if (r.probe?.ok) {
        toastOk(tr('be.probeOk'), `${r.url} · ${r.probe.ms}ms`);
        lastFound = [...lastFound.filter(x => x.url !== r.url), { ...r.probe, url: r.url }];
      } else {
        toastErr(tr('be.probeFail'), r.probe?.error || tr('be.noReply'));
      }
      await rebuild();
    } catch (e) { toastErr(tr('be.regFail'), e.message); }
  }

  async function remove(url) {
    try {
      const wasActive = rows.some(r => r.url === url && r.current);
      const r = await api.removeBackend(url);
      lastFound = lastFound.filter(x => x.url !== url);
      await rebuild(r.active);
      toastOk(tr('be.removedOk'), wasActive ? tr('be.removedBack', { url, active: r.active }) : url);
    } catch (e) { toastErr(tr('be.removeFail'), e.message); }
  }

  [urlIn, labelIn].forEach(i => i.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); add(); } }));

  const node = el('div.be', {},
    el('div.be__bar', {},
      el('button.btn.btn--sm', { type: 'button', html: icon('refresh', { cls: 'icon icon--sm' }) + `<span>${tr('be.scanBtn')}</span>`, onclick: scan }),
      hint),
    listBox,
    el('div.be__add', {}, urlIn, labelIn,
      el('button.btn.btn--sm.btn--primary', { type: 'button', html: icon('plus', { cls: 'icon icon--sm' }) + `<span>${tr('be.addBtn')}</span>`, onclick: add })),
    el('p.muted', { text: tr('be.note') }),
  );
  scan();
  return { node, refresh: scan };
}
