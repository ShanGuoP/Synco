// 右栏属性面板：快速指令胶囊 + 「指令 / 采样 / LoRA / 本地调整」子标签 + 底部提交区
// 胶囊行 → 子标签行 → 参数行（标签+数值直输+滑杆）
'use strict';
import { el, fill } from '../../core/dom.js';
import { icon } from '../../core/icons.js';
import { t } from '../../core/i18n.js';
import { makeProw } from '../../ui/controls.js';
import { makeSteps, STAGES } from '../../ui/progress.js';
import { store, patchSettings, defaultsFromCfg, effMode, PARAM_RANGE } from '../../state.js';
import { phrasesManager } from '../../ui/phrases.js';
import { baseName, fmtNum } from '../../core/format.js';

/* 「本地调整」那一页的内容由 adjust.js 交进来：生成参数与图像调整是两套状态机，
   共用的是这一列的骨架和标签条，不是数据结构 */
const TABS = [['prompt', 'pm.tabPrompt'], ['sample', 'pm.tabSample'], ['lora', 'pm.tabLora'], ['adjust', 'pm.tabAdjust']];

/* 与后端 api/common.rs 的 SEED_MAX 对齐：滑杆范围就是实际会提交的范围，回填才不会变成另一个种子 */
export const SEED_MAX = 2147483647;

/** 折叠分组：▸ 标题 ……… 徽标 */
function grp(title, { badge, collapsed = false } = {}, ...body) {
  const caret = el('span.caret', { html: icon('right', { cls: 'icon icon--sm' }) });
  const bd = el('div.grp__bd', {}, ...body);
  const hd = el('button.grp__hd', { type: 'button', 'aria-expanded': String(!collapsed) },
    caret, el('span', { text: title }),
    badge ? el('span.badge', { text: badge }) : null);
  const node = el('div.grp', { class: `grp${collapsed ? ' is-collapsed' : ''}` }, hd, bd);
  hd.addEventListener('click', () => {
    const c = node.classList.toggle('is-collapsed');
    hd.setAttribute('aria-expanded', String(!c));
  });
  return node;
}

export function createParams({ onSubmit, onStop, onMode, onInk }) {
  let tab = 'prompt';
  let busy = false;
  let loras = [];

  /* ---------------- 指令页 ---------------- */
  const ta = el('textarea.textarea', { rows: '7', placeholder: t('pm.promptPh'), spellcheck: 'false' });
  const neg = el('textarea.textarea', { rows: '3', placeholder: t('pm.negPh'), spellcheck: 'false' });
  const count = el('span.badge');

  /* 胶囊行：内容来自库里（设置 → 提示词短语，或就地「管理短语…」），不再是写死的字面量。
     最后一颗是管理入口，它没有 data-phrase，所以不会参与 is-on 判定。 */
  const chips = el('div.chips');

  function paintChips() {
    const list = (store.peek('phrases') || []).filter(p => p.prompt);
    fill(chips,
      ...list.map(p => el('button.chip-s', {
        type: 'button', text: p.name, dataset: { phrase: p.prompt },
        onclick: () => {
          ta.value = togglePhrase(ta.value, p.prompt, chips.querySelector(`[data-phrase="${CSS.escape(p.prompt)}"]`)?.classList.contains('is-on'));
          syncPrompt();
        },
      })),
      el('button.chip-s.chip-s--mgr', {
        type: 'button', 'data-tip': t('pm.chipMgrTip'),
        html: icon('edit', { cls: 'icon icon--sm' }) + `<span>${t('pm.chipMgr')}</span>`,
        onclick: () => phrasesManager(),
      }));
    syncPrompt();
  }

  function syncPrompt() {
    const cur = ta.value || '';
    for (const c of chips.querySelectorAll('.chip-s[data-phrase]')) c.classList.toggle('is-on', cur.includes(c.dataset.phrase));
    count.textContent = t('pm.charCount', { n: cur.length });
    patchSettings({ prompt: cur });
  }
  ta.addEventListener('input', syncPrompt);
  neg.addEventListener('input', () => patchSettings({ negative: neg.value }));
  paintChips();
  /* 在设置里改完短语，右栏这一排要当场跟着变（编辑器骨架只建一次） */
  store.subscribe((s, key) => { if (key === 'phrases') paintChips(); });

  const panePrompt = el('div', { style: { display: 'grid', gap: '14px' } },
    grp(t('pm.prompt'), { badge: t('pm.charCount', { n: 0 }) }, ta),
    grp(t('pm.negative'), { badge: t('pm.optionalBadge'), collapsed: true }, neg),
  );
  /* 把 grp 里的徽标换成可更新的计数节点 */
  panePrompt.children[0].querySelector('.badge').replaceWith(count);
  const cloudNote = el('p', { class: 'muted', hidden: true, text: t('pm.cloudNote') });
  /* 整图重绘唯一的画质旋钮就是输出长边，所以放进提交区而不是设置里 */
  const EDGE_STEPS = [['pm.edgeFollow', 0], ['pm.edge1k', 1024], ['pm.edge15k', 1536], ['pm.edge2k', 2048], ['pm.edge3k', 3072], ['pm.edge4k', 3840]];
  let edgePick = 0;
  const edgeChips = el('div.chips', {}, ...EDGE_STEPS.map(([key, n]) => el('button.chip-s', {
    type: 'button', text: t(key), dataset: { n: String(n) }, 'data-tip': n ? t('pm.edgeTip', { n }) : t('pm.edgeUseSetting'),
    onclick: () => { edgePick = n; patchSettings({ edge: n || null }); paintEdge(); },
  })));
  const paintEdge = () => { for (const c of edgeChips.children) c.classList.toggle('is-on', +c.dataset.n === edgePick); };
  const edgeRow = el('div', { hidden: true, style: { display: 'grid', gap: '6px' } },
    el('span.muted', { text: t('pm.edgeRow') }), edgeChips);
  panePrompt.append(cloudNote, edgeRow);

  /* ---------------- 采样页 ---------------- */
  const stepsCtl = makeProw({ label: t('pm.steps'), min: PARAM_RANGE.steps[0], max: PARAM_RANGE.steps[1], step: 1, value: 20, tip: t('pm.stepsTip'), onChange: v => patchSettings({ steps: v }) });
  const cfgCtl = makeProw({ label: t('pm.cfg'), min: PARAM_RANGE.cfg[0], max: PARAM_RANGE.cfg[1], step: 0.5, value: 3, tip: t('pm.cfgTip'), onChange: v => patchSettings({ cfg: v }) });
  const seedCtl = makeProw({ label: t('pm.seed'), min: 0, max: SEED_MAX, step: 1, value: 0, tip: t('pm.seedTip'), onChange: v => patchSettings({ seed: v }) });
  const rndCtl = toggleRow(t('pm.randSeed'), v => { patchSettings({ randomSeed: v }); seedCtl.setDisabled(v); });

  const paneSample = el('div', { style: { display: 'grid', gap: '10px' } },
    stepsCtl.node, cfgCtl.node, rndCtl.node, seedCtl.node,
    el('p', { class: 'muted', text: t('pm.batchNote') }));

  /* ---------------- LoRA 页 ---------------- */
  const loraBox = el('div', { style: { display: 'grid', gap: '10px' } });
  const paneLora = el('div', { style: { display: 'grid', gap: '10px' } }, loraBox,
    el('p', { class: 'muted', text: t('pm.loraNote') }));

  function buildLoras(list) {
    // missing 是"本机没有这个 LoRA"的标记，重建对象时得带上：丢了它，下面那句 l.missing 永远为假，
    // 界面上就再也不标那一格
    loras = list.map(l => ({ name: l.name, strength: l.strength, enabled: l.enabled !== false, missing: l.missing === true }));
    if (!list.length) { fill(loraBox, el('p', { class: 'muted', text: t('pm.noLora') })); return; }
    fill(loraBox, loras.map((l, i) => {
      const who = baseName(l.name);
      const num = el('input.input.input--num', { type: 'number', min: 0, max: 2, step: 0.05, value: fmtNum(l.strength, 0.05), 'aria-label': t('pm.loraStrengthAria', { name: who }) });
      const sw = el('button.toggle', { type: 'button', role: 'switch', 'aria-checked': String(l.enabled), 'aria-label': t('pm.loraEnableAria', { name: who }) });
      const row = el('div.lora', { class: `lora${l.enabled ? '' : ' is-off'}` },
        el('div.lora__hd', {}, el('span.lora__nm', { title: l.name, text: who }),
          l.missing ? el('span.lora__miss', { text: t('pm.loraMissing') }) : null, num, sw));
      const pr = makeProw({ label: t('pm.strength'), min: 0, max: 2, step: 0.05, value: l.strength,
        onChange: v => { l.strength = v; num.value = fmtNum(v, 0.05); emitLoras(); } });
      row.append(pr.node);
      num.addEventListener('change', () => {
        const raw = Math.max(0, Math.min(2, parseFloat(num.value) || 0));
        pr.set(raw);
      });
      sw.addEventListener('click', () => {
        l.enabled = !l.enabled;
        sw.setAttribute('aria-checked', String(l.enabled));
        row.classList.toggle('is-off', !l.enabled);
        pr.setDisabled(!l.enabled);
        emitLoras();
      });
      pr.setDisabled(!l.enabled);
      return row;
    }));
  }
  const emitLoras = () => patchSettings({ loras: loras.map(l => ({ name: l.name, strength: l.strength, enabled: l.enabled })) });

  /* ---------------- 生成方式：就地切本机 / 云端 ---------------- */
  const MODES = [['comfyui', 'pm.modeLocal'], ['cloud', 'pm.modeCloud']];
  const modeSeg = el('div.seg', {}, ...MODES.map(([k, key]) => el('button.seg__it', {
    type: 'button', dataset: { m: k }, text: t(key), 'data-tip': t(k === 'cloud' ? 'pm.modeCloudTip' : 'pm.modeLocalTip'),
    onclick: () => onMode?.(k),
  })));
  const modeTip = el('span.muted.nowrap');
  const modeRow = el('div.ed-mode', {}, el('span.ed-mode__lab', { text: t('pm.modeLabel') }), modeSeg, modeTip);

  function paintMode() {
    const cur = effMode();
    for (const b of modeSeg.children) b.classList.toggle('is-on', b.dataset.m === cur);
    const cloud = store.peek('cloud') || {};
    // 两条路的出图口径不一样，把"现在打到哪、按什么尺寸出"写在开关旁边，别让人以为切了是等价的
    modeTip.textContent = cur === 'cloud'
      ? t('pm.modeCloudName', { model: cloud.model || t('chip.noModel') })
      : String(store.peek('comfy') || '').replace(/^https?:\/\//, '') || t('pm.modeLocalHost');
  }

  /* 反向涂抹与整图重绘都只在这条路上有（本地那条要改的是 ComfyUI 工作流本身，是另一件事），
     而且必须把"到底把什么发出去"说当面——三种含义里有一种违反直觉，另一种干脆不看遮罩。 */
  const INK = [
    ['part', 'pm.inkPart', 'pm.inkPartTip'],
    ['keep', 'pm.inkKeep', 'pm.inkKeepTip'],
    ['full', 'pm.inkFull', 'pm.inkFullTip'],
  ];
  const inkSeg = el('div.seg', {}, ...INK.map(([v, key, tip]) => el('button.seg__it', {
    type: 'button', dataset: { v }, text: t(key), 'data-tip': t(tip), onclick: () => onInk?.(v),
  })));
  const inkRow = el('div.ed-mode.ed-ink', { hidden: true },
    el('span.ed-mode__lab', { text: t('pm.inkLabel') }), inkSeg,
    el('span.muted.nowrap', { text: t('pm.inkKeepNote') }));

  function paintInk() {
    const s = store.peek('settings') || {};
    const v = s.full ? 'full' : s.invert ? 'keep' : 'part';
    for (const b of inkSeg.children) b.classList.toggle('is-on', b.dataset.v === v);
    inkRow.querySelector('.muted').textContent = t(v === 'full' ? 'pm.inkFullNote' : v === 'keep' ? 'pm.inkKeepNote' : 'pm.inkPartNote');
  }

  function setScope() {
    paintInk();
    rebuildStages();
  }

  /* ---------------- 子标签 + 主体 ---------------- */
  const body = el('div.prop-body');
  const tabs = el('div.prop-tabs', {}, ...TABS.map(([k, key]) => el('button', {
    type: 'button', dataset: { k }, text: t(key), onclick: () => { tab = k; show(); onTabSwitch?.(k); },
  })));
  /* 「本地调整」那一页由外部交进来（没交进来之前这一格不显示，免得点出一个空壳） */
  let adjustNode = null;
  let onTabSwitch = null;
  function show() {
    for (const b of tabs.children) {
      const hidden = b.dataset.k === 'adjust' && !adjustNode;
      b.hidden = hidden;
      b.classList.toggle('is-on', b.dataset.k === tab);
    }
    const pane = tab === 'prompt' ? panePrompt : tab === 'sample' ? paneSample : tab === 'lora' ? paneLora : adjustNode;
    fill(body, pane || panePrompt);
    if (tab === 'prompt') syncPrompt();
  }

  /* ---------------- 底部提交区 ---------------- */
  // 阶段名一律由 makeSteps 按 `progress.<key>` 取，这里只说这条路上有哪几格
  const CLOUD_STAGES = [{ key: 'submit' }, { key: 'sample' }, { key: 'stitch' }];
  /* 整张重绘没有缝合这一步——阶段条上挂着"缝合"就是在说一件不会发生的事 */
  const FULL_STAGES = [{ key: 'submit' }, { key: 'sample' }];
  let cloudMode = false;
  let stages = makeSteps(STAGES);
  const stagesBox = el('div', {}, stages.node);
  const runLine = el('div.run-line');
  const goBtn = el('button.btn.btn--accent.btn--block', { type: 'button', onclick: () => onSubmit?.() });
  /* 生成中给一个出口：以前只能干等，ComfyUI 那边都停了这边还转圈 */
  const stopBtn = el('button.btn.btn--ghost.btn--block.btn--sm', { type: 'button', text: t('pm.stopBtn'), hidden: true, onclick: () => onStop?.() });
  const paintGo = () => {
    goBtn.innerHTML = busy
      ? `<span class="spin spin--dark"></span><span class="btn__label">${t('pm.busy')}</span>`
      : `${icon('play', { cls: 'icon icon--sm' })}<span class="btn__label">${t('pm.submit')}</span>`;
    goBtn.classList.toggle('is-busy', busy);
    stopBtn.hidden = !busy;
  };
  paintGo();
  const ft = el('div.prop-ft', {}, goBtn, stopBtn, stagesBox, runLine);

  const resetBtn = el('button.btn.btn--ghost.btn--icon.btn--sm', {
    type: 'button', 'aria-label': t('pm.reset'), 'data-tip': t('pm.reset'),
    html: icon('refresh', { cls: 'icon icon--sm' }), onclick: reset,
  });
  const node = el('aside.ed-prop', {},
    el('div.col-hd', {},
      el('h3', { html: icon('sliders', { cls: 'icon icon--sm' }) + `<span>${t('pm.title')}</span>` }),
      el('span.grow'),
      resetBtn),
    modeRow, inkRow, chips, tabs, body, ft,
  );

  function sync(settings) {
    const s = settings || store.peek('settings') || {};
    ta.value = s.prompt || '';
    neg.value = s.negative || '';
    stepsCtl.set(s.steps ?? 20, true);
    cfgCtl.set(s.cfg ?? 3, true);
    seedCtl.set(s.seed ?? 0, true);
    rndCtl.set(s.randomSeed !== false);
    seedCtl.setDisabled(s.randomSeed !== false);
    edgePick = Number(s.edge) || 0;
    paintEdge();
    buildLoras(s.loras || []);
    paintMode();
    paintInk();
    show();
  }

  /* 回填来源参数时把与之前不同的项标出来，一眼看到改了哪几处 */
  const markable = () => ({
    prompt: ta.closest('.grp'), negative: neg.closest('.grp'),
    steps: stepsCtl.node, cfg: cfgCtl.node, seed: seedCtl.node, randomSeed: rndCtl.node,
  });
  function clearMarks() {
    for (const n of Object.values(markable())) if (n) n.classList.remove('is-changed');
    tabs.querySelector('[data-k="lora"]')?.classList.remove('is-changed');
  }
  function markChanged(keys) {
    const m = markable();
    clearMarks();
    for (const k of keys) {
      if (k === 'loras') tabs.querySelector('[data-k="lora"]')?.classList.add('is-changed');
      else if (m[k]) m[k].classList.add('is-changed');
    }
  }
  /* 用户一动参数，标记就失效 */
  body.addEventListener('input', clearMarks, true);

  /** 回填一整套参数（不提交），changed 是相对回填前发生变化的键 */
  function applySettings(s, changed = []) {
    patchSettings(s);
    sync(s);
    markChanged(changed);
  }

  function reset() {
    const cfg = store.peek('cfg');
    if (!cfg) return;
    const d = defaultsFromCfg(cfg);
    /* 云端只看指令：步数 / CFG / LoRA / 种子那几项没地方生效，别顺手改掉用户本地记着的值 */
    const next = cloudMode ? { prompt: d.prompt, negative: d.negative } : d;
    patchSettings(next);
    sync(store.peek('settings'));
    line(t(cloudMode ? 'pm.restoredCloud' : 'pm.restoredWf'));
  }

  function setBusy(b) { busy = b; paintGo(); }
  /** 出错那一路要显式说 isErr：以前是拿"失败/错误/拒收"去匹配文案，换英文就全漏 */
  function line(txt, isErr) {
    fill(runLine, txt ? el('span', { class: isErr ? 'err' : '', text: txt }) : null);
  }

  /** 阶段条跟着"哪条路 + 发什么出去"走：整张重绘少一格缝合 */
  function stagesFor() {
    if (!cloudMode) return STAGES;
    return (store.peek('settings') || {}).full ? FULL_STAGES : CLOUD_STAGES;
  }
  function rebuildStages() {
    if (busy) return;      // 正在跑的那一张不动阶段条：它的范围在提交那一刻就定死了
    stages = makeSteps(stagesFor());
    fill(stagesBox, stages.node);
  }

  /** 本机 / 云端来回切：云端隐掉采样与 LoRA 页，并换一套阶段名 */
  function setCloud(on) {
    paintMode();          // 开关上的高亮与"打到哪"的提示，无论模式有没有变都要跟着 store 走
    paintInk();
    on = !!on;
    inkRow.hidden = !on;
    if (on === cloudMode) return;
    cloudMode = on;
    // 「本地调整」与生成模式无关（它根本不走后端），云端模式下也要留着；被强制收回的只有采样与 LoRA
    if (on && tab !== 'prompt' && tab !== 'adjust') tab = 'prompt';
    for (const b of tabs.children) b.style.display = on && b.dataset.k !== 'prompt' && b.dataset.k !== 'adjust' ? 'none' : '';
    cloudNote.hidden = !on;
    edgeRow.hidden = !on;
    const tip = t(on ? 'pm.resetCloud' : 'pm.reset');
    resetBtn.setAttribute('aria-label', tip);
    resetBtn.setAttribute('data-tip', tip);
    rebuildStages();
    show();
  }

  return {
    node, sync, setBusy, line, applySettings, clearMarks, setCloud, setScope,
    /** 把「本地调整」那一页挂进来（编辑器骨架只建一次，所以这是一次性的） */
    setAdjust(node2, onSwitch) { adjustNode = node2; onTabSwitch = onSwitch || null; show(); },
    get tab() { return tab; },
    get stages() { return stages; },
    get cloud() { return cloudMode; },
    get busy() { return busy; },      // .is-busy 只挡鼠标，键盘路径要自己看一眼
  };
}

function toggleRow(label, onChange) {
  let on = true;
  const sw = el('button.toggle', { type: 'button', role: 'switch', 'aria-checked': 'true', 'aria-label': label });
  sw.addEventListener('click', () => { on = !on; sw.setAttribute('aria-checked', String(on)); onChange?.(on); });
  const node = el('div.prow', { style: { display: 'flex', alignItems: 'center', justifyContent: 'space-between', minHeight: '28px' } },
    el('span.prow__name', { text: label, 'data-tip': t('pm.randSeedTip') }), sw);
  return { node, set(v) { on = !!v; sw.setAttribute('aria-checked', String(on)); } };
}

/**
 * 并入 / 移出这句提示词。
 * 短语本身就带中文逗号，所以"移出"必须按整句子串删：原来先按逗号切成列表项再等值比较，
 * 句子被切成两半，一项都匹配不上——点掉之后残段永远留在指令里。
 * 胶囊的 is-on 由文本回灌，这里再以文本为准判一次，重载后也不会重复追加。
 * 画布视图的同一排胶囊走的也是这一个函数，两处各写一份迟早会漂。
 */
export function togglePhrase(cur, phrase, wasOn) {
  const text = String(cur || '');
  if (wasOn || text.includes(phrase)) {
    return text.replace(phrase, '')
      .replace(/[，,]\s*(?=[，,]|$)/g, '')
      .replace(/^\s*[，,]\s*/, '')
      .trim();
  }
  const list = text.split(/[,，]\s*/).map(x => x.trim()).filter(Boolean);
  return [...list, phrase].join('，');
}
