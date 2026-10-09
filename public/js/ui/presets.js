// 参数预设：右栏头部的预设菜单（选用 / 另存 / 覆盖）+ 管理弹窗
'use strict';
import { el, fill } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api } from '../core/api.js';
import { t } from '../core/i18n.js';
import { store, PARAM_RANGE } from '../state.js';
import { modal, confirm, askName } from './modal.js';
import { toastOk, toastErr } from './toast.js';
import { fmtNum } from '../core/format.js';

const summary = p => t('pre.summary', {
  steps: p.steps ?? '—', cfg: fmtNum(p.cfg, 0.5), lora: (p.loras || []).filter(l => l.enabled).length,
});
// 存键名而不是文案：字典是 boot 里异步装的，模块级常量取文案只会拿到 ⟨键名⟩
const nameHint = () => ({ placeholder: t('pre.namePh'), hint: t('pre.nameHint') });

/**
 * 自由撰写一份预设。管理面板以前只有"另存为"（快照当前面板），
 * 没有面板可快照的时候（首页进来、或就是想空白写一条）根本没有入口。
 */
function askPreset({ title, name = '', prompt = '', negative = '', steps = 20, cfg = 3 }) {
  return new Promise(res => {
    let settled = false;
    const done = v => { if (!settled) { settled = true; res(v); } };
    const nm = el('input.input', { type: 'text', maxlength: '40', placeholder: t('pre.namePh'), value: name });
    const pt = el('textarea.textarea', { rows: '5', placeholder: t('pre.newPh'), spellcheck: 'false' });
    pt.value = prompt;
    const ng = el('textarea.textarea', { rows: '2', placeholder: t('pre.negPh'), spellcheck: 'false' });
    ng.value = negative;
    const st = el('input.input', { type: 'number', min: String(PARAM_RANGE.steps[0]), max: String(PARAM_RANGE.steps[1]), step: '1', value: String(steps) });
    const cf = el('input.input', { type: 'number', min: String(PARAM_RANGE.cfg[0]), max: String(PARAM_RANGE.cfg[1]), step: '0.5', value: String(cfg) });
    const fld = (label, node) => el('div', { style: { display: 'grid', gap: '4px' } }, el('span.muted', { text: label }), node);
    const save = () => {
      const v = {
        name: nm.value.trim(), prompt: pt.value, negative: ng.value,
        steps: Math.min(PARAM_RANGE.steps[1], Math.max(PARAM_RANGE.steps[0], Number(st.value) || 20)),
        cfg: Math.min(PARAM_RANGE.cfg[1], Math.max(PARAM_RANGE.cfg[0], Number(cf.value) || 3)),
      };
      if (!v.name) { toastErr(t('pre.needName')); nm.focus(); return; }
      done(v);
      m.close('save');
    };
    const box = el('div', { style: { display: 'grid', gap: '10px' } },
      fld(t('pre.name'), nm), fld(t('pre.prompt'), pt), fld(t('pre.negative'), ng),
      el('div', { style: { display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '10px' } }, fld(t('pre.steps'), st), fld(t('pre.cfg'), cf)),
      el('p.muted', { text: t('pre.loraNote') }),
      el('div', { style: { display: 'flex', justifyContent: 'flex-end', gap: '8px' } },
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('common.cancel'), onclick: () => { done(null); m.close('cancel'); } }),
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: t('common.save'), onclick: save })));
    const m = modal({ title, body: box, onClose: () => done(null) });
    requestAnimationFrame(() => nm.focus());
  });
}

/** 预设里可带走的字段：种子与随机开关属于单次运行，不进预设 */
const payload = name => {
  const s = store.peek('settings') || {};
  return {
    name: String(name || '').trim().slice(0, 40),
    prompt: s.prompt || '', negative: s.negative || '',
    steps: s.steps, cfg: s.cfg,
    loras: (s.loras || []).map(l => ({ name: l.name, strength: l.strength, enabled: !!l.enabled && !l.missing })),
  };
};

/** 面板当前参数是否仍等于某个预设（LoRA 只比预设里带的那几条） */
function matches(p, s) {
  if (!p || !s) return false;
  for (const k of ['prompt', 'negative', 'steps', 'cfg']) if (String(p[k] ?? '') !== String(s[k] ?? '')) return false;
  return (p.loras || []).every(l => {
    const hit = (s.loras || []).find(x => x.name === l.name);
    return hit && Number(hit.strength) === Number(l.strength) && !!hit.enabled === !!l.enabled;
  });
}

export function createPresetMenu({ onApply }) {
  let list = [];
  let appliedId = null;
  let open = false;

  const label = el('span.nowrap');
  const btn = el('button.btn.btn--ghost.btn--sm', {
    type: 'button', 'aria-haspopup': 'true', 'aria-expanded': 'false', 'data-tip': t('pre.tip'),
    onclick: () => (open ? close() : show()),
  }, el('span.ic', { html: icon('sparkles', { cls: 'icon icon--sm' }) }), label);

  const panel = el('div.pmenu');
  const projectId = () => store.peek('project')?.id || null;

  const syncLabel = () => {
    const p = list.find(x => x.id === appliedId);
    const dirty = p && !matches(p, store.peek('settings'));
    // 三种拼法各一条键：中英语序不同，把「·已改动」当碎片接在句子后面英文就读不通
    label.textContent = !p ? t('pre.label')
      : dirty ? t('pre.labelDirty', { name: p.name })
      : t('pre.labelApplied', { name: p.name });
    btn.classList.toggle('is-on', !!p);
    btn.classList.toggle('is-dirty', !!dirty);
  };
  store.subscribe((s, key) => { if (key === 'settings' || key === 'project') syncLabel(); });

  async function load() {
    try { list = await api.presets(projectId()); } catch (e) { list = []; toastErr(t('pre.readFail'), e.message); }
    if (appliedId && !list.some(p => p.id === appliedId)) appliedId = null;
    syncLabel();
    return list;
  }

  function close() {
    open = false; btn.setAttribute('aria-expanded', 'false');
    panel.remove();
    document.removeEventListener('pointerdown', outside, true);
    document.removeEventListener('keydown', onKey, true);
  }
  function outside(e) { if (panel.contains(e.target) || btn.contains(e.target)) return; close(); }
  function onKey(e) { if (e.key === 'Escape') { e.stopPropagation(); close(); } }

  function show() {
    open = true;
    btn.setAttribute('aria-expanded', 'true');
    const r = btn.getBoundingClientRect();
    panel.style.top = `${r.bottom + 4}px`;
    panel.style.left = `${Math.max(8, Math.min(r.left, innerWidth - 296))}px`;
    paintPanel();
    document.body.append(panel);
    document.addEventListener('pointerdown', outside, true);
    document.addEventListener('keydown', onKey, true);
  }

  function paintPanel() {
    const rows = list.length
      ? list.map(p => el('button.pmenu__it', {
          type: 'button',
          class: `pmenu__it${p.id === appliedId ? ' is-on' : ''}`,
          onclick: () => { appliedId = p.id; syncLabel(); close(); onApply?.(p); },
        },
        el('span.pmenu__nm.nowrap', { text: p.name }),
        el('span.pmenu__sum', { text: summary(p) }),
        p.project_id ? el('span.pmenu__tag', { text: t('pre.thisProject') }) : null,
        p.id === appliedId ? el('span.pmenu__ck', { html: icon('check', { cls: 'icon icon--sm' }) }) : null))
      : [el('p.pmenu__empty', { text: t('pre.emptyMenu') })];

    fill(panel,
      el('div.pmenu__hd', {}, el('b', { text: t('pre.title') }), el('span', { text: t('pre.count', { n: list.length }) })),
      el('div.pmenu__list', {}, ...rows),
      el('div.pmenu__acts', {},
        el('button.btn.btn--sm', { type: 'button', text: t('pre.saveAs'), onclick: () => saveAs() }),
        appliedId
          ? el('button.btn.btn--sm.btn--ghost', { type: 'button', text: t('pre.overwrite'), onclick: () => overwrite() })
          : el('button.btn.btn--sm.btn--ghost', { type: 'button', text: t('pre.manage'), onclick: () => manage() })),
    );
  }

  async function saveAs() {
    /* 撞名会被后端 400 打回来：对话框要带着刚打的那个名字重开，而不是让人重敲一遍 */
    let name = '';
    for (;;) {
      name = await askName(t('pre.saveAsAsk'), name, nameHint());
      if (!name) return;
      try {
        const p = await api.savePreset({ ...payload(name), project_id: projectId() });
        await load();
        appliedId = p.id; syncLabel();
        toastOk(t('pre.saved'), `${p.name} · ${summary(p)}`);
        paintPanel();
        return;
      } catch (e) { toastErr(t('pre.saveFail'), e.message); }
    }
  }

  async function overwrite() {
    const p = list.find(x => x.id === appliedId);
    if (!p) return;
    const ok = await confirm({ title: t('pre.overTitle', { name: p.name }), text: t('pre.overText'), okLabel: t('pre.overBtn') });
    if (!ok) return;
    try {
      await api.updatePreset(p.id, { ...payload(p.name), scope: p.project_id ? String(p.project_id) : 'global' });
      await load(); paintPanel();
      toastOk(t('pre.overDone'), p.name);
    } catch (e) { toastErr(t('pre.overFail'), e.message); }
  }

  function manage() {
    presetManager({ projectId: projectId(), onChanged: async () => { await load(); if (open) paintPanel(); } });
  }

  syncLabel();
  load();
  return { node: btn, refresh: load, applied: () => appliedId };
}

/** 预设列表分区：新增 / 改名 / 删除 / 切作用域。设置弹窗与旧的独立入口共用 */
export function createPresetsPane({ projectId = null, onChanged } = {}) {
  const body = el('div.dlg-flow');
  let list = [];

  async function refresh() {
    try { list = await api.presets(projectId); } catch (e) { list = []; toastErr(t('pre.readFail'), e.message); }
    paint();
  }

  async function add() {
    let seed = {};
    for (;;) {
      const v = await askPreset({ title: t('pre.newBtn'), ...seed });
      if (!v) return;
      seed = v;
      try {
        await api.savePreset({ ...v, kind: 'preset', project_id: projectId, loras: [] });
        await refresh();
        onChanged?.();
        toastOk(t('pre.created'), v.name);
        return;
      } catch (e) { toastErr(t('pre.saveFail'), e.message); }
    }
  }

  const head = () => el('div', { style: { display: 'grid', gap: '8px' } },
    el('p.muted', { text: t('pre.intro') }),
    el('div', { style: { display: 'flex', gap: '8px', alignItems: 'center' } },
      el('button.btn.btn--primary.btn--sm', { type: 'button', text: t('pre.newBtn'), onclick: () => add() }),
      list.length ? el('span.muted', { text: t('pre.count', { n: list.length }) }) : null));

  const paint = () => {
    if (!list.length) {
      fill(body, head(), el('p.muted', { text: t('pre.emptyPane') }));
      return;
    }
    fill(body, head(), ...list.map(p => el('div.be-row', {},
      el('div.be-row__main', {},
        el('b.be-row__url.nowrap', { text: p.name }),
        el('span.be-row__meta', { text: `${summary(p)} · ${t(p.project_id ? 'pre.thisProject' : 'pre.scopeGlobal')}` })),
      el('div.be-row__acts', {},
        el('button.btn.btn--sm.btn--ghost', { type: 'button', text: t(p.project_id ? 'pre.toGlobal' : 'pre.toProject'), disabled: !p.project_id && !projectId, onclick: async () => {
          const scope = p.project_id ? 'global' : String(projectId);
          try { const next = await api.updatePreset(p.id, { ...p, scope }); Object.assign(p, next); paint(); onChanged?.(); toastOk(t('pre.scopeChanged'), p.name); }
          catch (e) { toastErr(t('pre.scopeFail'), e.message); }
        } }),
        el('button.btn.btn--sm.btn--ghost', { type: 'button', text: t('pre.rename'), onclick: async () => {
          let n = p.name;
          for (;;) {
            n = await askName(t('pre.renameAsk'), n, nameHint());
            if (!n) return;
            try { const next = await api.updatePreset(p.id, { ...p, name: n }); Object.assign(p, next); paint(); onChanged?.(); toastOk(t('pre.renamed'), n); break; }
            catch (e) { toastErr(t('pre.renameFail'), e.message); }
          }
        } }),
        el('button.btn.btn--sm.btn--danger', { type: 'button', text: t('pre.del'), onclick: async () => {
          const ok = await confirm({ title: t('pre.delTitle', { name: p.name }), text: t('pre.delText'), okLabel: t('pre.del'), danger: true });
          if (!ok) return;
          try { await api.deletePreset(p.id); list = list.filter(x => x.id !== p.id); paint(); onChanged?.(); toastOk(t('pre.deleted'), p.name); }
          catch (e) { toastErr(t('pre.delFail'), e.message); }
        } })))));
  };
  refresh();
  return { node: body, refresh };
}

/** 独立的管理弹窗（首页卡片走这个，没有项目上下文） */
export function presetManager({ projectId = null, onChanged } = {}) {
  const pane = createPresetsPane({ projectId, onChanged });
  modal({ title: t('pre.managerTitle'), wide: true, body: pane.node, actions: [{ label: t('common.close'), kind: 'ghost' }] });
}
