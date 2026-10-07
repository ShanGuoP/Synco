// 画布（图生图）：一张白纸、一支笔、一句话，整幅交给云端生成。
// 复用绘制层与视口引擎，不复用编辑器骨架——编辑器那一块整段都假设"有原图 + 有结果行"，
// 塞一个 blank 开关要加十几个 guard 而换来的复用为零。
'use strict';
import { el, $, fill } from '../../core/dom.js';
import { icon } from '../../core/icons.js';
import { api, fileToPayload } from '../../core/api.js';
import { encodeMask } from '../../core/maskEncode.js';
import { store, loadProject, loadPhrases } from '../../state.js';
import { go } from '../../core/router.js';
import { toastOk, toastErr, toastBusy } from '../../ui/toast.js';
import { confirm, modal } from '../../ui/modal.js';
import { makeSlider } from '../../ui/controls.js';
import { createViewport } from '../editor/viewport.js';
import { createPainter } from '../editor/paint.js';
import { createHistory } from '../editor/history.js';
import { createCompare } from '../editor/compare.js';
import { togglePhrase } from '../editor/params.js';
import { adopt } from '../../gen.js';

/* 画布用深色墨：存的是"墨 + 透明底"的 PNG，发到云端前拍到白底上，屏幕上看到的就是发出去的 */
const INK = '#1f1e1c';
const SIZES = [['1024×1024', 1024, 1024], ['1152×896', 1152, 896], ['896×1152', 896, 1152], ['1536×1024', 1536, 1024]];
const TOOLS = [
  { key: 'pan', ico: 'hand', label: '抓手', tip: '平移画布 · H' },
  { key: 'brush', ico: 'brush', label: '画笔', tip: '画草稿 · B' },
  { key: 'erase', ico: 'eraser', label: '橡皮', tip: '擦回白纸 · E' },
];
const ZOOMS = [
  { key: 'fit', ico: 'fit', label: '合适', tip: '适应窗口 · 0' },
  { key: 'one', ico: 'one2one', label: '1:1', tip: '实际像素 · 1' },
  { key: 'zin', ico: 'zoomIn', label: '放大', tip: '或按 +' },
  { key: 'zout', ico: 'zoomOut', label: '缩小', tip: '或按 -' },
];

let c = null;                      // 上下文：只建一次，换画布复用
let seq = 0;                       // 装载序号，晚到的旧响应要能认出自己过期
let rerunFrom = null;              // 下一次生成是"哪条成图的再来一版"

export const isCanvasOpen = () => !!c && !$('#canvasView').hidden;

/* ==================== 新建 ==================== */
/** 尺寸建好之后改不了（画布就是那张纸），所以开之前先选清楚 */
export function newCanvas({ projectId = null } = {}) {
  return new Promise(res => {
    let settled = false;
    const finish = v => { if (!settled) { settled = true; res(v); } };
    const nm = el('input.input', { type: 'text', maxlength: '60', placeholder: '如 海报草稿' });
    let size = SIZES[0];
    const seg = el('div.seg', {}, ...SIZES.map(([label, w, h], i) => el('button.seg__it', {
      type: 'button', class: `seg__it${i === 0 ? ' is-on' : ''}`, text: label,
      onclick: e => {
        size = [label, w, h];
        for (const b of seg.children) b.classList.toggle('is-on', b === e.currentTarget);
      },
    })));
    let busy = { close() {} };
    const submit = async () => {
      busy = toastBusy('建画布…');
      try {
        const r = await api.canvasCreate({ name: nm.value.trim(), w: size[1], h: size[2], project_id: projectId });
        busy.close();
        finish(r);
        m.close('ok');
      } catch (e) {
        busy.close();
        toastErr('建不起来', e.message);
        nm.focus();
      }
    };
    const box = el('div', { style: { display: 'grid', gap: '10px' } },
      el('div', { style: { display: 'grid', gap: '4px' } }, el('span.muted', { text: '名字' }), nm),
      el('div', { style: { display: 'grid', gap: '4px' } }, el('span.muted', { text: '画布尺寸' }), seg),
      el('p.muted', { text: '云端对画布有硬约束：长边 ≤3840、比例 ≤3:1、像素 65.5 万~829 万。这几档都在框里。' }),
      el('div', { style: { display: 'flex', justifyContent: 'flex-end', gap: '8px' } },
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '取消', onclick: () => { finish(null); m.close('cancel'); } }),
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: '建画布', onclick: submit })));
    const m = modal({ title: '新建画布', body: box, onClose: () => finish(null) });
    requestAnimationFrame(() => nm.focus());
  });
}

/* ==================== 骨架 ==================== */
function build() {
  const paper = el('canvas', { class: 'cv-paper' });
  const layer = el('div.vp__canvas.cv-layer', {}, paper);
  const zoomPct = el('span.tool__val', { text: '100%' });
  const hud = el('span.pill', { text: '—' });
  const saveFlag = el('span.saveflag');
  const vpBox = el('div.vp', {}, layer);
  const stage = el('div.cv-stage', {}, vpBox, el('div.ed-hud.ed-hud--bl', {}, hud));
  // 光标定位要 offsetParent，所以它得在 stage 里面（编辑器那张也是这么挂的）
  const cursor = el('div.brush-cur');
  stage.append(cursor);

  const viewport = createViewport({
    stage, layer,
    onScale: s => { zoomPct.textContent = `${Math.round(s * 100)}%`; c?.painter?.invalidate?.(); },
  });
  /* 对比层与修图页共用同一个组件：措辞换过来，中缝拖拽那套数学不抄第二份 */
  const compare = createCompare({
    onClose: () => { c.cmpId = null; syncResults(c.rows || [], null); },
    onRestore: r => usePrompt(r),
    onUseSketch: r => takeSketch(r),
    copy: {
      left: '当时画稿', right: '成图', overOnWhite: true,
      restore: '用这条提示词再改一版',
      restoreTip: '把这一版的提示词搬回输入框，并记下谱系：下一版会标「改自 #id」',
    },
  });
  vpBox.append(compare.node);

  const brushVal = el('span.tool-slider__val', { text: '42' });
  const brushSlider = makeSlider({
    min: 2, max: 200, step: 2, value: 42, ariaLabel: '笔刷大小',
    onChange: v => { brushVal.textContent = String(v); c.painter.setBrush(v); },
  });
  const toolBtn = (k, ico, label, tip, onclick) => el('button.tool', {
    type: 'button', dataset: { k }, 'data-tip': tip, onclick,
  }, el('span.tool__ico', { html: icon(ico) }), el('span', { text: label }));
  const tools = el('div.ed-tools', {},
    el('button.tool-zoom', { type: 'button', 'data-tip': '点击适应窗口', onclick: () => viewport.fit() }, zoomPct),
    ...ZOOMS.map(z => toolBtn(z.key, z.ico, z.label, z.tip, () => onZoom(z.key))),
    el('span.tool-sep'),
    ...TOOLS.map(t => toolBtn(t.key, t.ico, t.label, t.tip, () => setTool(t.key))),
    el('span.tool-sep'),
    toolBtn('undo', 'undo', '撤销', '回退一笔 · Ctrl+Z', () => onUndo()),
    toolBtn('clear', 'trash', '清空', '擦成一张白纸', () => onClear()),
    el('div.tool-slider', {}, el('span.tool-slider__lab', { text: '笔刷' }), brushSlider.node, brushVal));

  const name = el('b.nowrap');
  const top = el('div.cv-top', {},
    el('div.cv-top__l', {},
      el('button.btn.btn--ghost.btn--icon.btn--sm', { type: 'button', 'aria-label': '返回项目', 'data-tip': '返回项目', html: icon('left', { cls: 'icon icon--sm' }), onclick: goBack }),
      el('div.ed-file', {}, name, el('span', { class: 'cv-tag', text: '画布' }))),
    el('div.cv-top__r', {}, saveFlag,
      el('button.btn.btn--ghost.btn--sm', { type: 'button', html: icon('refresh', { cls: 'icon icon--sm' }) + '<span>刷新成图</span>', onclick: () => load(c.imgId) })));

  /* 右侧：提示词 + 短语胶囊 + 参考图槽 + 提交 */
  const ta = el('textarea.textarea', { rows: '6', placeholder: '描述你要生成什么：一段一个主体一个动作', spellcheck: 'false' });
  const chips = el('div.chips.chips--side');
  const count = el('span.badge', { text: '0 字' });
  const go2 = el('button.btn.btn--accent.btn--block', { type: 'button', onclick: () => submit() });
  const line = el('div.run-line');
  const paintGo = (busy) => {
    go2.innerHTML = busy
      ? '<span class="spin spin--dark"></span><span class="btn__label">生成中…</span>'
      : `${icon('wand', { cls: 'icon icon--sm' })}<span class="btn__label">生成</span>`;
    go2.classList.toggle('is-busy', busy);
    go2.disabled = !!busy;
  };

  /* 参考图槽带：两条来源各一个按钮，不做弹出菜单——槽位状态要一眼看得见 */
  const refGrid = el('div.refslots');
  const refCount = el('span.muted', { text: '' });
  const refFile = el('input', { type: 'file', accept: 'image/*', multiple: 'multiple', hidden: 'hidden', style: { display: 'none' } });
  refFile.onchange = () => { const fs = [...refFile.files]; refFile.value = ''; addRefFiles(fs); };
  const refUp = el('button.btn.btn--ghost.btn--sm', {
    type: 'button', 'data-tip': '相机直出的图、截图都行；会被折进云端合法的尺寸档',
    html: icon('upload', { cls: 'icon icon--sm' }) + '<span>本地上传</span>', onclick: () => refFile.click(),
  });
  const refPick = el('button.btn.btn--ghost.btn--sm', {
    type: 'button', 'data-tip': '从这个项目里已有的图挑', html: icon('image', { cls: 'icon icon--sm' }) + '<span>从项目挑</span>',
    onclick: addRefFromProject,
  });
  const refBox = el('div.grp', {},
    el('div.cv-refhd', {}, el('span.cv-refhd__t', { text: '参考图' }), refCount, el('span.grow'), refUp, refPick),
    el('div.grp__bd', {}, refGrid, refFile));

  /* 右侧两列：生成列 + 结果历史列（与修图页同一套栏宽与卡片摆位，历史不再叠在生成面板下面） */
  const history = createHistory({
    onPick: showResult, onDel: delResult, onRestore: restoreRow, onFork: forkResult,
    emptyHint: '这张画布还没生成过。写好提示词，点右侧「生成」。',
  });
  const side = el('aside.cv-side', {},
    el('div.col-hd', {}, el('h3', { html: icon('wand', { cls: 'icon icon--sm' }) + '<span>生成</span>' })),
    el('div.cv-side__bd', {},
      el('div.grp', {}, el('div.grp__bd', {}, ta)),
      chips,
      el('div', { style: { display: 'flex', alignItems: 'center', gap: '8px' } }, count, el('span.grow'), el('span.muted', { text: '整幅生成 · 不保留画稿像素' })),
      refBox,
      line, go2));
  const histCol = el('div.cv-hist', {}, history.node);

  const painter = createPainter({
    mask: paper, cursor, viewport, ink: INK,
    onDirty: () => setFlag('保存中…', 'busy'),
    onSaved: onSketchSaved,
  });

  const node = el('div.cv', {}, top, tools, el('div.cv-main', {}, stage, side, histCol));
  const cur = {
    node, stage, vpBox, layer, paper, viewport, painter, tools, ta, chips, count, line, hud, name, saveFlag, setFlag,
    history, compare, go2, paintGo, side, refGrid, refCount, rows: [], refList: [], refMax: 4,
  };
  paintGo(false);
  return cur;
}

function setFlag(txt, kind = '') {
  c.saveFlag.textContent = txt;
  c.saveFlag.className = `saveflag${kind === 'busy' ? ' is-busy' : kind === 'ok' ? ' is-ok' : kind === 'err' ? ' is-err' : ''}`;
}

/* ==================== 打开 / 关闭 ==================== */
export async function openCanvas(projectId, imgId) {
  const host = $('#canvasView');
  if (!c) {
    c = build();
    host.append(c.node);
    wireKeys();
  }
  // 数据到手再掀覆盖层：脏 URL 不该显示一个空画布
  const p = store.peek('project');
  if (!p || p.id !== +projectId) {
    try { await loadProject(projectId); } catch (e) { toastErr('项目打不开', e.message || String(e)); go('/'); return; }
  }
  host.hidden = false;
  $('#shell').style.visibility = 'hidden';
  c.projectId = +projectId;
  await loadPhrases();          // 胶囊与修图页共用同一批短语；没加载过就是空的
  paintChips();
  await load(+imgId);
}

export function closeCanvas() {
  const host = $('#canvasView');
  if (host) host.hidden = true;
  const shell = $('#shell');
  if (shell) shell.style.visibility = '';
  c?.compare?.hide();
  Promise.resolve(c?.painter.flush()).catch(() => { /* 关页前冲一次，落不住也有队列里的那行兜着 */ });
}

const goBack = () => { const p = store.peek('project'); go(p ? `/p/${p.id}` : '/'); };

/* ==================== 装载一张画布 ==================== */
async function load(imgId) {
  const mine = ++seq;
  const stale = () => mine !== seq;
  const busy = toastBusy('打开画布…');
  let d;
  try { d = await api.canvas(imgId); } catch (e) { busy.close(); toastErr('打不开画布', e.message); goBack(); return; }
  busy.close();
  if (stale()) return;
  c.imgId = imgId;
  c.info = d;
  c.cmpId = null;
  c.compare.hide();       // 换画布不能把上一张的对比层留着：那是别人家的成图
  rerunFrom = null;      // 换画布不清掉谱系，下一版就会挂在上一张画布的成图上
  const im = d.image;
  c.name.textContent = im.name;
  c.hud.textContent = `${im.w}×${im.h} · 云端整幅生成`;
  c.viewport.setContentSize(im.w, im.h);
  // 画稿就是内容本身：1:1 装载，不做任何降采样
  await c.painter.load(im.w, im.h, d.sketch_url, imgId, { w: im.w, h: im.h });
  if (stale()) return;
  c.viewport.fit();
  setTool('brush');
  setFlag('已载入');
  paintChips();
  syncResults(d.results || []);
  c.refMax = d.refs_max || c.refMax || 4;
  paintRefs(d.refs || []);
}

function syncResults(rows, activeId) {
  c.rows = rows || [];
  c.history.setResults(c.rows, activeId ?? c.cmpId ?? null);
}

/* ==================== 参考图槽位 ==================== */
/* 槽位集合存在服务端（不是浏览器内存）：刷新页面、切走再回来都还要看得见"我挂了哪几张" */
function paintRefs(list) {
  c.refList = list || [];
  fill(c.refGrid,
    ...c.refList.map(ref => el('div.refslot', { title: ref.name || '' },
      ref.dead ? el('span.refslot__gone', { text: '文件已丢失' }) : el('img', { src: ref.url, alt: '', loading: 'lazy' }),
      el('button.refslot__x', {
        type: 'button', 'aria-label': '撤下这张参考图', 'data-tip': '撤下（不动盘上原来那张）',
        html: icon('close', { cls: 'icon icon--sm' }),
        onclick: () => setRefs(c.refList.filter(x => x.path !== ref.path).map(x => x.path)),
      }))),
    c.refList.length < c.refMax ? el('button.refslot.refslot--add', {
      type: 'button', 'aria-label': '从项目里挑一张参考图', 'data-tip': '从项目里挑',
      html: icon('plus', { cls: 'icon icon--sm' }), onclick: addRefFromProject,
    }) : null);
  c.refCount.textContent = `${c.refList.length}/${c.refMax}`;
}

/** 整组替换：撤一张、清空都走这条。服务端只删"这次不再认"的那些文件 */
async function setRefs(paths) {
  try {
    const d = await api.canvasSetRefs(c.imgId, paths);
    paintRefs(d.refs || []);
  } catch (e) { toastErr('槽位没改成', e.message); refreshRefs(); }
}

async function refreshRefs() {
  try { const d = await api.canvas(c.imgId); paintRefs(d.refs || []); } catch { /* 集合读不到就维持屏幕上那一版 */ }
}

const isImgFile = f => /^image\//.test(f.type || '') || /\.(jpe?g|png|webp|bmp|avif)$/i.test(f.name || '');

async function addRefFiles(files) {
  const list = files.filter(isImgFile);
  if (!list.length) { toastErr('不是图片文件', '支持 png / jpg / webp / bmp / avif'); return; }
  const room = c.refMax - c.refList.length;
  if (room <= 0) { toastErr(`参考图最多 ${c.refMax} 张`, '先撤下一张再传'); return; }
  if (list.length > room) toastErr(`只收前 ${room} 张`, `参考图一次最多 ${c.refMax} 张`);
  const payloads = [];
  for (const f of list.slice(0, room)) {
    try { payloads.push(await fileToPayload(f)); } catch (e) { toastErr('这张读不出来', `${f.name}：${e.message}`); }
  }
  if (!payloads.length) return;
  const busy = toastBusy('正在收下参考图…');
  try {
    const d = await api.canvasAddRefs(c.imgId, { files: payloads.map(p => ({ b64: p.b64 })) });
    busy.close();
    paintRefs(d.refs || []);
  } catch (e) { busy.close(); toastErr('参考图没加上', e.message); refreshRefs(); }
}

async function addRefIds(ids) {
  if (!ids.length) return;
  if (c.refMax - c.refList.length <= 0) { toastErr(`参考图最多 ${c.refMax} 张`, '先撤下一张再挑'); return; }
  try {
    const d = await api.canvasAddRefs(c.imgId, { image_ids: ids.slice(0, c.refMax - c.refList.length) });
    paintRefs(d.refs || []);
  } catch (e) { toastErr('参考图没加上', e.message); refreshRefs(); }
}

/** 从本项目已有的图里挑：加进来的是**复制的一份**，原图后来被删也不影响"这一版参考了哪张" */
function addRefFromProject() {
  const imgs = (store.peek('images') || []).filter(i => i.id !== c.imgId);
  if (!imgs.length) { toastErr('这个项目里还没有别的图', '先导入几张，或用「本地上传」'); return; }
  const m = modal({
    title: '从项目里挑参考图',
    body: el('div.refpick', {}, ...imgs.map(i => el('button.refpick__it', {
      type: 'button', onclick: () => { addRefIds([i.id]); m.close('ok'); },
    }, i.thumb_url ? el('img', { src: i.thumb_url, alt: '', loading: 'lazy' }) : el('span.refpick__ph'),
      el('span', { text: i.name, title: i.name })))),
  });
}

/** 历史条目的「回填」：提示词 + 那一版**实际带走**的参考图整套搬回待提交状态 */
async function restoreRow(r) {
  if (!r.prompt) { toastErr('这一版没留提示词'); return; }
  c.ta.value = r.prompt;
  syncPrompt();
  rerunFrom = r.id;
  c.compare.hide();
  if (!r.refs?.length) { line(`准备好用 #${r.id} 的提示词再改一版了`); return; }
  try {
    const d = await api.canvasAddRefs(c.imgId, { from_result: r.id });
    paintRefs(d.refs || []);
    line(`已把 #${r.id} 那版的 ${d.refs.length} 张参考图搬进槽位`);
  } catch (e) {
    toastErr('这一版的参考图搬不过来', e.message);
    line(`提示词已回填，参考图没搬过来：${String(e.message || '').slice(0, 60)}`);
  }
}

/** 成图另存为项目里的新图：要接着涂遮罩就走修图页那一条，画布这张不动 */
async function forkResult(r) {
  if (!r?.final_url) { toastErr('这条记录还没有成图'); return; }
  if (r.final_dead) { toastErr('这条成图的文件已经不在了', '记录还留着，PNG 不在盘上了。删掉这条记录即可'); return; }
  const busy = toastBusy('正在复制成图…');
  try {
    const f = await api.forkResult(r.id);
    await loadProject(c.projectId);
    busy.close();
    go(`/p/${c.projectId}/e/${f.image_id}`);
    toastOk('已另存为新图', `${f.name} · 画布和这张成图各自独立`);
  } catch (e) { busy.close(); toastErr('另存失败', e.message); }
}

/* ==================== 胶囊与提示词 ==================== */
function paintChips() {
  const list = (store.peek('phrases') || []).filter(p => p.prompt);
  fill(c.chips, ...list.map(p => el('button.chip-s', {
    type: 'button', text: p.name, dataset: { phrase: p.prompt },
    onclick: () => {
      const on = c.chips.querySelector(`[data-phrase="${CSS.escape(p.prompt)}"]`)?.classList.contains('is-on');
      c.ta.value = togglePhrase(c.ta.value, p.prompt, on);
      syncPrompt();
    },
  })));
  syncPrompt();
}

function syncPrompt() {
  const cur = c.ta.value || '';
  for (const el0 of c.chips.querySelectorAll('.chip-s[data-phrase]')) el0.classList.toggle('is-on', cur.includes(el0.dataset.phrase));
  c.count.textContent = `${cur.length} 字`;
}

/* ==================== 保存画稿 ==================== */
/** 空白画布也要存得回去：paint.js 判空时不给 b64，这里自己导一次全透明的 PNG */
async function onSketchSaved({ empty, b64, id }) {
  if (!id || id !== c.imgId) return true;      // 已经换画布了：这批笔迹不属于屏幕上这张
  let data = b64;
  if (empty && !data) {
    try { data = (await encodeMask(c.paper)).b64; } catch { setFlag('保存失败', 'err'); return false; }
  }
  try {
    await api.saveSketch(id, data);
    setFlag('画稿已保存', 'ok');
    return true;
  } catch (e) {
    setFlag('保存失败', 'err');
    toastErr('画稿保存失败', e.message);
    return false;
  }
}

/* ==================== 生成 ==================== */
async function submit() {
  if (!c?.imgId || c.busy) return;
  const prompt = (c.ta.value || '').trim();
  if (!prompt) { toastErr('提示词是空的', '画布只看这句话生成'); return; }
  await c.painter.flush();
  const imgId = c.imgId;
  const rerunOf = rerunFrom; rerunFrom = null;
  let r;
  try { r = await api.canvasGenerate(imgId, { prompt }, rerunOf); }
  catch (e) { toastErr('提交失败', e.message); return; }
  if (!r?.result_id) { toastErr('没有排上', r?.error || '云端没接这次提交'); return; }
  c.busy = true;
  c.paintGo(true);
  const withRefs = c.refList.length ? ` · 带 ${c.refList.length} 张参考图` : '';
  line(rerunOf
    ? `已排进云端队列 #${r.result_id}（改自 #${rerunOf}${withRefs}）…`
    : `已排进云端队列 #${r.result_id}${withRefs}…`);
  adopt(r.result_id, imgId, {
    onTick: s => { if (c.imgId === imgId) line(s === 'queued' ? '排队中…' : '云端生成中…'); },
    onDone: async done => {
      if (c.imgId === imgId) { c.busy = false; c.paintGo(false); }
      line(done?.status === 'done' ? `完成 #${done.id}` : `失败：${String(done?.error || '未知原因').slice(0, 90)}`);
      try {
        const d = await api.canvas(imgId);
        if (c.imgId === imgId) syncResults(d.results || []);
      } catch { /* 列表刷新失败不影响这次结果本身 */ }
      done?.status === 'done' ? toastOk('画布出图', `#${done.id}`) : toastErr('生成失败', String(done?.error || '').slice(0, 120));
    },
  });
}

function line(txt) {
  fill(c.line, txt ? el('span', { class: /失败|错误|拒收/.test(txt) ? 'err' : '', text: txt }) : null);
}

/* ==================== 成图查看 / 删除 ==================== */
/**
 * 点开一条历史：把成图叠在画布上，与**这一版当时的线稿**分栏拉动对比。
 * 快照功能上线之前的那些版本没有左半边，这时只铺成图并写明原因——
 * 拿"现在的画稿"冒充"当时的画稿"比不展示更坏。
 */
function showResult(r) {
  if (r.status !== 'done' || r.final_dead) {
    toastErr(r.final_dead ? '成图文件已丢失' : '这条还没出图', r.error || '还排在队列里或已经失败');
    return;
  }
  c.cmpId = r.id;
  syncResults(c.rows, r.id);
  c.compare.show({
    origUrl: r.sketch_url || null,
    resultUrl: r.final_url,
    title: `#${r.id} · ${(r.prompt || '').slice(0, 46)}${r.prompt && r.prompt.length > 46 ? '…' : ''}`,
    result: r,
    leftLabel: r.sketch_url ? '当时画稿' : '这一版没留画稿',
  });
}

/** 用这一版的提示词再改一版：谱系记在下一行上，历史列才看得出这张是从哪一版迭代来的 */
function usePrompt(r) {
  if (!r.prompt) { toastErr('这一版没留提示词'); return; }
  c.ta.value = r.prompt;
  syncPrompt();
  rerunFrom = r.id;
  c.compare.hide();
  line(`准备好用 #${r.id} 的提示词再改一版了`);
}

/** 取回这一版当时的线稿：写回画稿本体，屏幕上回到"提交那一刻我画的东西" */
async function takeSketch(r) {
  const ok = await confirm({
    title: `取回 #${r.id} 那版的画稿`,
    text: '画布会换成这一版提交时的线稿快照。你现在屏幕上的笔迹会被覆盖（快照本身不动，切回那一版还能再取一次）。',
    okLabel: '取回画稿',
  });
  if (!ok) return;
  const busy = toastBusy('取回画稿…');
  try {
    /* 先把屏幕上的笔迹冲出去再让服务端写回快照：不冲的话随后那次 load() 会带着"取回前那版"
       的 pending 笔迹 POST 回 /sketch，把刚写回的快照当场盖掉，而提示仍然说"已取回" */
    await c.painter.flush();
    await api.useSketch(c.imgId, r.id);
    busy.close();
    c.compare.hide();
    await load(c.imgId);
    setFlag('已取回这一版的画稿', 'ok');
    toastOk('画稿已取回', `#${r.id} 那版`);
  } catch (e) {
    busy.close();
    toastErr('取回失败', e.message);
  }
}

async function delResult(r) {
  const ok = await confirm({
    title: `删除记录 #${r.id}`,
    text: r.final_url ? '这条记录连同它的成图文件一起删掉。画稿不动。' : '只删这条记录。',
    danger: true, okLabel: '删除记录',
  });
  if (!ok) return;
  try {
    await api.delResult(r.id);
    const d = await api.canvas(c.imgId);
    syncResults(d.results || []);
    toastOk('记录已删除', `#${r.id}`);
  } catch (e) { toastErr('删除失败', e.message); }
}

/* ==================== 工具 / 缩放 / 键位 ==================== */
function setTool(k) {
  c.tool = k;
  for (const b of c.tools.querySelectorAll('.tool[data-k]')) b.classList.toggle('is-on', b.dataset.k === k);
  c.viewport.setMode(k === 'pan' ? 'pan' : 'paint');
  c.painter.setTool(k === 'erase' ? 'erase' : 'brush');
}

function onZoom(k) {
  ({ fit: () => c.viewport.fit(), one: () => c.viewport.one2one(),
     zin: () => c.viewport.zoomBy(1.35), zout: () => c.viewport.zoomBy(1 / 1.35) })[k]?.();
}

function onUndo() {
  if (!c.painter.canUndo) { toastErr('没有可撤销的笔画'); return; }
  c.painter.undo().then(ok => { if (ok) setFlag('已撤销一笔'); });
}

async function onClear() {
  const ok = await confirm({ title: '擦成一张白纸', text: '撤销键救不回来（会留 8 步），但画稿会在你下一次落笔时自动存回盘上。', okLabel: '清空' });
  if (!ok) return;
  c.painter.clear();
}

function wireKeys() {
  window.addEventListener('keydown', e => {
    if (!isCanvasOpen()) return;
    const tag = document.activeElement?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA') {
      if (e.key === 'Escape') document.activeElement.blur();
      return;
    }
    if (e.code === 'Space') { e.preventDefault(); (c.compare.isOn ? c.compare : c.viewport).setSpace(true); return; }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z') { e.preventDefault(); onUndo(); return; }
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') { e.preventDefault(); submit(); return; }
    const k = e.key.toLowerCase();
    if (k === 'b') setTool('brush');
    else if (k === 'e') setTool('erase');
    else if (k === 'h') setTool('pan');
    else if (k === '0') (c.compare.isOn ? c.compare : c.viewport).fit();
    else if (k === '1') (c.compare.isOn ? c.compare : c.viewport).one2one();
    else if (k === '+' || k === '=') (c.compare.isOn ? c.compare : c.viewport).zoomBy(1.3);
    else if (k === '-') (c.compare.isOn ? c.compare : c.viewport).zoomBy(1 / 1.3);
    // 对比层开着时 Esc 先关对比，不该一步退出画布
    else if (e.key === 'Escape') { if (c.compare.isOn) c.compare.hide(); else goBack(); }
  });
  window.addEventListener('keyup', e => {
    if (e.code !== 'Space') return;
    // 按下时给了谁就还给谁：对比层与画布各有自己的 viewport
    (c?.compare.isOn ? c.compare : c?.viewport)?.setSpace(false);
  });
}
