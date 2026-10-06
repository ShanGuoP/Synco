// 右栏属性面板：快速指令胶囊 + 「指令 / 采样 / LoRA」子标签 + 底部提交区
// 胶囊行 → 子标签行 → 参数行（标签+数值直输+滑杆）
'use strict';
import { el, fill } from '../../core/dom.js';
import { icon } from '../../core/icons.js';
import { makeProw } from '../../ui/controls.js';
import { makeSteps, STAGES } from '../../ui/progress.js';
import { store, patchSettings, defaultsFromCfg } from '../../state.js';
import { baseName, fmtNum } from '../../core/format.js';

/** 点胶囊 = 把这句话并入/移出正向指令 */
const PHRASES = [
  ['皮肤精修', '皮肤质感细腻通透，保留毛孔与绒毛细节'],
  ['去碎发', '去除杂乱碎发，发际线与鬓角干净'],
  ['服装平整', '服装褶皱自然平整，材质纹理清晰'],
  ['背景干净', '背景杂物与高光溢出消除，画面干净'],
  ['光影统一', '光线柔和统一，与周围环境色温一致'],
  ['手部修正', '手指结构与数量正确，关节自然'],
  ['只改遮罩区', '只编辑遮罩区域，其余保持原样'],
];

const TABS = [['prompt', '指令'], ['sample', '采样'], ['lora', 'LoRA']];

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

export function createParams({ onSubmit, onStop }) {
  let tab = 'prompt';
  let busy = false;
  let loras = [];

  /* ---------------- 指令页 ---------------- */
  const ta = el('textarea.textarea', { rows: '7', placeholder: '描述要改成什么样，一段一个主体一个动作', spellcheck: 'false' });
  const neg = el('textarea.textarea', { rows: '3', placeholder: '多余的手指、塑料感皮肤、噪点…', spellcheck: 'false' });
  const count = el('span.badge');

  const chips = el('div.chips', {}, ...PHRASES.map(([label, phrase]) => el('button.chip-s', {
    type: 'button', text: label, dataset: { phrase },
    onclick: () => { ta.value = togglePhrase(ta.value, phrase, chips.querySelector(`[data-phrase="${CSS.escape(phrase)}"]`)?.classList.contains('is-on')); syncPrompt(); },
  })));

  function syncPrompt() {
    const cur = ta.value || '';
    for (const c of chips.children) c.classList.toggle('is-on', cur.includes(c.dataset.phrase));
    count.textContent = `${cur.length} 字`;
    patchSettings({ prompt: cur });
  }
  ta.addEventListener('input', syncPrompt);
  neg.addEventListener('input', () => patchSettings({ negative: neg.value }));

  const panePrompt = el('div', { style: { display: 'grid', gap: '14px' } },
    grp('正向指令', { badge: '0 字' }, ta),
    grp('负面提示词', { badge: '可留空', collapsed: true }, neg),
  );
  /* 把 grp 里的徽标换成可更新的计数节点 */
  panePrompt.children[0].querySelector('.badge').replaceWith(count);
  const cloudNote = el('p', { class: 'muted', hidden: true,
    text: '云端模式：负面提示词并进正向一起发出，没有步数 / CFG / LoRA / 种子可调。' });
  /* 整图重绘唯一的画质旋钮就是输出长边，所以放进提交区而不是设置里 */
  const EDGE_STEPS = [['跟随设置', 0], ['1K', 1024], ['1.5K', 1536], ['2K', 2048], ['3K', 3072], ['4K', 3840]];
  let edgePick = 0;
  const edgeChips = el('div.chips', {}, ...EDGE_STEPS.map(([label, n]) => el('button.chip-s', {
    type: 'button', text: label, dataset: { n: String(n) }, 'data-tip': n ? `长边 ${n}` : '用设置里填的长边',
    onclick: () => { edgePick = n; patchSettings({ edge: n || null }); paintEdge(); },
  })));
  const paintEdge = () => { for (const c of edgeChips.children) c.classList.toggle('is-on', +c.dataset.n === edgePick); };
  const edgeRow = el('div', { hidden: true, style: { display: 'grid', gap: '6px' } },
    el('span.muted', { text: '输出尺寸 · 长边，按原图比例自动配' }), edgeChips);
  panePrompt.append(cloudNote, edgeRow);

  /* ---------------- 采样页 ---------------- */
  const stepsCtl = makeProw({ label: '采样步数', min: 4, max: 60, step: 1, value: 20, tip: '越大越细，也越慢', onChange: v => patchSettings({ steps: v }) });
  const cfgCtl = makeProw({ label: 'CFG 引导', min: 0.5, max: 14, step: 0.5, value: 3, tip: '越高越贴指令，过高会糊', onChange: v => patchSettings({ cfg: v }) });
  const seedCtl = makeProw({ label: '种子', min: 0, max: SEED_MAX, step: 1, value: 0, tip: '关闭随机种子时生效', onChange: v => patchSettings({ seed: v }) });
  const rndCtl = toggleRow('随机种子', v => { patchSettings({ randomSeed: v }); seedCtl.setDisabled(v); });

  const paneSample = el('div', { style: { display: 'grid', gap: '10px' } },
    stepsCtl.node, cfgCtl.node, rndCtl.node, seedCtl.node,
    el('p', { class: 'muted', text: '批量提交时每张在种子基础上叠加图片序号，同批不重复、又彼此可比。' }));

  /* ---------------- LoRA 页 ---------------- */
  const loraBox = el('div', { style: { display: 'grid', gap: '10px' } });
  const paneLora = el('div', { style: { display: 'grid', gap: '10px' } }, loraBox,
    el('p', { class: 'muted', text: '开关与强度直接改写提交时的 Lora 链；关掉等于从模型链上摘掉。' }));

  function buildLoras(list) {
    loras = list.map(l => ({ name: l.name, strength: l.strength, enabled: l.enabled !== false }));
    if (!list.length) { fill(loraBox, el('p', { class: 'muted', text: '工作流里没有挂 LoRA。' })); return; }
    fill(loraBox, loras.map((l, i) => {
      const num = el('input.input.input--num', { type: 'number', min: 0, max: 2, step: 0.05, value: fmtNum(l.strength, 0.05), 'aria-label': `${baseName(l.name)} 强度` });
      const sw = el('button.toggle', { type: 'button', role: 'switch', 'aria-checked': String(l.enabled), 'aria-label': `启用 ${baseName(l.name)}` });
      const row = el('div.lora', { class: `lora${l.enabled ? '' : ' is-off'}` },
        el('div.lora__hd', {}, el('span.lora__nm', { title: l.name, text: baseName(l.name) }),
          l.missing ? el('span.lora__miss', { text: '本机没有' }) : null, num, sw));
      const pr = makeProw({ label: '强度', min: 0, max: 2, step: 0.05, value: l.strength,
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

  /* ---------------- 子标签 + 主体 ---------------- */
  const body = el('div.prop-body');
  const tabs = el('div.prop-tabs', {}, ...TABS.map(([k, label]) => el('button', {
    type: 'button', dataset: { k }, text: label, onclick: () => { tab = k; show(); },
  })));
  function show() {
    for (const b of tabs.children) b.classList.toggle('is-on', b.dataset.k === tab);
    fill(body, tab === 'prompt' ? panePrompt : tab === 'sample' ? paneSample : paneLora);
    if (tab === 'prompt') syncPrompt();
  }

  /* ---------------- 底部提交区 ---------------- */
  const CLOUD_STAGES = [
    { key: 'submit', label: '提交' },
    { key: 'sample', label: '云端重绘' },
    { key: 'stitch', label: '缝合' },
  ];
  let cloudMode = false;
  let stages = makeSteps(STAGES);
  const stagesBox = el('div', {}, stages.node);
  const runLine = el('div.run-line');
  const goBtn = el('button.btn.btn--accent.btn--block', { type: 'button', onclick: () => onSubmit?.() });
  /* 生成中给一个出口：以前只能干等，ComfyUI 那边都停了这边还转圈 */
  const stopBtn = el('button.btn.btn--ghost.btn--block.btn--sm', { type: 'button', text: '中断这一张', hidden: true, onclick: () => onStop?.() });
  const paintGo = () => {
    goBtn.innerHTML = busy
      ? '<span class="spin spin--dark"></span><span class="btn__label">生成中…</span>'
      : `${icon('play', { cls: 'icon icon--sm' })}<span class="btn__label">提交生成</span>`;
    goBtn.classList.toggle('is-busy', busy);
    stopBtn.hidden = !busy;
  };
  paintGo();
  const ft = el('div.prop-ft', {}, goBtn, stopBtn, stagesBox, runLine);

  const resetBtn = el('button.btn.btn--ghost.btn--icon.btn--sm', {
    type: 'button', 'aria-label': '重置为工作流默认', 'data-tip': '重置为工作流默认',
    html: icon('refresh', { cls: 'icon icon--sm' }), onclick: reset,
  });
  const node = el('aside.ed-prop', {},
    el('div.col-hd', {},
      el('h3', { html: icon('sliders', { cls: 'icon icon--sm' }) + '<span>修图参数</span>' }),
      el('span.grow'),
      resetBtn),
    chips, tabs, body, ft,
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
    line(cloudMode ? '已恢复默认指令' : '已恢复工作流默认参数');
  }

  function setBusy(b) { busy = b; paintGo(); }
  function line(txt) {
    fill(runLine, txt ? el('span', { class: /失败|错误|拒收/.test(txt) ? 'err' : '', text: txt }) : null);
  }

  /** 本机 / 云端来回切：云端隐掉采样与 LoRA 页，并换一套阶段名 */
  function setCloud(on) {
    on = !!on;
    if (on === cloudMode) return;
    cloudMode = on;
    if (on && tab !== 'prompt') tab = 'prompt';
    for (const b of tabs.children) b.style.display = on && b.dataset.k !== 'prompt' ? 'none' : '';
    cloudNote.hidden = !on;
    edgeRow.hidden = !on;
    const tip = on ? '重置为默认指令' : '重置为工作流默认';
    resetBtn.setAttribute('aria-label', tip);
    resetBtn.setAttribute('data-tip', tip);
    stages = makeSteps(on ? CLOUD_STAGES : STAGES);
    fill(stagesBox, stages.node);
    show();
  }

  return {
    node, sync, setBusy, line, applySettings, clearMarks, setCloud,
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
    el('span.prow__name', { text: label, 'data-tip': '打开时每次提交都换一个新种子' }), sw);
  return { node, set(v) { on = !!v; sw.setAttribute('aria-checked', String(on)); } };
}

/**
 * 并入 / 移出这句提示词。
 * 短语本身就带中文逗号，所以"移出"必须按整句子串删：原来先按逗号切成列表项再等值比较，
 * 句子被切成两半，一项都匹配不上——点掉之后残段永远留在指令里。
 * 胶囊的 is-on 由 syncPrompt 按文本回灌，这里再以文本为准判一次，重载后也不会重复追加。
 */
function togglePhrase(cur, phrase, wasOn) {
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
