// 提示词短语：修图界面那一排胶囊的内容，可自由增删改
// 与参数预设共用后端一套 CRUD（presets 表按 kind 分桶），这里只管 phrase 那一桶
'use strict';
import { el, fill } from '../core/dom.js';
import { api } from '../core/api.js';
import { loadPhrases } from '../state.js';
import { modal, confirm } from './modal.js';
import { toastOk, toastErr } from './toast.js';

/** 一条短语 = 胶囊上的名字 + 真正并进指令的那句话 */
function askPhrase({ title, name = '', text = '', okLabel = '保存' }) {
  return new Promise(res => {
    let settled = false;
    const done = v => { if (!settled) { settled = true; res(v); } };
    const nm = el('input.input', { type: 'text', maxlength: '40', placeholder: '胶囊上的字，如 皮肤精修', value: name });
    const tx = el('textarea.textarea', { rows: '3', placeholder: '真正并进指令的那句话', spellcheck: 'false' });
    tx.value = text;
    const save = () => {
      const n = nm.value.trim();
      const t = tx.value.trim();
      if (!n) { toastErr('先起个名字'); nm.focus(); return; }
      if (!t) { toastErr('内容是空的', '短语总得并进指令点什么'); tx.focus(); return; }
      done({ name: n, text: t });
      m.close('save');
    };
    nm.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); tx.focus(); } });
    const box = el('div', { style: { display: 'grid', gap: '10px' } },
      el('div', { style: { display: 'grid', gap: '4px' } }, el('span.muted', { text: '名字' }), nm),
      el('div', { style: { display: 'grid', gap: '4px' } }, el('span.muted', { text: '内容' }), tx),
      el('p.muted', { text: '短语是叠加用的：点一下并进正向指令，再点一下移出，不会盖掉你已经写好的其余部分。' }),
      el('div', { style: { display: 'flex', justifyContent: 'flex-end', gap: '8px' } },
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '取消', onclick: () => { done(null); m.close('cancel'); } }),
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
    catch (e) { list = []; toastErr('读不到短语', e.message); }
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
    } catch (e) { toastErr('改不动', e.message); return false; }
  }

  async function add() {
    /* 撞名会被后端 400 打回来：对话框带着刚打的那两行重开，而不是让人白敲一遍 */
    let seed = {};
    for (;;) {
      const v = await askPhrase({ title: '新建短语', name: seed.name, text: seed.text, okLabel: '加进胶囊' });
      if (!v) return;
      seed = v;
      if (await after(() => api.savePreset({ kind: 'phrase', name: v.name, prompt: v.text }))) { toastOk('短语已加进胶囊', v.name); return; }
    }
  }

  async function edit(p) {
    let seed = { name: p.name, text: p.prompt || '' };
    for (;;) {
      const v = await askPhrase({ title: `改「${seed.name || p.name}」`, name: seed.name, text: seed.text, okLabel: '保存' });
      if (!v) return;
      seed = v;
      if (await after(() => api.updatePreset(p.id, { ...p, name: v.name, prompt: v.text }))) { toastOk('短语已更新', v.name); return; }
    }
  }

  async function del(p) {
    const ok = await confirm({
      title: `删除「${p.name}」`,
      text: '只是删掉这颗胶囊。已经写进指令框里的那句话不会被改动。',
      okLabel: '删除', danger: true,
    });
    if (!ok) return;
    if (await after(() => api.deletePreset(p.id))) toastOk('短语已删除', p.name);
  }

  const row = p => el('div.be-row', {},
    el('div.be-row__main', {},
      el('b.be-row__url.nowrap', { text: p.name, title: p.name }),
      el('span.be-row__meta', { text: p.prompt || '', title: p.prompt || '' })),
    el('div.be-row__acts', {},
      el('button.btn.btn--sm.btn--ghost', { type: 'button', text: '编辑', onclick: () => edit(p) }),
      el('button.btn.btn--sm.btn--danger', { type: 'button', text: '删除', onclick: () => del(p) })));

  const paint = () => {
    fill(body,
      el('div', { style: { display: 'grid', gap: '8px' } },
        el('p.muted', { text: '这些胶囊点一下就把那句话并进正向指令，再点一下移出。默认给了 7 条常用的，改成什么都行。' }),
        el('div', { style: { display: 'flex', gap: '8px' } },
          el('button.btn.btn--primary.btn--sm', { type: 'button', text: '新建短语', onclick: () => add() }),
          list.length ? el('span.muted', { style: { alignSelf: 'center' }, text: `${list.length} 条` }) : null)),
      list.length
        ? list.map(row)
        : el('p.muted', { text: '一条短语也没有。点「新建短语」写一句常用的，之后它就常驻在右栏。' }));
  };

  refresh();
  return { node: body, refresh };
}

/** 独立弹窗（编辑器右栏那颗「管理短语…」走的也是这个） */
export function phrasesManager() {
  const pane = createPhrasesPane();
  return modal({ title: '提示词短语', wide: true, body: pane.node, actions: [{ label: '关闭', kind: 'ghost' }] });
}
