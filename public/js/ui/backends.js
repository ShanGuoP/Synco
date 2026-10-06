// ComfyUI 后端管理：扫描本机端口、登记自定义接口、切换当前生效地址
'use strict';
import { el } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api } from '../core/api.js';
import { store } from '../state.js';
import { toastOk, toastErr, toastBusy } from './toast.js';

const gb = n => (n ? (n / 1024 ** 3).toFixed(1) : '0');

const meta = b => [
  b.version, b.device,
  b.vram_total ? `显存 ${gb(b.vram_free)}/${gb(b.vram_total)} GB` : null,
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
      rows.push({ url: c.url, label: c.label, ok: false, saved: true, current: c.url === active, error: '本次未应答' });
    }
  }
  if (!rows.some(r => r.url === active)) {
    rows.unshift({ url: active, ok: false, current: true, error: '本次未应答' });
  }
  return rows;
}

/** ComfyUI 后端分区：扫描 / 登记 / 切换。作为设置弹窗的一个 pane 复用 */
export function createBackendsPane() {
  const listBox = el('div.be__list');
  const urlIn = el('input.input', { type: 'text', placeholder: 'http://127.0.0.1:8188 或 http://192.168.1.20:8188', maxlength: '120', spellcheck: 'false' });
  const labelIn = el('input.input', { type: 'text', placeholder: '备注名（可选）', maxlength: '40', spellcheck: 'false' });
  const hint = el('span.be__hint');
  let rows = [], lastFound = [];

  const syncChip = () => { const cur = rows.find(r => r.current); if (cur) store.set({ comfy: cur.url }); };

  function paint() {
    if (!rows.length) {
      listBox.replaceChildren(el('p.muted', { text: '还没有可用的后端。点「扫描后端」找本机端口，或在下面登记自定义地址。' }));
      return;
    }
    listBox.replaceChildren(...rows.map(r => {
      const acts = [r.current
        ? el('span.badge', { text: '当前' })
        : el('button.btn.btn--sm', { type: 'button', text: '选用', onclick: () => select(r.url) })];
      if (r.saved) acts.push(el('button.btn.btn--sm.btn--ghost', { type: 'button', text: '移除', onclick: () => remove(r.url) }));
      return el('div.be-row', { class: `be-row${r.current ? ' is-on' : ''}${r.ok ? '' : ' is-off'}` },
        el('span.dot', { class: `dot ${r.current ? 'dot--mask' : r.ok ? 'dot--done' : 'dot--err'}` }),
        el('div.be-row__main', {},
          el('b.be-row__url.nowrap', { text: r.url, title: r.url }),
          el('span.be-row__meta', { text: r.ok ? meta(r) : (r.error || '连不上') }),
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
    const busy = toastBusy('扫描本机端口…');
    try {
      const r = await api.scanBackends();
      busy.close();
      lastFound = r.found || [];
      hint.textContent = `扫到 ${lastFound.length} 个应答`;
      await rebuild(r.active);
    } catch (e) { busy.close(); toastErr('扫描失败', e.message); }
  }

  async function select(url) {
    try {
      const r = await api.selectBackend(url);
      await rebuild(r.active);
      hint.textContent = `已切到 ${r.active}`;
      toastOk('后端已切换', r.active);
    } catch (e) { toastErr('切换失败', e.message); }
  }

  async function add() {
    const url = urlIn.value.trim();
    if (!url) { toastErr('先填地址', '例如 http://127.0.0.1:8188'); return; }
    try {
      const r = await api.addBackend(url, labelIn.value.trim());
      urlIn.value = ''; labelIn.value = '';
      if (r.probe?.ok) {
        toastOk('已登记并探活成功', `${r.url} · ${r.probe.ms}ms`);
        lastFound = [...lastFound.filter(x => x.url !== r.url), { ...r.probe, url: r.url }];
      } else {
        toastErr('已登记，但探活失败', r.probe?.error || '无应答');
      }
      await rebuild();
    } catch (e) { toastErr('登记失败', e.message); }
  }

  async function remove(url) {
    try {
      const wasActive = rows.some(r => r.url === url && r.current);
      const r = await api.removeBackend(url);
      lastFound = lastFound.filter(x => x.url !== url);
      await rebuild(r.active);
      toastOk('已移除登记', wasActive ? `${url} 当时正生效，已回到 ${r.active}` : url);
    } catch (e) { toastErr('移除失败', e.message); }
  }

  [urlIn, labelIn].forEach(i => i.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); add(); } }));

  const node = el('div.be', {},
    el('div.be__bar', {},
      el('button.btn.btn--sm', { type: 'button', html: icon('refresh', { cls: 'icon icon--sm' }) + '<span>扫描后端</span>', onclick: scan }),
      hint),
    listBox,
    el('div.be__add', {}, urlIn, labelIn,
      el('button.btn.btn--sm.btn--primary', { type: 'button', html: icon('plus', { cls: 'icon icon--sm' }) + '<span>登记并探活</span>', onclick: add })),
    el('p.muted', { text: '切换只影响之后的提交与取图，已入库的结果不动。挂在反向代理子路径下也可以，填到路径即可，末尾斜杠可省。' }),
  );
  scan();
  return { node, refresh: scan };
}
