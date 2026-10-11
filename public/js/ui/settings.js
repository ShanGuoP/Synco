// 设置：后端 / 工作流 / 云端 / 图像档位 / 外观 / 导出 / 短语 / 预设 / 数据目录 / 关于，一个弹窗装下
'use strict';
import { el, fill } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api } from '../core/api.js';
import { isDesktop, call, pickFolder, human, openExternal } from '../core/desktop.js';
import { MODES, STYLES, apply, mode, resolved, watch, appearance, applyAppearance } from '../core/theme.js';
import { osReduce, reduce, saved, set as setMotion, watch as watchMotion } from '../core/motion.js';
import { dl, dx, flushForSwitch, getLang, t } from '../core/i18n.js';
import { store } from '../state.js';
import { modal, confirm } from './modal.js';
import { toastOk, toastErr, toastBusy } from './toast.js';
import { createBackendsPane } from './backends.js';
import { createPresetsPane } from './presets.js';
import { createPhrasesPane } from './phrases.js';

/* 后端地址与 ComfyUI 根目录是同一件事的两半——都在回答"我连的是哪个 ComfyUI"，
   分成两个分区会让人在两边各填一半、自检结果却在另一个分区里 */
const SECTIONS = [['backend', 'settings.sec.backend'], ['workflow', 'settings.sec.workflow'], ['cloud', 'settings.sec.cloud'],
  ['image', 'settings.sec.image'], ['theme', 'settings.sec.theme'],
  ['export', 'settings.sec.export'], ['phrases', 'settings.sec.phrases'], ['presets', 'settings.sec.presets'],
  ['data', 'settings.sec.data'], ['about', 'settings.sec.about']];
// 存键名而不是文案：字典是 boot 里异步装的，模块级常量取文案只会拿到 ⟨键名⟩
const VERIFY_TEXT = { ok: 'settings.verify.ok', mismatch: 'settings.verify.mismatch', missing: 'settings.verify.missing', 'no-baseline': 'settings.verify.noBaseline' };
const gb = n => (n >= 1073741824 ? (n / 1073741824).toFixed(1) + ' GB' : n >= 1048576 ? (n / 1048576).toFixed(0) + ' MB' : '—');

const okRow = (ok, label, extra) => el('div.set__row', { class: `set__row${ok === null ? ' is-unknown' : ok ? '' : ' is-bad'}` },
  el('span.dot', { class: `dot ${ok === null ? '' : ok ? 'dot--done' : 'dot--err'}` }),
  el('b', { text: label }),
  extra ? el('span.set__extra', { text: extra }) : null);

/** 把两个分区拼成同一个：后端列表在上，本机目录与自检在下 */
function joinPanes(a, b) {
  return { node: el('div.dlg-flow', {}, a.node, b.node) };
}

function createWorkflowPane() {
  const inp = el('input.input', { type: 'text', spellcheck: 'false', placeholder: 'C:\\...\\ComfyUI\\user\\default\\workflows\\xxx.json' });
  const state = el('div.set__state');

  async function load() {
    try {
      const [cfg, s, r] = await Promise.all([api.cfg(), api.backends(), api.workflowRoles().catch(() => null)]);
      inp.value = cfg.workflow_path || '';
      const rows = [
        okRow(cfg.cfg_source === 'workflow',
          t(cfg.cfg_source === 'workflow' ? 'settings.wf.readFrom' : 'settings.wf.builtinFrom'), dx(cfg.workflow_error)),
      ];
      if (r) rows.push(graphRow(r));
      rows.push(okRow(!!s.active, t('settings.wf.curBackend', { host: String(s.active).replace(/^https?:\/\//, '') })));
      fill(state, ...rows);
    } catch (e) { fill(state, okRow(false, t('settings.wf.readFail'), e.message)); }
  }

  async function save() {
    try {
      const r = await api.setWorkflow(inp.value.trim());
      toastOk(t(r.cfg_source === 'workflow' ? 'settings.wf.linked' : 'settings.wf.savedNoRead'), r.workflow_path || '');
      store.set({ cfg: await api.cfg() });
      load();
    } catch (e) { toastErr(t('settings.wf.saveFail'), e.message); }
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: t('settings.wf.pathTitle') }), inp,
      el('p.muted', { text: t('settings.wf.pathNote') }),
      el('div', { style: { display: 'flex', gap: '8px', marginTop: '10px' } },
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: t('settings.wf.saveVerify'), onclick: save }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', 'data-tip': t('settings.wf.rolesTip'), text: t('settings.wf.roles'), onclick: () => editRoles(load) }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', 'data-tip': t('settings.wf.inspectTip'), text: t('settings.wf.inspect'), onclick: () => inspectWorkflow() }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('settings.wf.reload'), onclick: load }))),
    state);
  load();
  return { node };
}

/** 计算图来源那一行：接管成功是绿点，认不出角色是红点（提交会被明确拒掉），非 API 导出是灰点 */
function graphRow(r) {
  if (r.can_takeover) return okRow(true, t('settings.graph.mine'), t('settings.graph.rolesReady', { n: Object.keys(r.effective || {}).length }));
  if (!r.is_api) return okRow(null, t('settings.graph.builtin'), dx(r.reason, r.reason_args) || '');
  return okRow(false, t('settings.graph.notYet'), dl(r.errors));
}

/**
 * 角色映射编辑器：每一行是一个"Synco 要往哪儿填东西"，下拉里只有同一种类的节点。
 * 默认值取自动认出的那个——只有你改过的才会存进库，节点号被 ComfyUI 重排时还能重新认。
 */
async function editRoles(reload) {
  let d;
  try { d = await api.workflowRoles(); } catch (e) { toastErr(t('settings.roles.readFail'), e.message); return; }
  if (!d.is_api) {
    modal({
      title: t('settings.roles.notGraphTitle'),
      body: el('p.muted', { text: d.reason ? t('settings.roles.notGraphBody', { reason: dx(d.reason, d.reason_args) }) : t('settings.roles.notGraphBodyBare') }),
      actions: [{ label: t('common.gotIt'), kind: 'ghost' }],
    });
    return;
  }
  const auto = d.auto || {}, eff = d.effective || {};
  const selects = [];
  const byClass = cls => (d.nodes || []).filter(n => n.class_type === cls);
  const optText = (r, n) => t(
    n.title
      ? auto[r.key] === n.id ? 'settings.roles.optNodeTitleAuto' : 'settings.roles.optNodeTitle'
      : auto[r.key] === n.id ? 'settings.roles.optNodeAuto' : 'settings.roles.optNode',
    { id: n.id, title: n.title });
  const rows = (d.roles || []).map(r => {
    const list = byClass(r.class);
    const cur = eff[r.key] || '';
    const sel = el('select.select', { style: { flex: '0 0 auto', minWidth: '112px', maxWidth: '48%' } },
      el('option', { value: '', text: t(r.required ? 'settings.roles.autoMissing' : 'settings.roles.skip') }),
      ...list.map(n => el('option', { value: n.id, text: optText(r, n) })));
    sel.value = cur;
    if (!list.length) sel.disabled = true;
    selects.push({ key: r.key, node: sel, auto: auto[r.key] });
    return el('div.set__row', {},
      el('b', { text: dx(r.label) }),
      el('span.set__extra', {
        text: t(r.required ? 'settings.roles.classCountReq' : 'settings.roles.classCount', { cls: r.class, n: list.length }),
      }),
      sel);
  });
  const verdict = el('div', {});
  const paintVerdict = (v) => fill(verdict,
    el('h4.dlg-h4', { text: t('settings.roles.verifyTitle') }),
    ...(v.can_takeover
      ? [okRow(true, t('settings.roles.verdictOk'))]
      : [okRow(false, t('settings.roles.verdictBad'), dl(v.errors))]));
  paintVerdict(d);
  /* 只交与自动认出不同的那些：全量存进库等于把节点号钉死，
     而节点号是 ComfyUI 按画布顺序给的，挪一下就会变 */
  const grab = () => {
    const out = {};
    for (const s of selects) if (s.node.value && s.node.value !== s.auto) out[s.key] = s.node.value;
    return out;
  };
  async function saveRoles(handle, close) {
    try {
      const r = await api.setWorkflowRoles(grab());
      paintVerdict(r);
      if ((r.rejected || []).length) toastErr(t('settings.roles.rejected'), dl(r.rejected));
      else toastOk(t('settings.roles.saved'));
      if (close) { reload?.(); handle?.close(); }
    } catch (e) { toastErr(t('settings.roles.saveFail'), e.message); }
  }
  return modal({
    title: t('settings.wf.roles'), wide: true,
    body: el('div.dlg-flow', {},
      el('p.muted', { text: t('settings.roles.intro') }),
      ...rows, verdict),
    actions: [
      { label: t('settings.roles.saveSee'), kind: 'ghost', run: (h) => { saveRoles(h, false); return false; } },
      { label: t('settings.roles.saveClose'), kind: 'primary', run: (h) => { saveRoles(h, true); return false; } },
    ],
  });
}

/**
 * 把工作流文件里的节点摊开给人看（类名与个数），缺哪个认识的角色一并列出。
 */
async function inspectWorkflow() {
  const busy = toastBusy(t('settings.insp.busy'));
  let d;
  try { d = await api.workflowInspect(); } catch (e) { busy.close(); toastErr(t('settings.insp.fail'), e.message); return; }
  busy.close();
  const counts = new Map();
  for (const n of d.nodes || []) counts.set(n.class_type, (counts.get(n.class_type) || 0) + 1);
  const row = (k, v) => el('div.set__row', {}, el('b', { text: k }), el('span.set__extra', { text: v }));
  const body = el('div.dlg-flow', {},
    el('p.muted', {
      text: `${t(d.format === 'api' ? 'settings.insp.apiFmt' : 'settings.insp.uiFmt')} · ${t('settings.insp.nodeTotal', { n: d.total })}`,
    }),
    d.stitch_pair_ok ? null : el('div.set__row.is-bad', {}, el('span.dot', { class: 'dot dot--err' }),
      el('b', { text: t('settings.insp.noPair') }),
      el('span.set__extra', { text: t('settings.insp.noPairWhy') })),
    el('h4.dlg-h4', { text: t('settings.insp.listTitle') }),
    ...[...counts.entries()].sort((a, b) => a[0].localeCompare(b[0])).map(([k, n]) => row(k, t('settings.insp.count', { n }))),
    (d.known_missing || []).length
      ? el('div', {}, el('h4.dlg-h4', { text: t('settings.insp.missingTitle') }), el('p.muted', { text: d.known_missing.join(t('settings.sepList')) }))
      : null);
  return modal({ title: t('settings.insp.title'), wide: true, body, actions: [{ label: t('common.close'), kind: 'ghost' }] });
}

function createCloudPane() {
  const inp = (ph, type = 'text', extra = {}) => el('input.input', { type, spellcheck: 'false', placeholder: ph, ...extra });
  const baseInp = inp(t('settings.cloud.basePh'));
  const modelInp = inp(t('settings.cloud.modelPh'));
  const sizeInp = inp(t('settings.cloud.sizePh'));
  const qualInp = inp(t('settings.cloud.qualPh'));
  const keyInp = inp('API key', 'password', { autocomplete: 'new-password' });
  const timeoutInp = inp('', 'number', { min: '30000', max: '600000', step: '1000' });
  const concInp = inp('', 'number', { min: '1', max: '6', step: '1' });
  const expandInp = inp('', 'number', { min: '16', max: '200', step: '8' });
  const featherInp = inp('', 'number', { min: '8', max: '200', step: '8' });
  const edgeInp = inp('', 'number', { min: '512', max: '2048', step: '64' });
  const cloudCk = el('input', { type: 'checkbox', onchange: () => save({ kind: cloudCk.checked ? 'cloud' : 'comfyui' }) });
  const state = el('div.set__state');
  const fld = (label, node) => el('div', { style: { display: 'grid', gap: '4px' } }, el('span.muted', { text: label }), node);
  const pair = (a, b) => el('div', { style: { display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '10px' } }, a, b);
  let data = null, last = null;

  function paintState() {
    fill(state,
      okRow(cloudCk.checked, t(cloudCk.checked ? 'settings.cloud.modeCloud' : 'settings.cloud.modeLocal'),
        cloudCk.checked ? t('settings.cloud.modeCloudWhy') : ''),
      okRow(!!(data?.base && data?.model && data?.key_saved), t('settings.cloud.threeReady'),
        data?.key_saved ? t('settings.cloud.keyTail', { tail: data.key_tail }) : t('settings.cloud.keyNone')),
      last ? okRow(!!last.ok, t('settings.cloud.probe'),
        last.ok
          ? last.models != null ? t('settings.cloud.probeModels', { ms: last.ms, n: last.models }) : t('settings.cloud.probeRtt', { ms: last.ms })
          : dx(last.error, last.error_args)) : null);
  }

  /* 键名 → 输入框 → 服务端字段。局部保存后只回填这次真改过的那几个，
     否则勾一下"改用云端"就会把正在输入的 base_url / 模型名清回服务端的旧值 */
  const FIELDS = [
    ['base', baseInp, d => d.base],
    ['model', modelInp, d => d.model],
    ['size', sizeInp, d => d.size],
    ['quality', qualInp, d => d.quality],
    ['timeout', timeoutInp, d => d.timeout_ms],
    ['concurrency', concInp, d => d.concurrency],
    ['stitch_expand', expandInp, d => d.stitch_expand],
    ['stitch_feather', featherInp, d => d.stitch_feather],
    ['stitch_edge', edgeInp, d => d.stitch_edge],
  ];

  function absorb(d, only) {
    data = d;
    cloudCk.checked = d.kind === 'cloud';
    for (const [k, node, pick] of FIELDS) {
      if (only && !only.includes(k)) continue;
      const v = pick(d);
      node.value = v === null || v === undefined ? '' : String(v);
    }
    keyInp.value = '';
    keyInp.placeholder = d.key_saved ? t('settings.cloud.keySavedPh', { tail: d.key_tail }) : t('settings.cloud.keySetPh');
    paintState();
  }

  async function save(patch) {
    try {
      const r = await api.saveCloud(patch);
      absorb(r, Object.keys(patch));
      store.set({ cloud: r }, 'cloud');
      toastOk(t('settings.cloud.saved'), t(r.kind === 'cloud' ? 'settings.cloud.runCloud' : 'settings.cloud.runLocal'));
    } catch (e) { toastErr(t('settings.cloud.saveFail'), e.message); }
  }

  const saveAll = () => save({
    base: baseInp.value, model: modelInp.value, size: sizeInp.value, quality: qualInp.value,
    key: keyInp.value, timeout: timeoutInp.value, concurrency: concInp.value,
    stitch_expand: expandInp.value, stitch_feather: featherInp.value, stitch_edge: edgeInp.value,
  });

  async function test() {
    const busy = toastBusy(t('settings.cloud.testBusy'));
    try { last = await api.testCloud(); busy.close(); paintState(); }
    catch (e) { busy.close(); last = { ok: false, error: e.message }; paintState(); }
    last.ok
      ? toastOk(t('settings.cloud.testOk'), t('settings.cloud.probeRtt', { ms: last.ms }))
      : toastErr(t('settings.cloud.testDead'), dx(last.error, last.error_args).slice(0, 120));
  }

  async function load() {
    try { absorb(await api.cloud()); } catch (e) { fill(state, okRow(false, t('settings.cloud.readFail'), e.message)); }
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: t('settings.cloud.howTitle') }),
      el('label.set__ck', {}, cloudCk, el('span', { text: t('settings.cloud.ckLabel') })),
      el('p.muted', { text: t('settings.cloud.ckNote') })),
    el('div', { style: { display: 'grid', gap: '10px' } },
      fld(t('settings.cloud.baseLabel'), baseInp),
      pair(fld(t('settings.cloud.modelLabel'), modelInp), fld(t('settings.cloud.sizeLabel'), sizeInp)),
      pair(fld(t('settings.cloud.qualLabel'), qualInp), fld('API key', keyInp)),
      pair(fld(t('settings.cloud.timeoutLabel'), timeoutInp), fld(t('settings.cloud.concLabel'), concInp)),
      el('h4.dlg-h4', { text: t('settings.cloud.stitchTitle') }),
      el('p.muted', { style: { marginBottom: '6px' }, text: t('settings.cloud.stitchNote') }),
      el('div', { style: { display: 'grid', gridTemplateColumns: '1fr 1fr 1fr', gap: '10px' } },
        fld(t('settings.cloud.expandLabel'), expandInp),
        fld(t('settings.cloud.featherLabel'), featherInp),
        fld(t('settings.cloud.edgeLabel'), edgeInp)),
      el('div', { style: { display: 'flex', gap: '8px', flexWrap: 'wrap' } },
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: t('common.save'), onclick: saveAll }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('settings.cloud.testBtn'), onclick: test }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', 'data-tip': t('settings.cloud.clearKeyTip'), text: t('settings.cloud.clearKey'), onclick: () => save({ clear_key: true }) }))),
    state,
    el('p.muted', { style: { marginTop: '10px' }, text: t('settings.cloud.keyNote') }));
  load();
  return { node };
}

function createSetupPane() {
  // 占位符不能是任何人的真实机器路径：那会让人以为软件"默认要装在 D 盘"
  const rootInp = el('input.input', { type: 'text', spellcheck: 'false', placeholder: t('settings.setup.rootPh') });
  const proxyInp = el('input.input', { type: 'text', spellcheck: 'false', placeholder: t('settings.setup.proxyPh') });
  const body = el('div.dlg-flow');
  const progBox = el('div');
  const verifyBox = el('div.set__verify');
  let data = null, timer = 0;

  async function load(silent) {
    try {
      data = await api.setup();
      if (!rootInp.value) rootInp.value = data.root || '';
      paint();
      if (!silent) {
        toastOk(t('settings.setup.done'), t('settings.setup.packsLine', {
          ok: data.detect.packs.filter(p => p.installed).length,
          total: data.detect.packs.length,
          size: gb(data.detect.missing_bytes),
        }));
      }
    } catch (e) { if (!silent) toastErr(t('settings.setup.fail'), e.message); }
  }

  function paintProgress() {
    const p = data?.progress;
    if (!p) return null;
    const pct = p.total ? Math.round((p.done / p.total) * 100) : 0;
    return el('div.set__prog', {},
      el('span', {
        text: t('settings.setup.progLine', {
          status: t(p.status === 'error' ? 'settings.setup.stError' : p.status === 'done' ? 'settings.setup.stDone' : 'settings.setup.stRun'),
          message: p.message || '', done: p.done || 0, total: p.total || 0,
        }),
      }),
      el('div.set__track', {}, el('i', { style: { width: pct + '%' } })));
  }

  function paint() {
    const d = data?.detect;
    if (!d) { fill(body, el('p.muted', { text: t('settings.setup.needRoot') })); return; }
    const r = d.runtime;
    const root = d.root || '';
    /* 原来这里印的是字面量 "<root>\ComfyUI"——占位符从来没被替换过；
       而且根目录还没填的时候报"没找到"是误导，那时候选状态是"未知" */
    const env = (label, ok, rel, why) => okRow(root ? ok : null, label,
      root ? (ok ? '' : t('settings.setup.envMissing', { root, rel })) : why || t('settings.setup.envTip'));
    const rows = [
      el('div', {},
        el('h4.dlg-h4', { text: t('settings.setup.envTitle') }),
        env(t('settings.setup.envComfy'), r.comfyui && r.main_py, 'ComfyUI'),
        env(t('settings.setup.envPython'), r.python, 'python_embeded'),
        okRow(root ? r.qwen_nodes : null, t('settings.setup.envQwen'),
          root ? (r.qwen_nodes ? '' : t('settings.setup.envQwenWhy')) : t('settings.setup.noRoot'))),
      el('div', {},
        el('h4.dlg-h4', { text: t('settings.setup.packsTitle') }),
        ...d.packs.map(p => okRow(p.installed, p.dir,
          p.installed ? dx(p.label) : t('settings.setup.packsMissing', { label: dx(p.label), nodes: p.nodes.join(' / ') })))),
      el('div', {},
        el('h4.dlg-h4', { text: t('settings.setup.weightsTitle', { total: gb(d.total_bytes), missing: gb(d.missing_bytes) }) }),
        ...d.models.map(m => okRow(m.found ? true : m.optional ? null : false, dx(m.label),
          m.found
            ? t(m.sha256 ? 'settings.setup.wFound' : 'settings.setup.wFoundNoBase', { size: gb(m.bytes) })
            : m.optional ? t('settings.setup.wOptional') : t('settings.setup.wMissing', { dir: m.dir, rel: m.rel }))),
        el('div', { style: { display: 'flex', gap: '8px', alignItems: 'center', marginTop: '8px' } },
          el('button.btn.btn--ghost.btn--sm', {
            type: 'button', 'data-tip': t('settings.setup.deepTip'),
            html: icon('eye', { cls: 'icon icon--sm' }) + `<span>${t('settings.setup.deepBtn')}</span>`, onclick: deepVerify,
          }),
          verifyBox)),
      el('div', {},
        el('h4.dlg-h4', { text: t('settings.setup.scriptTitle') }),
        el('p.muted', { text: t('settings.setup.scriptNote') }),
        el('div', { style: { display: 'flex', gap: '8px', marginTop: '8px', flexWrap: 'wrap' } },
          el('button.btn.btn--primary.btn--sm', { type: 'button', html: icon('download', { cls: 'icon icon--sm' }) + `<span>${t('settings.setup.genBtn')}</span>`, onclick: gen }),
          el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('settings.setup.recheck'), onclick: () => load() })),
        proxyInp),
      progBox];
    /* 输入框里的路径和自检用的那份不一致时，下面所有结论都还是旧目录的——
       不写这一行，人就会对着"没找到"怀疑自己装错了地方 */
    if (root && rootInp.value && rootInp.value !== root) rows.unshift(
      el('div.set__row.is-unknown', {}, el('span.dot', {}), el('b', { text: t('settings.setup.changed') }),
        el('span.set__extra', { text: t('settings.setup.changedWhy') })));
    fill(body, ...rows);
  }

  async function deepVerify() {
    fill(verifyBox, el('span.muted', { text: t('settings.setup.computing') }));
    try {
      const r = await api.verifySetup(rootInp.value.trim());
      const bad = r.rows.filter(x => x.status === 'mismatch' || x.status === 'missing');
      /* 三态而不是"非黑即白"：没有基准可比既不是通过也不是损坏，画成红叉会让人以为文件坏了 */
      const state = s => (s === 'ok' ? true : s === 'no-baseline' ? null : false);
      const verdict = s => (VERIFY_TEXT[s] ? t(VERIFY_TEXT[s]) : s);
      fill(verifyBox, ...r.rows.map(x => okRow(state(x.status), `${dx(x.label)} ${verdict(x.status)}`)));
      if (bad.length) toastErr(t('settings.setup.mismatch'), t('settings.setup.mismatchBody', { names: bad.map(x => dx(x.label)).join(t('settings.sepList')) }));
      else toastOk(t('settings.setup.verifyOk'), t('settings.setup.verifyOkBody', { n: r.rows.filter(x => x.status === 'ok').length }));
    } catch (e) { fill(verifyBox, el('span.muted', { text: t('settings.setup.verifyFail', { msg: e.message }) })); }
  }

  /** 脚本在自己的进程里跑，这里只读它回写的进度，别反复做全量体检 */
  async function poll() {
    try {
      const r = await api.setupProgress();
      data = { ...(data || {}), progress: r.progress };
      fill(progBox, paintProgress());
    } catch { /* 面板刚关掉时可能已经取不到 */ }
  }

  async function gen() {
    try {
      const r = await api.genSetup({ path: rootInp.value.trim(), proxy: proxyInp.value.trim() });
      toastOk(t('settings.setup.genDone'), r.bat);
    } catch (e) { toastErr(t('settings.setup.genFail'), e.message); }
  }

  async function saveRoot() {
    try { await api.setSetupRoot(rootInp.value.trim()); await load(); }
    catch (e) { toastErr(t('settings.setup.saveFail'), e.message); }
  }

  /* 选目录而不是让人手打：粘贴进来的路径常带引号或尾斜杠，那种值判存在性会为假，
     症状就是明明装对了却报"没找到 ComfyUI 主程序" */
  async function browseRoot() {
    let p;
    try { p = await pickFolder(t('settings.setup.pickRoot')); }
    catch (e) { toastErr(t('settings.setup.noDialog'), e.message); return; }
    if (!p) return;
    rootInp.value = p;
    await saveRoot();
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: t('settings.setup.rootTitle') }),
      el('div.set__add', {}, rootInp,
        isDesktop() ? el('button.btn.btn--ghost.btn--sm', {
          type: 'button', 'data-tip': t('settings.setup.browseTip'),
          text: t('settings.setup.browse'), onclick: browseRoot,
        }) : null,
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: t('settings.setup.saveCheck'), onclick: saveRoot })),
      el('p.muted', { text: t('settings.setup.rootNote') })),
    body);
  load(true);
  /* 面板是缓存下来反复挂卸的：一见 node 离开 DOM 就 clearInterval，切走再回来进度条就永久冻在那一刻。
     改成"不在 DOM 里就跳过这一轮"，空转的成本是一次布尔判断，不是网络请求。 */
  timer = setInterval(() => { if (document.body.contains(node)) poll(); }, 2500);
  return { node };
}

/** 导出目录：成图直接复制到本机文件夹，不经过浏览器下载 */
function createExportPane() {
  const inp = el('input.input', { type: 'text', spellcheck: 'false', placeholder: t('settings.export.dirPh') });
  const state = el('div.set__state');

  async function load() {
    try {
      const d = await api.exportGet();
      inp.value = d.dir || '';
      fill(state, okRow(!!d.ready, t(d.ready ? 'settings.export.ready' : d.dir ? 'settings.export.gone' : 'settings.export.unset')));
    } catch (e) { fill(state, okRow(false, t('settings.export.readFail'), e.message)); }
  }

  async function save() {
    try {
      const r = await api.exportSetDir(inp.value.trim());
      inp.value = r.dir;
      toastOk(t('settings.export.ok'), r.dir);
      load();
    } catch (e) { toastErr(t('settings.export.bad'), e.message); }
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: t('settings.export.title') }),
      el('div.set__add', {}, inp,
        // 桌面版给一个系统目录框：手打导出目录这种路径太容易打错
        isDesktop() ? el('button.btn.btn--ghost.btn--sm', {
          type: 'button', text: t('settings.export.browse'), onclick: async () => {
            let p;
            try { p = await pickFolder(t('settings.export.pickTitle')); } catch (e) { toastErr(t('settings.export.noDialog'), e.message); return; }
            if (p) { inp.value = p; save(); }
          },
        }) : null,
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: t('settings.export.saveCheck'), onclick: save })),
      el('p.muted', { text: t('settings.export.note') })),
    state);
  load();
  return { node };
}

/** 图像档位：编辑/查看用的 proxy 长边，加上服务端队列的实时快照 */
function createImagePane() {
  const edgeInp = el('input.input', { type: 'number', min: '1024', max: '8192', step: '256' });
  const capInp = el('input.input', { type: 'number', min: '0', max: '65536', step: '512' });
  const cacheLine = el('span.muted');
  const state = el('div.set__state');

  async function loadCache() {
    try {
      const c = await api.get('/api/cache');
      capInp.value = String(c.cap_mb ?? 4096);
      cacheLine.textContent = c.bytes ? t('settings.image.cacheNow', { mb: (c.bytes / 1048576).toFixed(1) }) : t('settings.image.cacheEmpty');
    } catch (e) { cacheLine.textContent = `${t('settings.image.cacheFail')}：${e.message}`; }
  }

  async function load() {
    try {
      const [s, q] = await Promise.all([api.get('/api/settings'), api.queueState()]);
      edgeInp.value = String(s.proxy_edge ?? 3072);
      fill(state,
        okRow(true, t('settings.image.curLevel'), t('settings.image.curLevelNote', { edge: s.proxy_edge })),
        okRow(q.queued === 0, t('settings.image.queue'), t('settings.image.queueNote', { queued: q.queued, running: q.running, cap: q.concurrency })),
        okRow(null, t('settings.image.afterChange'), t('settings.image.afterChangeNote')));
    } catch (e) { fill(state, okRow(false, t('settings.image.readFail'), e.message)); }
    loadCache();
  }

  async function save() {
    const busy = toastBusy(t('settings.image.busy'));
    try { const r = await api.setProxyEdge(+edgeInp.value || 3072); busy.close(); toastOk(t('settings.image.saved'), t('settings.image.savedNote', { edge: r.proxy_edge })); load(); }
    catch (e) { busy.close(); toastErr(t('settings.image.saveFail'), e.message); }
  }

  /* 上限改完服务端立刻按新上限扫一次，所以这里直接回显省了多少 */
  async function saveCap() {
    const busy = toastBusy(t('settings.image.busy'));
    try {
      const r = await api.post('/api/cache/cap', { mb: +capInp.value || 0 });
      busy.close();
      const mb = (r.bytes || 0) / 1048576;
      toastOk(t('settings.image.capSaved'), mb > 0 ? t('settings.image.cacheSaved', { mb: mb.toFixed(1) }) : '');
      loadCache();
    } catch (e) { busy.close(); toastErr(t('settings.image.saveFail'), e.message); }
  }

  async function clearCache() {
    if (!await confirm({ title: t('settings.image.cacheClearTitle'), text: t('settings.image.cacheClearText'), okLabel: t('settings.image.cacheClearBtn') })) return;
    const busy = toastBusy(t('settings.image.busy'));
    try {
      const r = await api.post('/api/cache/clear', {});
      busy.close();
      toastOk(t('settings.image.cacheCleared'), t('settings.image.cacheSaved', { mb: ((r.bytes || 0) / 1048576).toFixed(1) }));
      loadCache();
    } catch (e) { busy.close(); toastErr(t('settings.image.saveFail'), e.message); }
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: t('settings.image.title') }), edgeInp,
      el('p.muted', { text: t('settings.image.note') }),
      el('div', { style: { display: 'flex', gap: '8px', marginTop: '10px' } },
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: t('common.save'), onclick: save }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('settings.image.reload'), onclick: load }))),
    el('div', {}, el('h4.dlg-h4', { text: t('settings.image.cache') }),
      el('p.muted', { text: t('settings.image.cacheNote') }),
      el('div', { style: { display: 'flex', gap: '8px', alignItems: 'center', marginTop: '8px' } }, capInp, cacheLine),
      el('div', { style: { display: 'flex', gap: '8px', marginTop: '10px' } },
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('common.save'), onclick: saveCap }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('settings.image.cacheClearBtn'), onclick: clearCache }))),
    state);
  load();
  return { node };
}

/** 数据目录：桌面版可选址、可把整坨资料搬过去；浏览器版只读 */
function createDataPane() {
  const state = el('div.set__state');
  const acts = el('div', { style: { display: 'flex', gap: '8px', marginTop: '10px' } });
  let vinfo = null;   // /api/version 说的：这次跑的到底是哪一套
  let info = null;    // 桌面壳说的：目录里有什么、从哪认出来的
  let ipcErr = '';    // 壳那一侧读不到时，把真实原因留着显示，别只报"读不到"

  const btn = (text, kind, onclick) => el('button.btn', {
    type: 'button', class: `btn btn--${kind || 'ghost'} btn--sm`, text, onclick,
  });

  async function choose() {
    let to;
    try { to = await pickFolder(t('settings.data.pickNew')); } catch (e) { toastErr(t('settings.data.noDialog'), e.message); return; }
    if (!to) return;
    try {
      info = (await call('set_data_dir', { path: to })).dir;
      paint();
      toastOk(t('settings.data.moved'), t('settings.data.movedNote'));
    } catch (e) { toastErr(t('settings.data.moveFail'), String(e.message || e)); }
  }

  async function copy() {
    let to;
    try { to = await pickFolder(t('settings.data.pickCopy')); } catch (e) { toastErr(t('settings.data.noDialog'), e.message); return; }
    if (!to) return;
    const busy = toastBusy(t('settings.data.copyBusy'));
    try {
      const r = await call('copy_data_to', { to });
      busy.close();
      info = r.dir;
      paint();
      toastOk(t('settings.data.copied', { n: r.copied.files, bytes: human(r.copied.bytes) }), t('settings.data.copiedNote'));
    } catch (e) { busy.close(); toastErr(t('settings.data.copyFail'), String(e.message || e)); }
  }

  async function restart() {
    try { await call('restart_app'); } catch (e) { toastErr(t('settings.data.restartFail'), e.message); }
  }

  function paint() {
    const path = info?.path || vinfo?.data_dir || '';
    fill(state,
      okRow(!!path, t('settings.data.curDir'),
        path ? (info ? t('settings.data.withSource', { path, source: dx(info.source) }) : path) : t('settings.data.unreadable')),
      info ? okRow(info.has_db, t('settings.data.contents'),
        t('settings.data.contentsNote', { projects: info.projects, images: info.images, bytes: human(info.bytes) })) : null,
      okRow(vinfo?.desktop, t('settings.data.form'),
        vinfo?.desktop ? t('settings.data.formDesktop') : t('settings.data.formWeb', { dir: vinfo?.public_dir || '' })),
      info ? okRow(null, t('settings.data.backup'), t('settings.data.backupNote')) : null,
      ipcErr ? okRow(false, t('settings.data.shellDead'), t('settings.data.shellDeadNote', { why: ipcErr })) : null,
      isDesktop() ? null : okRow(null, t('settings.data.needDesktop'), t('settings.data.needDesktopNote')));
    fill(acts,
      info?.restart_required ? btn(t('settings.data.restartBtn'), 'primary', restart) : null,
      isDesktop() ? btn(t('settings.data.chooseBtn'), 'ghost', choose) : null,
      isDesktop() && info && !info.restart_required ? btn(t('settings.data.copyBtn'), 'ghost', copy) : null);
  }

  async function load() {
    // Tauri 的 IPC 失败 reject 出来的是字符串，读 e.message 只会拿到 undefined，
    // 真实原因（权限、命令没注册、状态没托管）就这么被"读不到数据目录"盖掉了
    const why = e => String(e?.message || e || t('settings.data.unknown'));
    try {
      vinfo = await api.get('/api/version');
    } catch (e) {
      fill(state, okRow(false, t('settings.data.readFail'), why(e)));
      return;
    }
    try {
      if (isDesktop()) info = await call('data_dir_info');
    } catch (e) {
      ipcErr = why(e);        // 服务端自己就知道库在哪，壳读不到不该把整页清空
    }
    paint();
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: t('settings.data.title') }),
      el('p.muted', { text: t('settings.data.note') }),
      acts),
    state);
  load();
  return { node };
}

/** 外观：界面风格 + 三档底色 + 减弱动效 + 当前实际生效的那一档 */
function createThemePane() {
  const styleRow = el('div.appearance-options');
  const row = el('div', { style: { display: 'flex', gap: '8px', flexWrap: 'wrap' } });
  const now = el('div.set__state');
  /* 减弱动效（F7 的兜底开关）：和系统的 prefers-reduced-motion 同一个语义，任一到就全站瞬时——
     视口惯性、瓦片淡入、翻页的视图过渡、主题 crossfade 全关掉，悬停那些也给压成 .01ms。
     和主题一个道理存 localStorage 而不是库里：它是这台机器的显示偏好，换台机器不该被带走，
     而且首帧之前就要读到（CSP 之下只有 core/theme-boot.js 赶得上，它抢在 body 存在之前挂 <html>）。 */
  const sw = el('button.toggle', {
    type: 'button', role: 'switch', 'aria-checked': String(saved()), 'aria-label': t('settings.theme.reduceAria'),
    onclick: () => { setMotion(!saved()); paint(); },
  });

  /* 哪一路在起作用要说清楚：应用开关关掉不等于系统那一档也关了（那就还是减弱的） */
  const motionLine = () => {
    if (!reduce()) return t('settings.theme.motionAll');
    if (saved() && osReduce()) return t('settings.theme.motionBoth');
    if (saved()) return t('settings.theme.motionSelf');
    return t('settings.theme.motionOs');
  };

  function paint() {
    const cur = mode();
    fill(styleRow, ...STYLES.map(([k, key]) => el('button.appearance-option', {
      type: 'button', 'aria-pressed': String(k === appearance()),
      onclick: () => { applyAppearance(k); paint(); },
    }, el('span.appearance-option__preview', { dataset: { style: k }, 'aria-hidden': 'true' },
      el('span'), el('span'), el('span')),
    el('b', { text: t(key) }),
    el('span.muted', { text: t(`settings.theme.${k}Tip`) }))));
    /* MODES 里存的是键名：字典在 boot 之后才装载，模块级常量取文案只会拿到 ⟨键名⟩ */
    fill(row, ...MODES.map(([k, key]) => el('button.btn.btn--sm', {
      type: 'button', class: `btn btn--sm${k === cur ? ' btn--primary' : ' btn--ghost'}`,
      'aria-pressed': String(k === cur),
      text: t(key), onclick: () => { apply(k); paint(); },
    })));
    sw.setAttribute('aria-checked', String(saved()));
    const name = t(resolved() === 'dark' ? 'theme.ink' : 'theme.paper');
    fill(now,
      el('p.muted', {
        text: cur === 'system' ? t('settings.theme.effectiveSys', { mode: name }) : t('settings.theme.effectiveFixed', { mode: name }),
      }),
      el('p.muted', { text: motionLine() }));
  }

  /* 界面语言存 app_settings 而不是 localStorage：它是这个工作台的偏好，换机器、重装都该带着走。
     切换走整体重载——视图是启动时一次建好的，逐个重建的代价比整体重载高得多；
     重载之前把在飞的笔迹与参数冲掉（beforeSwitch 那条注册链），不然用户刚涂的就跟着页面一起没了。 */
  const langRow = el('div', { style: { display: 'flex', gap: '8px', flexWrap: 'wrap' } });
  const langLine = el('p.muted', { text: t('settings.appearance.langTip') });
  function paintLang() {
    const cur = getLang();
    fill(langRow, ...[['zh', 'settings.appearance.langZh'], ['en', 'settings.appearance.langEn']].map(([k, key]) => el('button.btn.btn--sm', {
      type: 'button', class: `btn btn--sm${k === cur ? ' btn--primary' : ' btn--ghost'}`,
      text: t(key), onclick: () => chooseLang(k),
    })));
  }
  async function chooseLang(k) {
    if (k === getLang()) return;
    langLine.textContent = t('settings.appearance.langBusy');
    try {
      await api.setLang(k);
      await flushForSwitch();
      location.reload();
    } catch (e) {
      toastErr(t('settings.theme.langFail'), e.message || String(e));
      langLine.textContent = t('settings.appearance.langTip');
      paintLang();
    }
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: t('settings.theme.styleTitle') }), styleRow,
      el('p.muted', { text: t('settings.theme.styleNote') })),
    el('div', {}, el('h4.dlg-h4', { text: t('settings.theme.title') }), row,
      el('p.muted', { text: t('settings.theme.note') })),
    el('div', {}, el('h4.dlg-h4', { text: t('settings.appearance.langName') }), langRow, langLine),
    el('div', {}, el('h4.dlg-h4', { text: t('settings.theme.motionTitle') }),
      el('div.set__ck', {}, sw, el('span', { text: t('settings.theme.reduceLabel') })),
      now,
      el('p.muted', { text: t('settings.theme.motionNote') })));
  paint();
  paintLang();
  watch(() => paint());
  watchMotion(() => paint());
  return { node };
}

/** 关于：版本号 + 这次构建的 commit + 最近提交当日志；没有发布渠道就不装「自动更新」 */
/* 作者页是写死的：这一栏不读库、不读配置，装了安装包也照样在。署名是人名，两种语言都照写 */
const AUTHOR = { name: '@杉果派', url: 'https://github.com/ShanGuoP' }; // i18n-keep 人名不进字典
/* 仓库地址：更新日志那一栏由服务端回，读不到时退到这个常量（致谢里的 NOTICE 链接也用它） */
const REPO = 'https://github.com/ShanGuoP/Synco';

/* 开源致谢：只列真的进了这个二进制的东西，许可口径见仓库根的 NOTICE.md。
   评估过但没采用的（photon-rs、YuNet/FaceMesh 权重）不写在这里——
   写没在包里的署名既没义务也没意义，还会让人以为界面里有人脸功能。
   第一列存键名：这一格是模块级常量，装载时字典还没到。 */
const CREDITS = [
  ['settings.about.cCrates', 'MIT / Apache-2.0'],
  ['settings.about.cTauri', 'MIT / Apache-2.0'],
  ['settings.about.cMls', 'MPL-2.0', 'https://github.com/mpizenberg/rust_mls'],
  ['settings.about.cFont', 'SIL OFL 1.1', 'https://fonts.google.com/noto/specimen/Noto+Serif+SC'],
];

/**
 * 关于：版本、GitHub Releases 的更新日志、仓库与作者。
 *
 * 这一页以前读的是本机 `.git` 的提交历史，装了安装包的人看到的只有"这里就是空的"，
 * 外加三段解释为什么读不到。更新日志改成从仓库的 Releases 拉（`/api/releases`），
 * 那几段自述一律去掉——下载用户要的是"这一版改了什么、去哪下、谁做的"。
 */
function createAboutPane() {
  const state = el('div.set__state');
  const log = el('div.set__state');

  /** 真 `<a>` 而不是"看起来能点的文字"：右键复制链接、键盘 Tab 都还在，只是点击改走系统浏览器 */
  const link = (label, url) => el('a', {
    href: url, rel: 'noopener noreferrer', 'data-tip': url,
    onclick: async e => { e.preventDefault(); if (!(await openExternal(url))) toastErr(t('settings.about.noBrowser'), url); },
    text: label,
  });
  const linkRow = (label, ...kids) => el('div.set__row', {}, el('span.dot'), el('b', { text: label }), ...kids);

  async function load() {
    let v, r;
    try { v = await api.get('/api/version'); } catch (e) { fill(state, okRow(false, t('settings.about.readFail'), e.message)); return; }
    try { r = await api.get('/api/releases'); } catch (e) { r = { releases: [], error: e.message }; }

    const repo = r.repo || REPO;
    const built = v.commit && v.commit !== 'unknown' ? t('settings.about.built', { commit: v.commit, date: String(v.date).slice(0, 10) }) : '';
    const latest = (r.latest || {}).tag ? String(r.latest.tag).replace(/^v/, '') : '';
    const newer = !!latest && latest !== v.version;
    const rows = [
      // 中文名「新刻」的权威出处在 app.title，界面别处只跟着它走
      okRow(true, t('app.title'), t('settings.about.tagline')),
      okRow(true, t('settings.about.version', { v: v.version }), built),
      // 只在真的不是最新时多说一句；一致的时候不写"已是最新版"这种废话
      newer ? okRow(null, t('settings.about.newer', { tag: latest }), '') : null,
      linkRow(t('settings.about.repo'), link(repo.replace(/^https:\/\//, ''), repo)),
      linkRow(t('settings.about.author'), link(AUTHOR.name, AUTHOR.url)),
    ];
    if (newer && (r.latest || {}).download) rows.push(linkRow(t('settings.about.download'), link(t('settings.about.asset', { v: latest }), r.latest.download)));
    fill(state, ...rows);

    const cs = r.releases || [];
    fill(log,
      el('h4.dlg-h4', { text: t('settings.about.logTitle') }),
      // 三种状态分开说：拉失败、拉到了但仓库没发布过、拉到了有内容。
      // 把"还没有 Release"报成"网络不通"会让人白查半天
      r.error
        ? [okRow(false, t('settings.about.logFail'), dx(r.error, r.error_args))]
        : cs.length
          ? cs.map(c => el('div.set__row', {},
              el('span.muted', { text: `${c.date} · ${c.tag}` }),
              link(c.name && c.name !== c.tag ? c.name : t('settings.about.releaseName'), c.url),
              c.download ? link(t('settings.about.download'), c.download) : null))
          : [okRow(null, t('settings.about.noRelease'), t('settings.about.noReleaseNote'))]);
  }

  const node = el('div.dlg-flow', {},
    el('div', { style: { display: 'flex', justifyContent: 'flex-end' } },
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('settings.about.reload'), onclick: load })),
    state, log,
    el('h4.dlg-h4', { text: t('settings.about.creditsTitle') }),
    ...CREDITS.map(([key, lic, url]) => el('div.set__row', {},
      el('span.dot'), el('span', { text: t(key) }), el('span.muted', { text: lic }),
      url ? link(t('settings.about.source'), url) : null)),
    linkRow(t('settings.about.fullList'), link('NOTICE.md', `${REPO}/blob/main/NOTICE.md`)));
  load();
  return { node };
}

export function settingsModal(section = 'backend') {
  const previousFocus = document.activeElement;
  let cur = section;
  const pane = el('div.set__pane');
  const built = {};
  const nav = el('div.set__nav', {}, ...SECTIONS.map(([k, key]) => el('button.set__nav__it', {
    type: 'button', class: `set__nav__it${k === cur ? ' is-on' : ''}`, dataset: { k }, text: t(key),
    onclick: () => {
      cur = k;
      for (const b of nav.children) b.classList.toggle('is-on', b.dataset.k === k);
      built[k] = built[k] || (k === 'backend' ? joinPanes(createBackendsPane(), createSetupPane())
        : k === 'workflow' ? createWorkflowPane()
        : k === 'cloud' ? createCloudPane()
        : k === 'image' ? createImagePane()
        : k === 'theme' ? createThemePane()
        : k === 'export' ? createExportPane()
        : k === 'data' ? createDataPane()
        : k === 'about' ? createAboutPane()
        : k === 'phrases' ? createPhrasesPane()
        : createPresetsPane({ projectId: store.peek('project')?.id || null }));
      fill(pane, el('h2.settings-workspace__title', { text: t(key) }), built[k].node);
    },
  })));
  nav.querySelector('.is-on').click();
  // 新 UI（玻璃）里设置是"工作区内的一层页面"：只能靠右上角 ❌ 退出——
  // 遮罩点击、Esc 与底部"关闭"按钮都不产生关闭（用户规范）；杂志外观维持原默契。
  const glass = document.documentElement.dataset.appearance === 'glass';
  const m = modal({
    title: t('shell.settings'), wide: true, lockClose: glass,
    body: el('div.set', {}, nav, pane),
    actions: glass ? [] : [{ label: t('common.close'), kind: 'ghost' }],
    onClose: () => { if (previousFocus?.isConnected) previousFocus.focus(); },
  });
  m.node.classList.add('settings-workspace');
  // 工作区继续是同一对话框会话；Tab 不漏进被遮住的画布，Esc/关闭仍走原工厂。
  m.node.addEventListener('keydown', e => {
    if (e.key !== 'Tab') return;
    const nodes = [...m.node.querySelectorAll('button, input, select, textarea, a[href], [tabindex="0"]')]
      .filter(n => !n.disabled && n.getClientRects().length);
    if (!nodes.length) return;
    const first = nodes[0], last = nodes[nodes.length - 1];
    if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
    else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
  });
  return m;
}
