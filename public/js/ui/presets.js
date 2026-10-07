// 参数预设：右栏头部的预设菜单（选用 / 另存 / 覆盖）+ 管理弹窗
'use strict';
import { el, fill } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api } from '../core/api.js';
import { store, PARAM_RANGE } from '../state.js';
import { modal, confirm, askName } from './modal.js';
import { toastOk, toastErr } from './toast.js';
import { fmtNum } from '../core/format.js';

const summary = p => `${p.steps ?? '—'}步 · CFG${fmtNum(p.cfg, 0.5)} · LoRA ${(p.loras || []).filter(l => l.enabled).length}`;
const NAME_HINT = { placeholder: '如 皮肤精修常用', hint: '只存提示词、负面、步数、CFG 与 LoRA 链；种子不进预设。' };

/**
 * 自由撰写一份预设。管理面板以前只有"另存为"（快照当前面板），
 * 没有面板可快照的时候（首页进来、或就是想空白写一条）根本没有入口。
 */
function askPreset({ title, name = '', prompt = '', negative = '', steps = 20, cfg = 3 }) {
  return new Promise(res => {
    let settled = false;
    const done = v => { if (!settled) { settled = true; res(v); } };
    const nm = el('input.input', { type: 'text', maxlength: '40', placeholder: '如 皮肤精修常用', value: name });
    const pt = el('textarea.textarea', { rows: '5', placeholder: '描述要改成什么样，一段一个主体一个动作', spellcheck: 'false' });
    pt.value = prompt;
    const ng = el('textarea.textarea', { rows: '2', placeholder: '多余的手指、塑料感皮肤、噪点…', spellcheck: 'false' });
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
      if (!v.name) { toastErr('先起个名字'); nm.focus(); return; }
      done(v);
      m.close('save');
    };
    const box = el('div', { style: { display: 'grid', gap: '10px' } },
      fld('名字', nm), fld('正向指令', pt), fld('负面提示词', ng),
      el('div', { style: { display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '10px' } }, fld('采样步数', st), fld('CFG 引导', cf)),
      el('p.muted', { text: 'LoRA 链按本机工作流当前的挂法走；要连 LoRA 一起存，就先在编辑器里调好再「另存为新预设」。种子不进预设。' }),
      el('div', { style: { display: 'flex', justifyContent: 'flex-end', gap: '8px' } },
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '取消', onclick: () => { done(null); m.close('cancel'); } }),
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: '保存', onclick: save })));
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
    type: 'button', 'aria-haspopup': 'true', 'aria-expanded': 'false', 'data-tip': '把这套参数存成预设，下次直接选',
    onclick: () => (open ? close() : show()),
  }, el('span.ic', { html: icon('sparkles', { cls: 'icon icon--sm' }) }), label);

  const panel = el('div.pmenu');
  const projectId = () => store.peek('project')?.id || null;

  const syncLabel = () => {
    const p = list.find(x => x.id === appliedId);
    const dirty = p && !matches(p, store.peek('settings'));
    label.textContent = p ? `预设 · ${p.name}${dirty ? ' ·已改动' : ''}` : '预设';
    btn.classList.toggle('is-on', !!p);
    btn.classList.toggle('is-dirty', !!dirty);
  };
  store.subscribe((s, key) => { if (key === 'settings' || key === 'project') syncLabel(); });

  async function load() {
    try { list = await api.presets(projectId()); } catch (e) { list = []; toastErr('读不到预设', e.message); }
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
        p.project_id ? el('span.pmenu__tag', { text: '本项目' }) : null,
        p.id === appliedId ? el('span.pmenu__ck', { html: icon('check', { cls: 'icon icon--sm' }) }) : null))
      : [el('p.pmenu__empty', { text: '还没有预设。把参数调好后回来点「另存为预设」。' })];

    fill(panel,
      el('div.pmenu__hd', {}, el('b', { text: '参数预设' }), el('span', { text: `${list.length} 个` })),
      el('div.pmenu__list', {}, ...rows),
      el('div.pmenu__acts', {},
        el('button.btn.btn--sm', { type: 'button', text: '另存为新预设', onclick: () => saveAs() }),
        appliedId
          ? el('button.btn.btn--sm.btn--ghost', { type: 'button', text: '覆盖当前', onclick: () => overwrite() })
          : el('button.btn.btn--sm.btn--ghost', { type: 'button', text: '管理…', onclick: () => manage() })),
    );
  }

  async function saveAs() {
    /* 撞名会被后端 400 打回来：对话框要带着刚打的那个名字重开，而不是让人重敲一遍 */
    let name = '';
    for (;;) {
      name = await askName('另存为预设', name, NAME_HINT);
      if (!name) return;
      try {
        const p = await api.savePreset({ ...payload(name), project_id: projectId() });
        await load();
        appliedId = p.id; syncLabel();
        toastOk('预设已保存', `${p.name} · ${summary(p)}`);
        paintPanel();
        return;
      } catch (e) { toastErr('保存失败，改个名字再来', e.message); }
    }
  }

  async function overwrite() {
    const p = list.find(x => x.id === appliedId);
    if (!p) return;
    const ok = await confirm({ title: `覆盖「${p.name}」`, text: '用当前参数替换这个预设的内容，名字不变。', okLabel: '覆盖' });
    if (!ok) return;
    try {
      await api.updatePreset(p.id, { ...payload(p.name), scope: p.project_id ? String(p.project_id) : 'global' });
      await load(); paintPanel();
      toastOk('预设已覆盖', p.name);
    } catch (e) { toastErr('覆盖失败', e.message); }
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
    try { list = await api.presets(projectId); } catch (e) { list = []; toastErr('读不到预设', e.message); }
    paint();
  }

  async function add() {
    let seed = {};
    for (;;) {
      const v = await askPreset({ title: '新建预设', ...seed });
      if (!v) return;
      seed = v;
      try {
        await api.savePreset({ ...v, kind: 'preset', project_id: projectId, loras: [] });
        await refresh();
        onChanged?.();
        toastOk('预设已建好', v.name);
        return;
      } catch (e) { toastErr('保存失败，改个名字再来', e.message); }
    }
  }

  const head = () => el('div', { style: { display: 'grid', gap: '8px' } },
    el('p.muted', { text: '预设是一整份参数快照：套下去就把指令框整个替换掉。想叠加着改用的是「提示词短语」。' }),
    el('div', { style: { display: 'flex', gap: '8px', alignItems: 'center' } },
      el('button.btn.btn--primary.btn--sm', { type: 'button', text: '新建预设', onclick: () => add() }),
      list.length ? el('span.muted', { text: `${list.length} 个` }) : null));

  const paint = () => {
    if (!list.length) {
      fill(body, head(), el('p.muted', { text: '还没有预设。可以现在就写一份，也可以在编辑器右侧把参数调好后「另存为新预设」。' }));
      return;
    }
    fill(body, head(), ...list.map(p => el('div.be-row', {},
      el('div.be-row__main', {},
        el('b.be-row__url.nowrap', { text: p.name }),
        el('span.be-row__meta', { text: `${summary(p)} · ${p.project_id ? '本项目' : '全局'}` })),
      el('div.be-row__acts', {},
        el('button.btn.btn--sm.btn--ghost', { type: 'button', text: p.project_id ? '升为全局' : '收到项目', disabled: !p.project_id && !projectId, onclick: async () => {
          const scope = p.project_id ? 'global' : String(projectId);
          try { const next = await api.updatePreset(p.id, { ...p, scope }); Object.assign(p, next); paint(); onChanged?.(); toastOk('作用域已改', p.name); }
          catch (e) { toastErr('改不动', e.message); }
        } }),
        el('button.btn.btn--sm.btn--ghost', { type: 'button', text: '改名', onclick: async () => {
          let n = p.name;
          for (;;) {
            n = await askName('重命名预设', n, NAME_HINT);
            if (!n) return;
            try { const next = await api.updatePreset(p.id, { ...p, name: n }); Object.assign(p, next); paint(); onChanged?.(); toastOk('已改名', n); break; }
            catch (e) { toastErr('改名失败，改一个再来', e.message); }
          }
        } }),
        el('button.btn.btn--sm.btn--danger', { type: 'button', text: '删除', onclick: async () => {
          const ok = await confirm({ title: `删除「${p.name}」`, text: '只是删掉这个模板，已生成的结果不受影响。', okLabel: '删除', danger: true });
          if (!ok) return;
          try { await api.deletePreset(p.id); list = list.filter(x => x.id !== p.id); paint(); onChanged?.(); toastOk('预设已删除', p.name); }
          catch (e) { toastErr('删除失败', e.message); }
        } })))));
  };
  refresh();
  return { node: body, refresh };
}

/** 独立的管理弹窗（首页卡片走这个，没有项目上下文） */
export function presetManager({ projectId = null, onChanged } = {}) {
  const pane = createPresetsPane({ projectId, onChanged });
  modal({ title: '参数预设管理', wide: true, body: pane.node, actions: [{ label: '关闭', kind: 'ghost' }] });
}
