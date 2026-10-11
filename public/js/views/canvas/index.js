// 画布（图生图）：一张白纸、一支笔、一句话，整幅交给云端生成。
// 复用绘制层与视口引擎，不复用编辑器骨架——编辑器那一块整段都假设"有原图 + 有结果行"，
// 塞一个 blank 开关要加十几个 guard 而换来的复用为零。
'use strict';
import { el, $, fill } from '../../core/dom.js';
import { icon } from '../../core/icons.js';
import { api, fileToPayload } from '../../core/api.js';
import { encodeMask } from '../../core/maskEncode.js';
import { dx, t } from '../../core/i18n.js';
import { store, loadProject, loadPhrases } from '../../state.js';
import { go } from '../../core/router.js';
import { createViewSession } from '../../core/viewSession.js';
import { hold } from '../../core/guard.js';
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
  { key: 'pan', ico: 'hand', label: 'ed.tPan', tip: 'ed.tPanTip' },
  { key: 'brush', ico: 'brush', label: 'dlg.dBrush', tip: 'cv.tBrushTip' },
  { key: 'erase', ico: 'eraser', label: 'dlg.dEraser', tip: 'cv.tEraseTip' },
];
const ZOOMS = [
  { key: 'fit', ico: 'fit', label: 'ed.tFit', tip: 'ed.tFitTip' },
  { key: 'one', ico: 'one2one', label: 'ed.tOne', tip: 'ed.tOneTip' },
  { key: 'zin', ico: 'zoomIn', label: 'compare.zoomIn', tip: 'ed.tZinTip' },
  { key: 'zout', ico: 'zoomOut', label: 'compare.zoomOut', tip: 'ed.tZoutTip' },
];

let c = null;                      // 上下文：只建一次，换画布复用
const session = createViewSession();
const inFlight = new Set();        // 有提交在飞的画布 id：闸门要按画布存，切走再回来才知道按钮该不该禁着
let seq = 0;                       // 装载序号，晚到的旧响应要能认出自己过期
let rerunFrom = null;              // 下一次生成是"哪条成图的再来一版"

export const isCanvasOpen = () => !!c && !$('#canvasView').hidden;

/* ==================== 新建 ==================== */
/** 尺寸建好之后改不了（画布就是那张纸），所以开之前先选清楚 */
export function newCanvas({ projectId = null } = {}) {
  return new Promise(res => {
    let settled = false;
    const finish = v => { if (!settled) { settled = true; res(v); } };
    const nm = el('input.input', { type: 'text', maxlength: '60', placeholder: t('cv.namePh') });
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
      busy = toastBusy(t('cv.busy'));
      try {
        const r = await api.canvasCreate({ name: nm.value.trim(), w: size[1], h: size[2], project_id: projectId });
        busy.close();
        finish(r);
        m.close('ok');
      } catch (e) {
        busy.close();
        toastErr(t('home.createFail'), e.message);
        nm.focus();
      }
    };
    const box = el('div', { style: { display: 'grid', gap: '10px' } },
      el('div', { style: { display: 'grid', gap: '4px' } }, el('span.muted', { text: t('ph.name') }), nm),
      el('div', { style: { display: 'grid', gap: '4px' } }, el('span.muted', { text: t('cv.sizeLab') }), seg),
      el('p.muted', { text: t('cv.sizeNote') }),
      el('div', { style: { display: 'flex', justifyContent: 'flex-end', gap: '8px' } },
        el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('common.cancel'), onclick: () => { finish(null); m.close('cancel'); } }),
        el('button.btn.btn--primary.btn--sm', { type: 'button', text: t('cv.create'), onclick: submit })));
    const m = modal({ title: t('home.canvasBtn'), body: box, onClose: () => finish(null) });
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
      left: t('cv.leftSketch'), right: t('compare.right'), overOnWhite: true,
      restore: t('cv.restore'),
      restoreTip: t('cv.restoreTip'),
    },
  });
  vpBox.append(compare.node);

  const brushVal = el('span.tool-slider__val', { text: '42' });
  const brushSlider = makeSlider({
    min: 2, max: 200, step: 2, value: 42, ariaLabel: t('ed.brushAria'),
    onChange: v => { brushVal.textContent = String(v); c.painter.setBrush(v); },
  });
  const toolBtn = (k, ico, label, tip, onclick) => el('button.tool', {
    type: 'button', dataset: { k }, 'data-tip': tip, 'aria-label': label, onclick,
  }, el('span.tool__ico', { html: icon(ico) }), el('span', { text: label }));
  const tools = el('div.ed-tools', {},
    el('button.tool-zoom', { type: 'button', 'data-tip': t('ed.zoomTip'), onclick: () => viewport.fit() }, zoomPct),
    ...ZOOMS.map(z => toolBtn(z.key, z.ico, t(z.label), t(z.tip), () => onZoom(z.key))),
    el('span.tool-sep'),
    ...TOOLS.map(d => toolBtn(d.key, d.ico, t(d.label), t(d.tip), () => setTool(d.key))),
    el('span.tool-sep'),
    toolBtn('undo', 'undo', t('ed.tUndo'), t('ed.tUndoTip'), () => onUndo()),
    toolBtn('clear', 'trash', t('ed.tClear'), t('cv.eraseAll'), () => onClear()),
    el('div.tool-slider', {}, el('span.tool-slider__lab', { text: t('ed.brushLab') }), brushSlider.node, brushVal));

  const name = el('b.nowrap');
  const togglePanel = key => {
    const off = node.classList.toggle(key === 'gen' ? 'is-gen-collapsed' : 'is-history-collapsed');
    if (!off && document.documentElement.dataset.appearance === 'glass' && window.matchMedia('(max-width: 960px)').matches) {
      node.classList.add(key === 'gen' ? 'is-history-collapsed' : 'is-gen-collapsed');
    }
    for (const button of node.querySelectorAll('[data-panel]')) {
      button.setAttribute('aria-expanded', String(!node.classList.contains(button.dataset.panel === 'gen' ? 'is-gen-collapsed' : 'is-history-collapsed')));
    }
  };
  const top = el('div.cv-top', {},
    el('div.cv-top__l', {},
      el('button.btn.btn--ghost.btn--icon.btn--sm', { type: 'button', 'aria-label': t('ed.back'), 'data-tip': t('ed.back'), html: icon('left', { cls: 'icon icon--sm' }), onclick: goBack }),
      el('div.ed-file', {}, name, el('span', { class: 'cv-tag', text: t('nav.canvas') }))),
    el('div.cv-top__r', {}, saveFlag,
      el('button.btn.btn--ghost.btn--sm.workspace-only', { type: 'button', dataset: { panel: 'gen' }, 'aria-expanded': 'true', text: t('ed.railProp'), onclick: () => togglePanel('gen') }),
      el('button.btn.btn--ghost.btn--sm.workspace-only', { type: 'button', dataset: { panel: 'history' }, 'aria-expanded': 'false', text: t('ed.railHist'), onclick: () => togglePanel('history') }),
      el('button.btn.btn--ghost.btn--sm', { type: 'button', html: icon('refresh', { cls: 'icon icon--sm' }) + `<span>${t('cv.refreshResult')}</span>`, onclick: () => load(c.imgId) })));

  /* 右侧：提示词 + 短语胶囊 + 参考图槽 + 提交 */
  const ta = el('textarea.textarea', { rows: '6', placeholder: t('cv.promptPh'), spellcheck: 'false' });
  const chips = el('div.chips.chips--side');
  const count = el('span.badge', { text: t('pm.charCount', { n: 0 }) });
  const go2 = el('button.btn.btn--accent.btn--block', { type: 'button', onclick: () => submit() });
  const line = el('div.run-line');
  const paintGo = (busy) => {
    go2.innerHTML = busy
      ? `<span class="spin spin--dark"></span><span class="btn__label">${t('pm.busy')}</span>`
      : `${icon('wand', { cls: 'icon icon--sm' })}<span class="btn__label">${t('cv.gen')}</span>`;
    go2.classList.toggle('is-busy', busy);
    go2.disabled = !!busy;
  };

  /* 参考图槽带：两条来源各一个按钮，不做弹出菜单——槽位状态要一眼看得见 */
  const refGrid = el('div.refslots');
  const refCount = el('span.muted', { text: '' });
  const refFile = el('input', { type: 'file', accept: 'image/*', multiple: 'multiple', hidden: 'hidden', style: { display: 'none' } });
  refFile.onchange = () => { const fs = [...refFile.files]; refFile.value = ''; addRefFiles(fs); };
  const refUp = el('button.btn.btn--ghost.btn--sm', {
    type: 'button', 'data-tip': t('cv.refUpTip'),
    html: icon('upload', { cls: 'icon icon--sm' }) + `<span>${t('cv.refUp')}</span>`, onclick: () => refFile.click(),
  });
  const refPick = el('button.btn.btn--ghost.btn--sm', {
    type: 'button', 'data-tip': t('cv.projPickTip'), html: icon('image', { cls: 'icon icon--sm' }) + `<span>${t('cv.projPick')}</span>`,
    onclick: addRefFromProject,
  });
  const refBox = el('div.grp', {},
    el('div.cv-refhd', {}, el('span.cv-refhd__t', { text: t('cv.refsTitle') }), refCount, el('span.grow'), refUp, refPick),
    el('div.grp__bd', {}, refGrid, refFile));

  /* 右侧两列：生成列 + 结果历史列（与修图页同一套栏宽与卡片摆位，历史不再叠在生成面板下面） */
  const history = createHistory({
    onPick: showResult, onDel: delResult, onRestore: restoreRow, onFork: forkResult,
    emptyHint: t('cv.emptyHint'),
  });
  const side = el('aside.cv-side', {},
    el('div.col-hd', {}, el('h3', { html: icon('wand', { cls: 'icon icon--sm' }) + `<span>${t('cv.gen')}</span>` })),
    el('div.cv-side__bd', {},
      el('div.grp', {}, el('div.grp__bd', {}, ta)),
      chips,
      el('div', { style: { display: 'flex', alignItems: 'center', gap: '8px' } }, count, el('span.grow'), el('span.muted', { text: t('cv.fullNote') })),
      refBox,
      line, go2));
  const histCol = el('div.cv-hist', {}, history.node);

  const painter = createPainter({
    mask: paper, cursor, viewport, ink: INK,
    onDirty: () => setFlag(t('ed.saving'), 'busy'),
    onSaved: onSketchSaved,
  });

  const node = el('div.cv.is-history-collapsed', {}, top, tools, el('div.cv-main', {}, stage, side, histCol));
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
  const opening = session.begin();
  ++seq;
  if (c) {
    try { await c.painter.flush(); }
    catch (e) { if (opening.current()) toastErr(t('cv.openFail'), e.message); return; }
    if (!opening.current()) return;
  }
  const host = $('#canvasView');
  if (!c) {
    c = build();
    host.append(c.node);
    wireKeys();
  }
  // 数据到手再掀覆盖层：脏 URL 不该显示一个空画布
  const p = store.peek('project');
  if (!p || p.id !== +projectId) {
    try { await loadProject(projectId, opening.current); } catch (e) { if (opening.current()) { toastErr(t('ed.projFail'), e.message || String(e)); go('/'); } return; }
  }
  if (!opening.current()) return;
  host.hidden = false;
  $('#shell').style.visibility = 'hidden';
  c.projectId = +projectId;
  await loadPhrases();          // 胶囊与修图页共用同一批短语；没加载过就是空的
  if (!opening.current()) return;
  paintChips();
  await load(+imgId);
}

export function closeCanvas() {
  session.end();
  ++seq;
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
  if (!session.current()) return;
  const mine = ++seq;
  const stale = () => mine !== seq || !session.current();
  const busy = toastBusy(t('cv.openBusy'));
  let d;
  try { d = await api.canvas(imgId); } catch (e) { busy.close(); if (!stale()) { toastErr(t('cv.openFail'), e.message); goBack(); } return; }
  busy.close();
  if (stale()) return;
  c.imgId = imgId;
  c.info = d;
  c.cmpId = null;
  c.compare.hide();       // 换画布不能把上一张的对比层留着：那是别人家的成图
  rerunFrom = null;      // 换画布不清掉谱系，下一版就会挂在上一张画布的成图上
  const im = d.image;
  c.name.textContent = im.name;
  c.hud.textContent = t('cv.dimsCloud', { wh: `${im.w}×${im.h}` });
  c.viewport.setContentSize(im.w, im.h);
  // 画稿就是内容本身：1:1 装载，不做任何降采样
  await c.painter.load(im.w, im.h, d.sketch_url, imgId, { w: im.w, h: im.h }, () => !stale());
  if (stale()) return;
  c.viewport.fit();
  setTool('brush');
  setFlag(t('cv.loaded'));
  paintChips();
  syncResults(d.results || []);
  c.refMax = d.refs_max || c.refMax || 4;
  paintRefs(d.refs || []);
  // 按钮状态要跟着"这张有没有在飞"走：从生成中的画布切走再切回来，禁用的必须是禁用、
  // 能点的必须是能点，而不是沿用上一张的界面
  c.paintGo(inFlight.has(imgId));
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
      ref.dead ? el('span.refslot__gone', { text: t('hist.goneFile') }) : el('img', { src: ref.url, alt: '', loading: 'lazy' }),
      el('button.refslot__x', {
        type: 'button', 'aria-label': t('cv.refDropAria'), 'data-tip': t('cv.refDropTip'),
        html: icon('close', { cls: 'icon icon--sm' }),
        onclick: () => setRefs(c.refList.filter(x => x.path !== ref.path).map(x => x.path)),
      }))),
    c.refList.length < c.refMax ? el('button.refslot.refslot--add', {
      type: 'button', 'aria-label': t('cv.refPickAria'), 'data-tip': t('cv.refPickTip'),
      html: icon('plus', { cls: 'icon icon--sm' }), onclick: addRefFromProject,
    }) : null);
  c.refCount.textContent = `${c.refList.length}/${c.refMax}`;
}

/** 整组替换：撤一张、清空都走这条。服务端只删"这次不再认"的那些文件 */
async function setRefs(paths) {
  const same = hold(() => c.imgId);
  try {
    const d = await api.canvasSetRefs(c.imgId, paths);
    if (!same()) return;
    paintRefs(d.refs || []);
  } catch (e) { if (!same()) return; toastErr(t('cv.slotFail'), e.message); refreshRefs(); }
}

async function refreshRefs() {
  const same = hold(() => c.imgId);
  try { const d = await api.canvas(c.imgId); if (same()) paintRefs(d.refs || []); } catch { /* 集合读不到就维持屏幕上那一版 */ }
}

const isImgFile = f => /^image\//.test(f.type || '') || /\.(jpe?g|png|webp|bmp|avif)$/i.test(f.name || '');

async function addRefFiles(files) {
  const list = files.filter(isImgFile);
  if (!list.length) { toastErr(t('cv.notImage'), t('cv.formats')); return; }
  const room = c.refMax - c.refList.length;
  if (room <= 0) { toastErr(t('cv.refMax', { n: c.refMax }), t('cv.refFullUpload')); return; }
  if (list.length > room) toastErr(t('cv.onlyFirst', { n: room }), t('cv.refMaxBatch', { n: c.refMax }));
  const payloads = [];
  for (const f of list.slice(0, room)) {
    try { payloads.push(await fileToPayload(f)); } catch (e) { toastErr(t('cv.fileFail'), t('cv.fileBody', { name: f.name, msg: e.message })); }
  }
  if (!payloads.length) return;
  const busy = toastBusy(t('cv.refBusy'));
  const same = hold(() => c.imgId);
  try {
    const d = await api.canvasAddRefs(c.imgId, { files: payloads.map(p => ({ b64: p.b64 })) });
    busy.close();
    if (!same()) return;      // 换画布了：这一组 refs 是上一张的
    paintRefs(d.refs || []);
  } catch (e) { busy.close(); if (!same()) return; toastErr(t('cv.refFail'), e.message); refreshRefs(); }
}

async function addRefIds(ids) {
  if (!ids.length) return;
  if (c.refMax - c.refList.length <= 0) { toastErr(t('cv.refMax', { n: c.refMax }), t('cv.refFullPick')); return; }
  const same = hold(() => c.imgId);
  try {
    const d = await api.canvasAddRefs(c.imgId, { image_ids: ids.slice(0, c.refMax - c.refList.length) });
    if (!same()) return;
    paintRefs(d.refs || []);
  } catch (e) { if (!same()) return; toastErr(t('cv.refFail'), e.message); refreshRefs(); }
}

/** 从本项目已有的图里挑：加进来的是**复制的一份**，原图后来被删也不影响"这一版参考了哪张" */
function addRefFromProject() {
  const imgs = (store.peek('images') || []).filter(i => i.id !== c.imgId);
  if (!imgs.length) { toastErr(t('cv.noOther'), t('cv.noOtherBody')); return; }
  const m = modal({
    title: t('cv.refPickTitle'),
    body: el('div.refpick', {}, ...imgs.map(i => el('button.refpick__it', {
      type: 'button', onclick: () => { addRefIds([i.id]); m.close('ok'); },
    }, i.thumb_url ? el('img', { src: i.thumb_url, alt: '', loading: 'lazy' }) : el('span.refpick__ph'),
      el('span', { text: i.name, title: i.name })))),
  });
}

/** 历史条目的「回填」：提示词 + 那一版**实际带走**的参考图整套搬回待提交状态 */
async function restoreRow(r) {
  if (!r.prompt) { toastErr(t('cv.noPrompt')); return; }
  c.ta.value = r.prompt;
  syncPrompt();
  rerunFrom = r.id;
  c.compare.hide();
  if (!r.refs?.length) { line(t('cv.readyRerun', { id: r.id })); return; }
  const same = hold(() => c.imgId);
  try {
    const d = await api.canvasAddRefs(c.imgId, { from_result: r.id });
    if (!same()) return;      // 换画布了：参考图与那行提示都不该落在新这张上
    paintRefs(d.refs || []);
    line(t('cv.refsMoved', { id: r.id, n: d.refs.length }));
  } catch (e) {
    if (!same()) return;
    toastErr(t('cv.refsFail'), e.message);
    line(t('cv.refsPartial', { msg: String(e.message || '').slice(0, 60) }), true);
  }
}

/** 成图另存为项目里的新图：要接着涂遮罩就走修图页那一条，画布这张不动 */
async function forkResult(r) {
  if (!r?.final_url) { toastErr(t('ed.noResultYet')); return; }
  if (r.final_dead) { toastErr(t('ed.resultGone'), t('ed.resultGoneBody')); return; }
  const busy = toastBusy(t('ed.copying'));
  try {
    const f = await api.forkResult(r.id);
    await loadProject(c.projectId);
    busy.close();
    go(`/p/${c.projectId}/e/${f.image_id}`);
    toastOk(t('ed.forked'), t('cv.forkedBody', { name: f.name }));
  } catch (e) { busy.close(); toastErr(t('ed.forkFail'), e.message); }
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
  c.count.textContent = t('pm.charCount', { n: cur.length });
}

/* ==================== 保存画稿 ==================== */
/** 空白画布也要存得回去：paint.js 判空时不给 b64，这里自己导一次全透明的 PNG */
async function onSketchSaved({ empty, b64, id }) {
  if (!id || id !== c.imgId) return true;      // 已经换画布了：这批笔迹不属于屏幕上这张
  let data = b64;
  if (empty && !data) {
    try { data = (await encodeMask(c.paper)).b64; } catch { setFlag(t('ed.saveFail'), 'err'); return false; }
  }
  try {
    await api.saveSketch(id, data);
    setFlag(t('cv.saved'), 'ok');
    return true;
  } catch (e) {
    setFlag(t('ed.saveFail'), 'err');
    toastErr(t('cv.saveFail'), e.message);
    return false;
  }
}

/* ==================== 生成 ==================== */
async function submit() {
  if (!c?.imgId || inFlight.has(c.imgId)) return;
  const prompt = (c.ta.value || '').trim();
  if (!prompt) { toastErr(t('cv.emptyPrompt'), t('cv.emptyPromptBody')); return; }
  const imgId = c.imgId;
  const rerunOf = rerunFrom; rerunFrom = null;
  /* 闸门在第一个 await 之前落下。写在 await 之后就等于没设：提交要飞几秒，
     这期间判据读到的还是 false，双击就是两条云端任务、两份钱。
     按钮禁用只挡鼠标，Ctrl+Enter 绕得过来，所以两条路径都看这个集合。 */
  inFlight.add(imgId);
  c.paintGo(true);
  const release = () => {
    inFlight.delete(imgId);
    if (c && c.imgId === imgId) c.paintGo(false);
  };
  try { await c.painter.flush(); } catch (e) { release(); toastErr(t('cv.notFlushed'), e.message); return; }
  let r;
  try { r = await api.canvasGenerate(imgId, { prompt }, rerunOf); }
  catch (e) { release(); toastErr(t('gen.submitFail'), e.message); return; }
  if (!r?.result_id) { release(); toastErr(t('cv.notQueued'), dx(r?.error, r?.error_args) || t('cv.cloudRejected')); return; }
  const nf = c.refList.length;
  line(rerunOf
    ? nf ? t('cv.queuedRerunRefs', { id: r.result_id, from: rerunOf, n: nf }) : t('cv.queuedRerun', { id: r.result_id, from: rerunOf })
    : nf ? t('cv.queuedRefs', { id: r.result_id, n: nf }) : t('cv.queuedPlain', { id: r.result_id }));
  adopt(r.result_id, imgId, {
    onTick: s => { if (c.imgId === imgId) line(s === 'queued' ? t('cv.queuedTick') : t('cv.cloudTick')); },
    onDone: async done => {
      release();
      done?.status === 'done'
        ? line(t('cv.doneLine', { id: done.id }))
        : line(t('cv.failLine', { msg: (dx(done?.error, done?.error_args) || t('ed.unknown')).slice(0, 90) }), true);
      try {
        const d = await api.canvas(imgId);
        if (c.imgId === imgId) syncResults(d.results || []);
      } catch { /* 列表刷新失败不影响这次结果本身 */ }
      done?.status === 'done' ? toastOk(t('cv.doneToast'), `#${done.id}`) : toastErr(t('gen.failTitle'), dx(done?.error, done?.error_args).slice(0, 120));
    },
  });
}

function line(txt, isErr) {
  fill(c.line, txt ? el('span', { class: isErr ? 'err' : '', text: txt }) : null);
}

/* ==================== 成图查看 / 删除 ==================== */
/**
 * 点开一条历史：把成图叠在画布上，与**这一版当时的线稿**分栏拉动对比。
 * 快照功能上线之前的那些版本没有左半边，这时只铺成图并写明原因——
 * 拿"现在的画稿"冒充"当时的画稿"比不展示更坏。
 */
function showResult(r) {
  if (r.status !== 'done' || r.final_dead) {
    toastErr(r.final_dead ? t('cv.resultDead') : t('cv.notYet'), dx(r.error, r.error_args) || t('cv.stillQueued'));
    return;
  }
  c.cmpId = r.id;
  syncResults(c.rows, r.id);
  c.compare.show({
    origUrl: r.sketch_url || null,
    resultUrl: r.final_url,
    title: `#${r.id} · ${(r.prompt || '').slice(0, 46)}${r.prompt && r.prompt.length > 46 ? '…' : ''}`,
    result: r,
    leftLabel: r.sketch_url ? t('cv.leftSketch') : t('cv.noSketch'),
  });
}

/** 用这一版的提示词再改一版：谱系记在下一行上，历史列才看得出这张是从哪一版迭代来的 */
function usePrompt(r) {
  if (!r.prompt) { toastErr(t('cv.noPrompt')); return; }
  c.ta.value = r.prompt;
  syncPrompt();
  rerunFrom = r.id;
  c.compare.hide();
  line(t('cv.readyRerun', { id: r.id }));
}

/** 取回这一版当时的线稿：写回画稿本体，屏幕上回到"提交那一刻我画的东西" */
async function takeSketch(r) {
  const ok = await confirm({
    title: t('cv.sketchTitle', { id: r.id }),
    text: t('cv.sketchText'),
    okLabel: t('cv.sketchBtn'),
  });
  if (!ok) return;
  const busy = toastBusy(t('cv.sketchBusy'));
  const imgId = c.imgId;
  const same = hold(() => c.imgId);
  try {
    /* 先把屏幕上的笔迹冲出去再让服务端写回快照：不冲的话随后那次 load() 会带着"取回前那版"
       的 pending 笔迹 POST 回 /sketch，把刚写回的快照当场盖掉，而提示仍然说"已取回" */
    await c.painter.flush();
    await api.useSketch(imgId, r.id);
    busy.close();
    if (!same()) return;      // 换画布了：这张的快照已经取回，但屏幕上不是它了
    c.compare.hide();
    await load(imgId);
    setFlag(t('cv.sketchDoneFlag'), 'ok');
    toastOk(t('cv.sketchDoneToast'), t('cv.sketchDone', { id: r.id }));
  } catch (e) {
    busy.close();
    toastErr(t('cv.sketchFail'), e.message);
  }
}

async function delResult(r) {
  const imgId = c.imgId;
  const same = hold(() => c.imgId);
  const ok = await confirm({
    title: t('ed.delRecTitle', { id: r.id }),
    text: r.final_url ? t('cv.delRecText') : t('cv.delRecOnly'),
    danger: true, okLabel: t('ed.delRecBtn'),
  });
  if (!ok) return;
  try {
    await api.delResult(r.id);
    const d = await api.canvas(imgId);
    if (!same()) return;      // 删完回来已经不是这张了：别把上一张的历史条画上去
    syncResults(d.results || []);
    toastOk(t('ed.recDeleted'), `#${r.id}`);
  } catch (e) { toastErr(t('ed.delFail'), e.message); }
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
  if (!c.painter.canUndo) { toastErr(t('ed.noUndo')); return; }
  c.painter.undo().then(ok => { if (ok) setFlag(t('ed.undone')); });
}

async function onClear() {
  const ok = await confirm({ title: t('cv.eraseAll'), text: t('cv.clearText'), okLabel: t('ed.tClear') });
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
