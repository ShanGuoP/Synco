// 提示词短语：修图界面那一排胶囊的内容，可自由增删改
// 与参数预设共用后端一套 CRUD（presets 表按 kind 分桶），这里只管 phrase 那一桶
'use strict';
import { el, fill } from '../core/dom.js';
import { api } from '../core/api.js';
import { t as tr } from '../core/i18n.js';
import { loadPhrases } from '../state.js';
import { modal, confirm } from './modal.js';
import { toastOk, toastErr } from './toast.js';

/** 一条短语 = 胶囊上的名字 + 真正并进指令的那句话 */
function askPhrase({ title, name = '', text = '', okLabel = tr('common.save') }) {
  return new Promise(res => {
    let settled = false;
    const done = v => { if (!settled) { settled = true; res(v); } };
    const nm = el('input.input', { type: 'text', maxlength: '40', placeholder: tr('ph.namePh'), value: name });
    const tx = el('textarea.textarea', { rows: '3', placeholder: tr('ph.textPh'), spellcheck: 'false' });
    tx.value = text;
    const save = () => {
      const n = nm.value.trim();
      const t = tx.value.trim();
      if (!n) { toastErr(tr('ph.needName')); nm.focus(); return; }
      if (!t) { toastErr(tr('ph.emptyTitle'), tr('ph.emptyBody')); tx.focus(); return; }
      done({ name: n, text: t });
      m.close('save');
    };
    nm.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); tx.focus(); } });
    const box = el('div', { style: { display: 'grid', gap: '10px' } },
      el('div', { style: { display: 'grid', gap: '4px' } }, el('span.muted', { text: tr('ph.name') }), nm),
      el('div', { style: { display: 'grid', gap: '4px' } }, el('span.muted', { text: tr('ph.content') }), tx),
      el('p.muted', { text: tr('ph.intro') }),
      el('div', { style: { display: 'flex', justifyContent: 'flex-end', gap: '8px' } },
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: tr('common.cancel'), onclick: () => { done(null); m.close('cancel'); } }),
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: okLabel, onclick: save })));
    const m = modal({ title, body: box, onClose: () => done(null) });
    requestAnimationFrame(() => { nm.focus(); nm.select(); });
  });
}

/** 短语管理分区：设置页与编辑器里的「管理短语…」共用 */
export function createPhrasesPane({ onChanged } = {}) {
  const body = el('div.dlg-flow');
  let list = [];

  async function refresh() {
    try { list = await api.presets(null, 'phrase'); }
    catch (e) { list = []; toastErr(tr('ph.readFail'), e.message); }
    paint();
  }

  /** 任何一次改动都要回灌 store，编辑器右栏的胶囊才会当场跟着变 */
  async function after(mutate) {
    try {
      await mutate();
      await loadPhrases();
      list = await api.presets(null, 'phrase');
      paint();
      onChanged?.();
      return true;
    } catch (e) { toastErr(tr('ph.writeFail'), e.message); return false; }
  }

  async function add() {
    /* 撞名会被后端 400 打回来：对话框带着刚打的那两行重开，而不是让人白敲一遍 */
    let seed = {};
    for (;;) {
      const v = await askPhrase({ title: tr('ph.newOne'), name: seed.name, text: seed.text, okLabel: tr('ph.addToChips') });
      if (!v) return;
      seed = v;
      if (await after(() => api.savePreset({ kind: 'phrase', name: v.name, prompt: v.text }))) { toastOk(tr('ph.added'), v.name); return; }
    }
  }

  async function edit(p) {
    let seed = { name: p.name, text: p.prompt || '' };
    for (;;) {
      const v = await askPhrase({ title: tr('ph.editTitle', { name: seed.name || p.name }), name: seed.name, text: seed.text, okLabel: tr('common.save') });
      if (!v) return;
      seed = v;
      if (await after(() => api.updatePreset(p.id, { ...p, name: v.name, prompt: v.text }))) { toastOk(tr('ph.updated'), v.name); return; }
    }
  }

  async function del(p) {
    const ok = await confirm({
      title: tr('ph.delTitle', { name: p.name }),
      text: tr('ph.delText'),
      okLabel: tr('ph.del'), danger: true,
    });
    if (!ok) return;
    if (await after(() => api.deletePreset(p.id))) toastOk(tr('ph.deleted'), p.name);
  }

  const row = p => el('div.be-row', {},
    el('div.be-row__main', {},
      el('b.be-row__url.nowrap', { text: p.name, title: p.name }),
      el('span.be-row__meta', { text: p.prompt || '', title: p.prompt || '' })),
    el('div.be-row__acts', {},
      el('button.btn.btn--sm.btn--ghost', { type: 'button', text: tr('ph.edit'), onclick: () => edit(p) }),
      el('button.btn.btn--sm.btn--danger', { type: 'button', text: tr('ph.del'), onclick: () => del(p) })));

  const paint = () => {
    fill(body,
      el('div', { style: { display: 'grid', gap: '8px' } },
        el('p.muted', { text: tr('ph.intro') }),
        el('div', { style: { display: 'flex', gap: '8px' } },
          el('button.btn.btn--primary.btn--sm', { type: 'button', text: tr('ph.newOne'), onclick: () => add() }),
          list.length ? el('span.muted', { style: { alignSelf: 'center' }, text: tr('ph.count', { n: list.length }) }) : null)),
      list.length
        ? list.map(row)
        : el('p.muted', { text: tr('ph.noneYet') }));
  };

  refresh();
  return { node: body, refresh };
}

/** 独立弹窗（编辑器右栏那颗「管理短语…」走的也是这个） */
export function phrasesManager() {
  const pane = createPhrasesPane();
  return modal({ title: tr('ph.title'), wide: true, body: pane.node, actions: [{ label: tr('common.close'), kind: 'ghost' }] });
}
