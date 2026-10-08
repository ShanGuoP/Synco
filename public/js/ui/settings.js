// 设置：后端 / 工作流 / 云端 / 图像档位 / 外观 / 导出 / 短语 / 预设 / 数据目录 / 关于，一个弹窗装下
'use strict';
import { el, fill } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api } from '../core/api.js';
import { isDesktop, call, pickFolder, human, openExternal } from '../core/desktop.js';
import { MODES, apply, mode, resolved, watch } from '../core/theme.js';
import { osReduce, reduce, saved, set as setMotion, watch as watchMotion } from '../core/motion.js';
import { store } from '../state.js';
import { modal } from './modal.js';
import { toastOk, toastErr, toastBusy } from './toast.js';
import { createBackendsPane } from './backends.js';
import { createPresetsPane } from './presets.js';
import { createPhrasesPane } from './phrases.js';

/* 后端地址与 ComfyUI 根目录是同一件事的两半——都在回答"我连的是哪个 ComfyUI"，
   分成两个分区会让人在两边各填一半、自检结果却在另一个分区里 */
const SECTIONS = [['backend', 'ComfyUI'], ['workflow', '工作流参数'], ['cloud', '云端生成'], ['image', '图像档位'], ['theme', '外观'],
  ['export', '导出目录'], ['phrases', '提示词短语'], ['presets', '参数预设'], ['data', '数据目录'], ['about', '关于']];
const VERIFY_TEXT = { ok: '指纹一致', mismatch: '与基准不符', missing: '文件不在', 'no-baseline': '无基准可比' };
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
        okRow(cfg.cfg_source === 'workflow', cfg.cfg_source === 'workflow' ? '参数读自本机工作流' : '工作流读不到，正用内置默认参数', cfg.workflow_error || ''),
      ];
      if (r) rows.push(graphRow(r));
      rows.push(okRow(!!s.active, '当前后端 ' + String(s.active).replace(/^https?:\/\//, '')));
      fill(state, ...rows);
    } catch (e) { fill(state, okRow(false, '读不到设置', e.message)); }
  }

  async function save() {
    try {
      const r = await api.setWorkflow(inp.value.trim());
      toastOk(r.cfg_source === 'workflow' ? '已关联工作流' : '已保存，但读不到该文件', r.workflow_path || '');
      store.set({ cfg: await api.cfg() });
      load();
    } catch (e) { toastErr('保存失败', e.message); }
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: '工作流文件路径' }), inp,
      el('p.muted', { text: '填 ComfyUI 的 API 导出（Save (API Format) / Export (API)）：那份 JSON 就是提交用的计算图，Synco 只往里面填照片、遮罩、提示词、种子/步数/CFG 和 LoRA 开关，其余按你文件里那样跑。UI 导出（save）只读参数，当不了计算图。' }),
      el('div', { style: { display: 'flex', gap: '8px', marginTop: '10px' } },
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: '保存并校验', onclick: save }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', 'data-tip': '认出哪些节点负责装图 / 采样 / 出图，对不上就自己指', text: '角色映射', onclick: () => editRoles(load) }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', 'data-tip': '列出这个文件里有哪些节点、缺哪些', text: '清点节点', onclick: () => inspectWorkflow() }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '重新读取', onclick: load }))),
    state);
  load();
  return { node };
}

/** 计算图来源那一行：接管成功是绿点，认不出角色是红点（提交会被明确拒掉），非 API 导出是灰点 */
function graphRow(r) {
  if (r.can_takeover) return okRow(true, '提交用的计算图：你的工作流', `${Object.keys(r.effective || {}).length} 个角色已就位`);
  if (!r.is_api) return okRow(null, '提交用的计算图：程序内置', r.reason || '');
  return okRow(false, '你的工作流还不能接管提交（提交会被拒）', (r.errors || []).join('；'));
}

/**
 * 角色映射编辑器：每一行是一个"Synco 要往哪儿填东西"，下拉里只有同一种类的节点。
 * 默认值取自动认出的那个——只有你改过的才会存进库，节点号被 ComfyUI 重排时还能重新认。
 */
async function editRoles(reload) {
  let d;
  try { d = await api.workflowRoles(); } catch (e) { toastErr('读不到角色表', e.message); return; }
  if (!d.is_api) {
    modal({
      title: '这个文件当不了计算图',
      body: el('p.muted', { text: `${d.reason || '读不到节点表'}。角色映射要的是 API 导出：在 ComfyUI 菜单里点 Save (API Format) 或 Export (API)，再把那个文件指过来。` }),
      actions: [{ label: '知道了', kind: 'ghost' }],
    });
    return;
  }
  const auto = d.auto || {}, eff = d.effective || {};
  const selects = [];
  const byClass = cls => (d.nodes || []).filter(n => n.class_type === cls);
  const rows = (d.roles || []).map(r => {
    const list = byClass(r.class);
    const cur = eff[r.key] || '';
    const sel = el('select.select', { style: { flex: '0 0 auto', minWidth: '112px', maxWidth: '48%' } },
      el('option', { value: '', text: r.required ? '（没认出，必须指一个）' : '不用这一路输出' }),
      ...list.map(n => el('option', {
        value: n.id,
        text: `节点 ${n.id}${n.title ? ` · ${n.title}` : ''}${auto[r.key] === n.id ? '（自动认出）' : ''}`,
      })));
    sel.value = cur;
    if (!list.length) sel.disabled = true;
    selects.push({ key: r.key, node: sel, auto: auto[r.key] });
    return el('div.set__row', {},
      el('b', { text: r.label }),
      el('span.set__extra', { text: `${r.class} · 图里 ${list.length} 个${r.required ? ' · 必需' : ''}` }),
      sel);
  });
  const verdict = el('div', {});
  const paintVerdict = (v) => fill(verdict,
    el('h4.dlg-h4', { text: '校验' }),
    ...(v.can_takeover ? [okRow(true, '角色齐、连线也对，提交走你这张图')]
      : [okRow(false, '还不行（提交会被拒）', (v.errors || []).join('；'))]));
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
      if ((r.rejected || []).length) toastErr('有几项没存进去', r.rejected.join('；'));
      else toastOk('角色映射已保存');
      if (close) { reload?.(); handle?.close(); }
    } catch (e) { toastErr('保存失败', e.message); }
  }
  return modal({
    title: '角色映射', wide: true,
    body: el('div.dlg-flow', {},
      el('p.muted', { text: 'Synco 按角色往你的图里填东西，不认节点号：挪位置、改编号都不影响。认不出来的在这里指一次，按文件各存一份。' }),
      ...rows, verdict),
    actions: [
      { label: '存一下看校验', kind: 'ghost', run: (h) => { saveRoles(h, false); return false; } },
      { label: '保存并关闭', kind: 'primary', run: (h) => { saveRoles(h, true); return false; } },
    ],
  });
}

/**
 * 把工作流文件里的节点摊开给人看（类名与个数），缺哪个认识的角色一并列出。
 */
async function inspectWorkflow() {
  const busy = toastBusy('清点节点…');
  let d;
  try { d = await api.workflowInspect(); } catch (e) { busy.close(); toastErr('清点失败', e.message); return; }
  busy.close();
  const counts = new Map();
  for (const n of d.nodes || []) counts.set(n.class_type, (counts.get(n.class_type) || 0) + 1);
  const row = (k, v) => el('div.set__row', {}, el('b', { text: k }), el('span.set__extra', { text: v }));
  const body = el('div.dlg-flow', {},
    el('p.muted', { text: d.format === 'api' ? 'API 导出（按输入名取参数，也能当计算图提交）' : 'UI/litegraph 导出（按控件位置取参数，只读参数）' } + ` · 共 ${d.total} 个节点`),
    d.stitch_pair_ok ? null : el('div.set__row.is-bad', {}, el('span.dot', { class: 'dot dot--err' }),
      el('b', { text: '没看到裁切与缝合那一对节点' }),
      el('span.set__extra', { text: '「蒙版外逐像素不动」靠 InpaintCropImproved + InpaintStitchImproved 在工作流里完成。缺了它们这张图不能接管提交。' })),
    el('h4.dlg-h4', { text: '节点清单' }),
    ...[...counts.entries()].sort((a, b) => a[0].localeCompare(b[0])).map(([k, n]) => row(k, `${n} 个`)),
    (d.known_missing || []).length
      ? el('div', {}, el('h4.dlg-h4', { text: '本管线认识、这个文件里没有的' }), el('p.muted', { text: d.known_missing.join('、') }))
      : null);
  return modal({ title: '工作流节点清点', wide: true, body, actions: [{ label: '关闭', kind: 'ghost' }] });
}

function createCloudPane() {
  const inp = (ph, type = 'text', extra = {}) => el('input.input', { type, spellcheck: 'false', placeholder: ph, ...extra });
  const baseInp = inp('https://api.openai.com/v1，或你的中转地址');
  const modelInp = inp('模型名照接口方给的写，例如 gpt-image-2');
  const sizeInp = inp('输出长边，例如 1024');
  const qualInp = inp('画质，例如 medium / high');
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
      okRow(cloudCk.checked, cloudCk.checked ? '当前走云端' : '当前走本机 ComfyUI', cloudCk.checked ? '没装 ComfyUI 的机器用这条' : ''),
      okRow(!!(data?.base && data?.model && data?.key_saved), '三项配齐', data?.key_saved ? `key 末四位 ${data.key_tail}` : '还没填 key'),
      last ? okRow(!!last.ok, '接口测试', last.ok ? `${last.ms}ms${last.models != null ? ` · 列出 ${last.models} 个模型` : ''}` : (last.error || '')) : null);
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
    keyInp.placeholder = d.key_saved ? `已保存 ····${d.key_tail}（留空表示不改）` : '还没填 API key';
    paintState();
  }

  async function save(patch) {
    try {
      const r = await api.saveCloud(patch);
      absorb(r, Object.keys(patch));
      store.set({ cloud: r }, 'cloud');
      toastOk('已保存云端配置', r.kind === 'cloud' ? '生成走云端' : '生成仍走本机 ComfyUI');
    } catch (e) { toastErr('保存失败', e.message); }
  }

  const saveAll = () => save({
    base: baseInp.value, model: modelInp.value, size: sizeInp.value, quality: qualInp.value,
    key: keyInp.value, timeout: timeoutInp.value, concurrency: concInp.value,
    stitch_expand: expandInp.value, stitch_feather: featherInp.value, stitch_edge: edgeInp.value,
  });

  async function test() {
    const busy = toastBusy('测接口中…');
    try { last = await api.testCloud(); busy.close(); paintState(); }
    catch (e) { busy.close(); last = { ok: false, error: e.message }; paintState(); }
    last.ok ? toastOk('接口应答正常', `${last.ms}ms`) : toastErr('接口不应答', String(last.error || '').slice(0, 120));
  }

  async function load() {
    try { absorb(await api.cloud()); } catch (e) { fill(state, okRow(false, '读不到云端配置', e.message)); }
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: '生成方式' }),
      el('label.set__ck', {}, cloudCk, el('span', { text: '改用云端局部重绘（这台机器没有 ComfyUI 时勾上）' })),
      el('p.muted', { text: '勾上后精修页的提交会把蒙版外扩、裁出一块裁切区发给云端，成图拿回后在浏览器里做色彩校正与多频段缝合贴回原图——未涂区域保持原图像素，不会有整图漂移。步数 / CFG / LoRA / 种子在这一路没有对应参数，面板会收掉；负面提示词并进正向一起发。批量提交暂时还没接云端。' })),
    el('div', { style: { display: 'grid', gap: '10px' } },
      fld('base_url（接口方给的前缀，含 /v1）', baseInp),
      pair(fld('模型名', modelInp), fld('默认输出长边（精修页提交时可临时改）', sizeInp)),
      pair(fld('画质（留空=不发这个字段）', qualInp), fld('API key', keyInp)),
      pair(fld('超时（毫秒）', timeoutInp), fld('云端并发（阶段 3 才用）', concInp)),
      el('h4.dlg-h4', { text: '缝合参数（接缝明显时往大调）' }),
      el('p.muted', { style: { marginBottom: '6px' }, text: '外扩 = 模型重绘范围比涂抹大多少像素，接缝藏在这里面；羽化 = 贴回过渡带宽度，平滑织物 / 渐变背景建议 64 以上。二者联动约束羽化 ≤ 0.6×外扩，超了会自动收回来。改完要重新提交生成才生效。' }),
      el('div', { style: { display: 'grid', gridTemplateColumns: '1fr 1fr 1fr', gap: '10px' } },
        fld('蒙版外扩（px）', expandInp),
        fld('贴回羽化（px）', featherInp),
        fld('裁切目标长边', edgeInp)),
      el('div', { style: { display: 'flex', gap: '8px', flexWrap: 'wrap' } },
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: '保存', onclick: saveAll }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '测试接口', onclick: test }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', 'data-tip': '从本机库里抹掉 key', text: '清掉 key', onclick: () => save({ clear_key: true }) }))),
    state,
    el('p.muted', { style: { marginTop: '10px' }, text: 'key 只写在本机 SQLite，服务只绑回环，任何接口都不会把它回传给页面；但它仍是明文落盘，别把 data 目录同步出去。' }));
  load();
  return { node };
}

function createSetupPane() {
  // 占位符不能是任何人的真实机器路径：那会让人以为软件"默认要装在 D 盘"
  const rootInp = el('input.input', { type: 'text', spellcheck: 'false', placeholder: '本机 ComfyUI 便携包那一层，如 C:\\...\\ComfyUI_windows_portable' });
  const proxyInp = el('input.input', { type: 'text', spellcheck: 'false', placeholder: 'http://127.0.0.1:7897（留空不用代理）' });
  const body = el('div.dlg-flow');
  const progBox = el('div');
  const verifyBox = el('div.set__verify');
  let data = null, timer = 0;

  async function load(silent) {
    try {
      data = await api.setup();
      if (!rootInp.value) rootInp.value = data.root || '';
      paint();
      if (!silent) toastOk('自检完成', `${data.detect.packs.filter(p => p.installed).length}/${data.detect.packs.length} 个节点包 · 缺 ${gb(data.detect.missing_bytes)} 权重`);
    } catch (e) { if (!silent) toastErr('自检失败', e.message); }
  }

  function paintProgress() {
    const p = data?.progress;
    if (!p) return null;
    const pct = p.total ? Math.round((p.done / p.total) * 100) : 0;
    return el('div.set__prog', {},
      el('span', { text: `${p.status === 'error' ? '失败' : p.status === 'done' ? '完成' : '进行中'} · ${p.message || ''} (${p.done || 0}/${p.total || 0})` }),
      el('div.set__track', {}, el('i', { style: { width: pct + '%' } })));
  }

  function paint() {
    const d = data?.detect;
    if (!d) { fill(body, el('p.muted', { text: '填 ComfyUI 根目录后点「自检」。' })); return; }
    const r = d.runtime;
    const root = d.root || '';
    /* 原来这里印的是字面量 "<root>\ComfyUI"——占位符从来没被替换过；
       而且根目录还没填的时候报"没找到"是误导，那时候选状态是"未知" */
    const env = (label, ok, rel, why) => okRow(root ? ok : null, label,
      root ? (ok ? '' : `没找到 ${root}\\${rel}`) : why || '还没填根目录：选一个本机 ComfyUI 便携包的根目录再自检');
    const rows = [
      el('div', {},
        el('h4.dlg-h4', { text: '运行环境' }),
        env('ComfyUI 主程序', r.comfyui && r.main_py, 'ComfyUI'),
        env('内嵌 Python', r.python, 'python_embeded'),
        okRow(root ? r.qwen_nodes : null, 'Qwen 内置节点 nodes_qwen.py',
          root ? (r.qwen_nodes ? '' : 'ComfyUI 版本过旧，升级后才有') : '还没填根目录')),
      el('div', {},
        el('h4.dlg-h4', { text: '第三方节点包（这条管线只用这两个）' }),
        ...d.packs.map(p => okRow(p.installed, p.dir,
          p.installed ? p.label : '缺：' + p.label + '（' + p.nodes.join(' / ') + '）'))),
      el('div', {},
        el('h4.dlg-h4', { text: `权重（共 ${gb(d.total_bytes)}，缺 ${gb(d.missing_bytes)}）` }),
        ...d.models.map(m => okRow(m.found ? true : m.optional ? null : false, m.label,
          m.found
            ? `${gb(m.bytes)} · ${m.sha256 ? '有指纹基准' : '无指纹基准'}`
            : m.optional ? '可选 · 未启用' : `缺文件 · ${m.dir}/${m.rel}`)),
        el('div', { style: { display: 'flex', gap: '8px', alignItems: 'center', marginTop: '8px' } },
          el('button.btn.btn--ghost.btn--sm', { type: 'button', 'data-tip': '逐个算本机指纹与基准比对，约十几秒', html: icon('eye', { cls: 'icon icon--sm' }) + '<span>深度校验指纹</span>', onclick: deepVerify }),
          verifyBox)),
      el('div', {},
        el('h4.dlg-h4', { text: '一键配置脚本' }),
        el('p.muted', { text: '生成到 data\\setup\\ 后自己双击 install_comfyui.bat：本机没有 ComfyUI 便携包时它会自己下载并解压（约 4 GB，带断点续传与进度），然后克隆缺的节点包、装依赖、逐个核对权重指纹。权重它不替你下——清单里没有下载源，缺的只报路径与体积，不动你的文件。' }),
        el('div', { style: { display: 'flex', gap: '8px', marginTop: '8px', flexWrap: 'wrap' } },
          el('button.btn.btn--primary.btn--sm', { type: 'button', html: icon('download', { cls: 'icon icon--sm' }) + '<span>生成脚本</span>', onclick: gen }),
          el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '重新自检', onclick: () => load() })),
        proxyInp),
      progBox];
    /* 输入框里的路径和自检用的那份不一致时，下面所有结论都还是旧目录的——
       不写这一行，人就会对着"没找到"怀疑自己装错了地方 */
    if (root && rootInp.value && rootInp.value !== root) rows.unshift(
      el('div.set__row.is-unknown', {}, el('span.dot', {}), el('b', { text: '路径改过了' }),
        el('span.set__extra', { text: '下面的结果属于旧目录，点「保存并自检」才按新路径重测' })));
    fill(body, ...rows);
  }

  async function deepVerify() {
    fill(verifyBox, el('span.muted', { text: '算指纹中，22GB 约十几秒…' }));
    try {
      const r = await api.verifySetup(rootInp.value.trim());
      const bad = r.rows.filter(x => x.status === 'mismatch' || x.status === 'missing');
      /* 三态而不是"非黑即白"：没有基准可比既不是通过也不是损坏，画成红叉会让人以为文件坏了 */
      const state = s => (s === 'ok' ? true : s === 'no-baseline' ? null : false);
      fill(verifyBox, ...r.rows.map(x => okRow(state(x.status), `${x.label} ${VERIFY_TEXT[x.status] || x.status}`)));
      if (bad.length) toastErr('有文件与基准不符', bad.map(x => x.label).join('、') + '：可能被换过版本或下载损坏');
      else toastOk('指纹校验通过', `${r.rows.filter(x => x.status === 'ok').length} 个文件与基准一致`);
    } catch (e) { fill(verifyBox, el('span.muted', { text: '校验失败：' + e.message })); }
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
      toastOk('脚本已生成', r.bat);
    } catch (e) { toastErr('生成失败', e.message); }
  }

  async function saveRoot() {
    try { await api.setSetupRoot(rootInp.value.trim()); await load(); }
    catch (e) { toastErr('保存失败', e.message); }
  }

  /* 选目录而不是让人手打：粘贴进来的路径常带引号或尾斜杠，那种值判存在性会为假，
     症状就是明明装对了却报"没找到 ComfyUI 主程序" */
  async function browseRoot() {
    let p;
    try { p = await pickFolder('选 ComfyUI 便携包根目录（里面应有 ComfyUI 与 python_embeded 两层）'); }
    catch (e) { toastErr('目录框没打开', e.message); return; }
    if (!p) return;
    rootInp.value = p;
    await saveRoot();
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: 'ComfyUI 根目录（本机）' }),
      el('div.set__add', {}, rootInp,
        isDesktop() ? el('button.btn.btn--ghost.btn--sm', {
          type: 'button', 'data-tip': '不用手打路径——粘贴常带引号或尾斜杠，那种路径判存在性会为假',
          text: '浏览…', onclick: browseRoot }) : null,
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: '保存并自检', onclick: saveRoot })),
      el('p.muted', { text: '远端后端只能探接口，目录体检针对本机这一台。' })),
    body);
  load(true);
  /* 面板是缓存下来反复挂卸的：一见 node 离开 DOM 就 clearInterval，切走再回来进度条就永久冻在那一刻。
     改成"不在 DOM 里就跳过这一轮"，空转的成本是一次布尔判断，不是网络请求。 */
  timer = setInterval(() => { if (document.body.contains(node)) poll(); }, 2500);
  return { node };
}

/** 导出目录：成图直接复制到本机文件夹，不经过浏览器下载 */
function createExportPane() {
  const inp = el('input.input', { type: 'text', spellcheck: 'false', placeholder: 'D:\\导出\\成图（不存在会自动创建）' });
  const state = el('div.set__state');

  async function load() {
    try {
      const d = await api.exportGet();
      inp.value = d.dir || '';
      fill(state, okRow(!!d.ready,
        d.ready ? '目录在，点「导出」直接落盘'
          : d.dir ? '这个目录当前不存在（U 盘没插？），导出时会再验一次并建好'
            : '还没设置：精修页点「导出」会提醒你来这里填'));
    } catch (e) { fill(state, okRow(false, '读不到导出设置', e.message)); }
  }

  async function save() {
    try {
      const r = await api.exportSetDir(inp.value.trim());
      inp.value = r.dir;
      toastOk('导出目录已就绪', r.dir);
      load();
    } catch (e) { toastErr('目录不可用', e.message); }
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: '导出目录（本机文件夹）' }),
      el('div.set__add', {}, inp,
        // 桌面版给一个系统目录框：手写 D:\导出\成图 这种路径太容易打错
        isDesktop() ? el('button.btn.btn--ghost.btn--sm', {
          type: 'button', text: '浏览…', onclick: async () => {
            let p;
            try { p = await pickFolder('选一个放导出成图的文件夹'); } catch (e) { toastErr('目录框没打开', e.message); return; }
            if (p) { inp.value = p; save(); }
          },
        }) : null,
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: '保存并校验', onclick: save })),
      el('p.muted', { text: '精修页点「导出」把当前成图按「原图名_#记录id.png」直接复制到这里，不经过浏览器下载；目录不存在会自动创建。每次导出都会探一次可写性，U 盘拔了这类情况会当场报错。' })),
    state);
  load();
  return { node };
}

/** 图像档位：编辑/查看用的 proxy 长边，加上服务端队列的实时快照 */
function createImagePane() {
  const edgeInp = el('input.input', { type: 'number', min: '1024', max: '8192', step: '256' });
  const state = el('div.set__state');

  async function load() {
    try {
      const [s, q] = await Promise.all([api.get('/api/settings'), api.queueState()]);
      edgeInp.value = String(s.proxy_edge ?? 3072);
      fill(state,
        okRow(true, '当前编辑档位', `长边超过 ${s.proxy_edge} 的原图才切 proxy；缩略图 320、瓦片 512 是固定的`),
        okRow(q.queued === 0, '云端队列', `排队 ${q.queued} · 在跑 ${q.running} · 并发上限 ${q.concurrency}`),
        okRow(null, '改档位之后', '档位写进文件名，旧档自然失效，新档由首次访问时懒切——不用手工清目录'));
    } catch (e) { fill(state, okRow(false, '读不到档位', e.message)); }
  }

  async function save() {
    const busy = toastBusy('保存中…');
    try { const r = await api.setProxyEdge(+edgeInp.value || 3072); busy.close(); toastOk('档位已设', `长边超过 ${r.proxy_edge} 才切 proxy`); load(); }
    catch (e) { busy.close(); toastErr('保存失败', e.message); }
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: '编辑档位（proxy 长边）' }), edgeInp,
      el('p.muted', { text: '精修画布与涂抹层都按这一档工作：调低更省内存与显存，调高能在原图上抠更细。原图永远留在盘上，改这一档不会动原图。' }),
      el('div', { style: { display: 'flex', gap: '8px', marginTop: '10px' } },
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: '保存', onclick: save }),
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '重新读取', onclick: load }))),
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
    try { to = await pickFolder('选一个目录放工坊的资料'); } catch (e) { toastErr('目录框没打开', e.message); return; }
    if (!to) return;
    try {
      info = (await call('set_data_dir', { path: to })).dir;
      paint();
      toastOk('已记下新目录', '重启工坊后生效；旧目录里的东西不会被动');
    } catch (e) { toastErr('换址没成', String(e.message || e)); }
  }

  async function copy() {
    let to;
    try { to = await pickFolder('把资料复制到这个目录（必须是空目录）'); } catch (e) { toastErr('目录框没打开', e.message); return; }
    if (!to) return;
    const busy = toastBusy('正在复制资料…');
    try {
      const r = await call('copy_data_to', { to });
      busy.close();
      info = r.dir;
      paint();
      toastOk(`已复制 ${r.copied.files} 个文件（${human(r.copied.bytes)}）`, '重启后从新目录打开');
    } catch (e) { busy.close(); toastErr('复制失败', String(e.message || e)); }
  }

  async function restart() {
    try { await call('restart_app'); } catch (e) { toastErr('重启没成功', e.message); }
  }

  function paint() {
    const path = info?.path || vinfo?.data_dir || '';
    fill(state,
      okRow(!!path, '当前数据目录', path ? `${path}${info ? `（${info.source}）` : ''}` : '读不到'),
      info ? okRow(info.has_db, '里面有什么', `${info.projects} 个项目 · ${info.images} 张图 · ${human(info.bytes)}`) : null,
      okRow(vinfo?.desktop, '运行形态', vinfo?.desktop
        ? '桌面版 · 界面内嵌在 exe 里'
        : `浏览器版 · 界面从盘上 ${vinfo?.public_dir || ''} 读`),
      info ? okRow(null, '备份口径', '库里只有 app.db（+WAL），原图与成图在同目录的 projects/ 下——要备份就得整个目录一起拿') : null,
      ipcErr ? okRow(false, '壳那一侧读不到', `${ipcErr}（目录、项目数与体积要问壳，所以这三行没了；换址与搬家按钮也会失灵）`) : null,
      isDesktop() ? null : okRow(null, '换目录', '这一步要桌面版：用 Synco.exe 打开后这里会出现选址与搬家按钮'));
    fill(acts,
      info?.restart_required ? btn('立即重启工坊', 'primary', restart) : null,
      isDesktop() ? btn('换一个目录…', 'ghost', choose) : null,
      isDesktop() && info && !info.restart_required ? btn('把资料复制到新目录…', 'ghost', copy) : null);
  }

  async function load() {
    // Tauri 的 IPC 失败 reject 出来的是字符串，读 e.message 只会拿到 undefined，
    // 真实原因（权限、命令没注册、状态没托管）就这么被"读不到数据目录"盖掉了
    const why = e => String(e?.message || e || '未知错');
    try {
      vinfo = await api.get('/api/version');
    } catch (e) {
      fill(state, okRow(false, '读不到数据目录', why(e)));
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
    el('div', {}, el('h4.dlg-h4', { text: '资料放在哪' }),
      el('p.muted', { text: '几百个项目、几十 GB 权重之外，工坊自己的库与原图都只在这个目录里。桌面版第一次启动会就近找老库；找错了就在这里改，改完可以只记地址、也可以把东西整体复制过去。' }),
      acts),
    state);
  load();
  return { node };
}

/** 外观：三档底色 + 减弱动效 + 当前实际生效的那一档 */
function createThemePane() {
  const row = el('div', { style: { display: 'flex', gap: '8px', flexWrap: 'wrap' } });
  const now = el('div.set__state');
  /* 减弱动效（F7 的兜底开关）：和系统的 prefers-reduced-motion 同一个语义，任一到就全站瞬时——
     视口惯性、瓦片淡入、翻页的视图过渡、主题 crossfade 全关掉，悬停那些也给压成 .01ms。
     和主题一个道理存 localStorage 而不是库里：它是这台机器的显示偏好，换台机器不该被带走，
     而且首帧之前就要读到（CSP 之下只有 core/theme-boot.js 赶得上，它抢在 body 存在之前挂 <html>）。 */
  const sw = el('button.toggle', {
    type: 'button', role: 'switch', 'aria-checked': String(saved()), 'aria-label': '减弱动效',
    onclick: () => { setMotion(!saved()); paint(); },
  });

  /* 哪一路在起作用要说清楚：应用开关关掉不等于系统那一档也关了（那就还是减弱的） */
  const motionLine = () => {
    if (!reduce()) return '动效：全开（画布惯性、翻页淡入、主题淡入都在）';
    if (saved() && osReduce()) return '动效：已减弱（这一档开关与系统的「减少动态效果」都开着）';
    if (saved()) return '动效：已减弱（这一档开关）';
    return '动效：已减弱（来自系统的「减少动态效果」偏好；这里留关即可只跟系统）';
  };

  function paint() {
    const cur = mode();
    fill(row, ...MODES.map(([k, label]) => el('button.btn.btn--sm', {
      type: 'button', class: `btn btn--sm${k === cur ? ' btn--primary' : ' btn--ghost'}`,
      text: label, onclick: () => { apply(k); paint(); },
    })));
    sw.setAttribute('aria-checked', String(saved()));
    fill(now,
      el('p.muted', {
        text: `当前生效：${resolved() === 'dark' ? '墨黑' : '纸白'}${cur === 'system' ? '（跟随系统，改系统外观会立刻跟着变）' : '（固定档，不随系统）'}`,
      }),
      el('p.muted', { text: motionLine() }));
  }

  const node = el('div.dlg-flow', {},
    el('div', {}, el('h4.dlg-h4', { text: '界面底色' }), row,
      el('p.muted', { text: '只换窗口、列表与面板。画布四周那一圈一直是中性深底——判色要在稳定底色下做，换主题不会把照片往冷暖任何一边带。' })),
    el('div', {}, el('h4.dlg-h4', { text: '动效' }),
      el('div.set__ck', {}, sw, el('span', { text: '减弱动效（翻页不再淡入、画布手势不再滑行，主题一改就到位）' })),
      now,
      el('p.muted', { text: '对着系统里那个「减少动态效果」是同一档事：任一边开着，这里就一律瞬时。开着时主题切换没有淡入（那本来就是淡入），底色照样立刻换到位。' })));
  paint();
  watch(() => paint());
  watchMotion(() => paint());
  return { node };
}

/** 关于：版本号 + 这次构建的 commit + 最近提交当日志；没有发布渠道就不装「自动更新」 */
/* 作者页是写死的：这一栏不读库、不读配置，装了安装包也照样在 */
const AUTHOR = { name: '@杉果派', url: 'https://github.com/ShanGuoP' };
/* 仓库地址：更新日志那一栏由服务端回，读不到时退到这个常量（致谢里的 NOTICE 链接也用它） */
const REPO = 'https://github.com/ShanGuoP/Synco';

/* 开源致谢：只列真的进了这个二进制的东西，许可口径见仓库根的 NOTICE.md。
   评估过但没采用的（photon-rs、YuNet/FaceMesh 权重）不写在这里——
   写没在包里的署名既没义务也没意义，还会让人以为界面里有人脸功能。 */
const CREDITS = [
  ['axum · tokio · rusqlite · image · imageproc · fast_image_resize · reqwest · rust-embed', 'MIT / Apache-2.0'],
  ['Tauri 2 桌面壳', 'MIT / Apache-2.0'],
  ['moving-least-squares（几何形变数学）', 'MPL-2.0', 'https://github.com/mpizenberg/rust_mls'],
  ['Noto Serif SC 界面字体', 'SIL OFL 1.1', 'https://fonts.google.com/noto/specimen/Noto+Serif+SC'],
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
    onclick: async e => { e.preventDefault(); if (!(await openExternal(url))) toastErr('叫不动系统浏览器', url); },
    text: label,
  });
  const linkRow = (label, ...kids) => el('div.set__row', {}, el('span.dot'), el('b', { text: label }), ...kids);

  async function load() {
    let v, r;
    try { v = await api.get('/api/version'); } catch (e) { fill(state, okRow(false, '读不到版本', e.message)); return; }
    try { r = await api.get('/api/releases'); } catch (e) { r = { releases: [], error: e.message }; }

    const repo = r.repo || REPO;
    const built = v.commit && v.commit !== 'unknown' ? `构建 ${v.commit} · ${String(v.date).slice(0, 10)}` : '';
    const latest = (r.latest || {}).tag ? String(r.latest.tag).replace(/^v/, '') : '';
    const rows = [
      // 中文名暂定「新刻」：这一栏是名字的权威出处，界面别处只跟着它走
      okRow(true, 'Synco 新刻', '本机 ComfyUI / 云端两用的局部重绘工作台'),
      okRow(true, `版本 ${v.version}`, built),
      // 只在真的不是最新时多说一句；一致的时候不写"已是最新版"这种废话
      (latest && latest !== v.version) ? okRow(null, `GitHub 上有 ${latest}`, '') : null,
      linkRow('仓库', link(repo.replace(/^https:\/\//, ''), repo)),
      linkRow('作者', link(AUTHOR.name, AUTHOR.url)),
    ];
    if (latest && latest !== v.version && (r.latest || {}).download) rows.push(linkRow('下载', link(`${latest} 安装包`, r.latest.download)));
    fill(state, ...rows);

    const cs = r.releases || [];
    fill(log,
      el('h4.dlg-h4', { text: '更新日志' }),
      // 三种状态分开说：拉失败、拉到了但仓库没发布过、拉到了有内容。
      // 把"还没有 Release"报成"网络不通"会让人白查半天
      r.error
        ? [okRow(false, '更新日志没拉到', r.error)]
        : cs.length
          ? cs.map(c => el('div.set__row', {},
              el('span.muted', { text: `${c.date} · ${c.tag}` }),
              link(c.name && c.name !== c.tag ? c.name : '这一版', c.url),
              c.download ? link('下载', c.download) : null))
          : [okRow(null, '仓库还没有发布 Release', '在 GitHub 上发布版本后，这里会列出每一版并给出安装包链接')]);
  }

  const node = el('div.dlg-flow', {},
    el('div', { style: { display: 'flex', justifyContent: 'flex-end' } },
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '重新读取', onclick: load })),
    state, log,
    el('h4.dlg-h4', { text: '开源致谢' }),
    ...CREDITS.map(([what, lic, url]) => el('div.set__row', {},
      el('span.dot'), el('span', { text: what }), el('span.muted', { text: lic }),
      url ? link('来源', url) : null)),
    linkRow('完整清单', link('NOTICE.md', `${REPO}/blob/main/NOTICE.md`)));
  load();
  return { node };
}

export function settingsModal(section = 'backend') {
  let cur = section;
  const pane = el('div.set__pane');
  const built = {};
  const nav = el('div.set__nav', {}, ...SECTIONS.map(([k, label]) => el('button.set__nav__it', {
    type: 'button', class: `set__nav__it${k === cur ? ' is-on' : ''}`, dataset: { k }, text: label,
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
      fill(pane, built[k].node);
    },
  })));
  nav.querySelector('.is-on').click();
  const m = modal({
    title: '设置', wide: true,
    body: el('div.set', {}, nav, pane),
    actions: [{ label: '关闭', kind: 'ghost' }],
  });
  return m;
}
