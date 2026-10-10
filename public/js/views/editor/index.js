// 编辑器装配：五区版式 + 工具/视口/绘制/参数/历史/胶片条的接线
'use strict';
import { el, $, $$, fill } from '../../core/dom.js';
import { icon } from '../../core/icons.js';
import { api } from '../../core/api.js';
import { mountWindowControls } from '../../core/desktop.js';
import { go } from '../../core/router.js';
import { store, loadProject, saveSettings, stateOf, toggleSel, selectWhere, invertSel, clearSel, touchImage, diffSettings, defaultsFromCfg, settingsFromPreset, setJob, isCloud, effMode, patchSettings, cloudPrompt } from '../../state.js';
import { submit, adopt, drop } from '../../gen.js';
import { toastOk, toastErr, toastBusy } from '../../ui/toast.js';
import { makeSlider } from '../../ui/controls.js';
import { shortcutsModal, helpModal } from '../../ui/dialogs.js';
import { settingsModal } from '../../ui/settings.js';
import { createPresetMenu } from '../../ui/presets.js';
import { confirm } from '../../ui/modal.js';
import { createViewport } from './viewport.js';
import { createTileView } from './tiles.js';
import { createPainter } from './paint.js';
import { createParams, SEED_MAX } from './params.js';
import { createAdjust } from './adjust.js';
import { createHistory } from './history.js';
import { createFilmstrip } from '../../views/editor/filmstrip.js';
import { createCompare } from './compare.js';
import { fmtFile, fmtDims } from '../../core/format.js';
import { beforeSwitch, dx, t } from '../../core/i18n.js';

let ctx = null;   // 当前编辑器上下文（只建一次，切图复用）
let rerunFrom = null;       // 下一次提交是「哪条结果的重跑」
let lastSubmitted = null;   // 上次提交的参数快照，用来判断面板里是否有未提交改动

const TOOLS = [
  { key: 'pan',   ico: 'hand',   label: 'ed.tPan', tip: 'ed.tPanTip' },
  { key: 'brush', ico: 'brush',  label: 'dlg.dBrush', tip: 'ed.tBrushTip' },
  { key: 'erase', ico: 'eraser', label: 'dlg.dEraser', tip: 'ed.tEraseTip' },
];
const ZOOMS = [
  { key: 'fit',  ico: 'fit',     label: 'ed.tFit', tip: 'ed.tFitTip' },
  { key: 'one',  ico: 'one2one', label: 'ed.tOne', tip: 'ed.tOneTip' },
  { key: 'zin',  ico: 'zoomIn',  label: 'compare.zoomIn', tip: 'ed.tZinTip' },
  { key: 'zout', ico: 'zoomOut', label: 'compare.zoomOut', tip: 'ed.tZoutTip' },
];
const EDITS = [
  { key: 'undo',  ico: 'undo',  label: 'ed.tUndo', tip: 'ed.tUndoTip' },
  { key: 'clear', ico: 'trash', label: 'ed.tClear', tip: 'ed.tClearTip' },
];

/**
 * 涂抹层的长边档位，与底图脱钩。
 * 底图在 321–3072 这一档只有 320 的缩略图（proxy 只在原图超出档位时才切），
 * 跟着底图走就等于把笔迹画在 213×320 的格子上再让服务端放大——边界成块，外扩与羽化全算在粗格子上。
 * 上限就是 proxy 那一档：撤销栈存 8 张同尺寸快照，24MP 满尺寸要 768MB。
 */
const PAINT_EDGE = 3072;

export const isEditorOpen = () => !!ctx && !$('#editor').hidden;

/* ==================== 骨架（只建一次） ==================== */
function buildShell() {
  // 底图 = 一张 proxy 海报 + 可视区的瓦片；浏览器不再持有全分辨率位图
  const poster = el('img.vp__poster', { alt: '', draggable: 'false' });
  const tileBox = el('div.vp__tiles');
  const mask = el('canvas', { id: 'edMask' });
  // 反向涂抹的预览层：朱红铺满、笔迹处开洞（涂住的是要保住的）。小尺寸画布由 CSS 拉伸，
  // 每帧跟着笔迹刷新才付得起——它只是语义提示，不需要边缘精度
  const invert = el('canvas', { class: 'ed-invert', 'aria-hidden': 'true', hidden: true });
  const cursor = el('div.brush-cur');
  const layer = el('div.vp__canvas', {}, poster, tileBox, mask, invert);
  const tiles = createTileView(tileBox);
  const compare = createCompare({ onClose: syncZoomPills, onRestore: restoreFromResult, onFork: forkResult });
  const vp = el('div.vp', {}, layer, cursor, compare.node);

  const hudSize = el('span.pill', { text: '—' });
  const hudFit = el('button.pill', { type: 'button', text: t('compare.fit'), onclick: () => viewport.fit() });
  const hudOne = el('button.pill', { type: 'button', text: '1:1', onclick: () => viewport.one2one() });
  const hudZoom = el('span.pill', { text: '100%' });
  const hint = el('div.ed-hint', { hidden: true });
  const stage = el('div.ed-stage', {},
    vp, hint,
    el('div.ed-hud.ed-hud--bl', {}, hudSize),
    el('div.ed-hud.ed-hud--br', {}, hudFit, hudOne, hudZoom));

  const viewport = createViewport({ stage, layer, onView: v => tiles.sync(v.s, v.tx, v.ty, v.stageW, v.stageH), onScale: s => {
    const pct = `${Math.round(s * 100)}%`;
    zoomPct.textContent = pct;
    hudZoom.textContent = pct;
    syncZoomPills();
    ctx?.painter?.invalidate?.();   // 缩放变了，光标尺寸/定位缓存作废
    ctx?.adjust?.setScale(s);       // 液化盘跟着缩放重定位（手柄尺寸走 CSS 的 --iz）
  } });

  const toolBtn = (k, ico, label, tip, onclick) => el('button.tool', {
    type: 'button', dataset: { k }, 'data-tip': tip, onclick,
  }, el('span.tool__ico', { html: icon(ico) }), el('span', { text: label }));

  const brushVal = el('span.tool-slider__val', { text: '70' });
  const zoomPct = el('span.tool__val', { text: '100%' });
  const brushSlider = makeSlider({
    min: 8, max: 320, step: 2, value: 70, ariaLabel: t('ed.brushAria'),
    onChange: v => { brushVal.textContent = String(v); ctx.painter?.setBrush(v); },
  });
  const tools = el('div.ed-tools', {},
    el('button.tool-zoom', { type: 'button', 'data-tip': t('ed.zoomTip'), onclick: () => viewport.fit() }, zoomPct),
    ...ZOOMS.map(z => toolBtn(z.key, z.ico, t(z.label), t(z.tip), () => onZoom(z.key))),
    el('span.tool-sep'),
    ...TOOLS.map(d => toolBtn(d.key, d.ico, t(d.label), t(d.tip), () => setTool(d.key))),
    el('span.tool-sep'),
    ...EDITS.map(d => toolBtn(d.key, d.ico, t(d.label), t(d.tip), () => onEdit(d.key))),
    el('div.tool-slider', {}, el('span.tool-slider__lab', { text: t('ed.brushLab') }), brushSlider.node, brushVal),
  );

  const fname = el('b.nowrap');
  const fdims = el('span');
  const saveFlag = el('span.saveflag');
  const exportBtn = el('button.btn.btn--primary.btn--sm', { type: 'button', html: icon('download', { cls: 'icon icon--sm' }) + `<span>${t('ed.export')}</span>`, onclick: exportCurrent });
  const topRight = el('div.ed-top__r', {},
    saveFlag,
    el('button.btn.btn--ghost.btn--sm.ed-params-btn', { type: 'button', html: icon('sliders', { cls: 'icon icon--sm' }) + `<span>${t('ed.params')}</span>`, onclick: toggleDrawer }),
    el('button.btn.btn--ghost.btn--sm', { type: 'button', html: icon('compare', { cls: 'icon icon--sm' }) + `<span>${t('ed.compare')}</span>`, onclick: openBestCompare }),
    exportBtn);
  // 顶栏兼作窗口标题区：deep 让空白处都能拖，里面的按钮/输入仍按可点处理不会被吞
  const top = el('div.ed-top', { 'data-tauri-drag-region': 'deep' },
    el('div.ed-top__l', {},
      el('button.btn.btn--ghost.btn--icon.btn--sm', { type: 'button', 'aria-label': t('ed.back'), 'data-tip': t('ed.back'), html: icon('left', { cls: 'icon icon--sm' }), onclick: goBack }),
      el('div.ed-file', {}, fname, fdims)),
    el('div.ed-top__c', {},
      el('div.ed-seg', {},
        el('button', { type: 'button', text: t('crumb.home'), onclick: () => go('/') }),
        el('button', { type: 'button', text: t('crumb.project'), onclick: goBack }),
        el('button.is-on', { type: 'button', text: t('crumb.editor'), disabled: true }))),
    topRight);
  mountWindowControls(topRight);

  const rail = el('div.ed-rail', {},
    railBtn('pre', 'layers', 'ed.railHist', true, () => toggleCol('no-pre', 'pre')),
    railBtn('prop', 'sliders', 'ed.railProp', true, () => toggleCol('no-prop', 'prop')),
    railBtn('cmp', 'compare', 'ed.railCmp', false, openBestCompare),
    el('div', { style: { flex: '1 1 auto' } }),
    railBtn('help', 'book', 'dlg.help', false, helpModal));

  const history = createHistory({ onPick: showCompare, onRestore: restoreFromResult, onFork: forkResult, onDel: delResult });
  const params = createParams({ onSubmit: doSubmit, onStop: stopCurrent, onMode: applyMode, onInk: applyScope });
  /* 预设按钮塞进参数列头部，紧挨重置键，不新增一行 */
  const presets = createPresetMenu({ onApply: applyPreset });
  const hd = params.node.querySelector('.col-hd');
  hd.insertBefore(presets.node, hd.lastElementChild);
  const film = createFilmstrip({
    onPick: id => { if (id !== ctx.imgId) showImage(id); },
    onToggleSel: id => { toggleSel(id); syncFilm(); },
    onSelectMasked: () => { selectWhere(i => i.has_mask); syncFilm(); },
    onInvert: () => { invertSel(); syncFilm(); },
    onClearSel: () => { clearSel(); syncFilm(); },
    onImport: () => { const p = store.peek('project'); if (p) go(`/p/${p.id}`); },
  });

  const painter = createPainter({
    mask, cursor,
    onSaved: onMaskSaved,
    // 画笔模块每描完一帧才回调一次，反向预览跟着它刷新（每帧一次小尺寸 blit，付得起）
    onDirty: () => { setFlag(t('ed.dirtyFlag'), 'busy'); syncInvertHint(); },
  });

  /* 「本地调整」：参数在这一层，像素永远交给服务端算。它要用 params 那条状态行，
     所以必须排在 params 之后；面板那一页再反向挂回 params 的标签条上 */
  const adjust = createAdjust({
    layer, poster, stage, viewport, tiles,
    line: (txt, isErr) => params.line(txt, isErr),
    idOf: () => ctx?.imgId || 0,
    infoOf: () => ctx?.info,
    onForked: id => forkAdjusted(id),
    brushLocked: msg => { if (msg) toastErr(t('ed.brushLocked'), msg); },
    // 画幅尺寸交给左下角那枚读数：带着裁切/旋转时，屏幕上那张已与源图不同
    onFrame: (w, h) => { if (ctx) { ctx.hudFrame = { w, h }; paintHud(); } },
  });
  params.setAdjust(adjust.node, k => { if (k !== 'adjust') adjust.exit(); });

  const setFlag = (txt, kind = '') => {
    saveFlag.textContent = txt;
    saveFlag.className = `saveflag${kind === 'busy' ? ' is-busy' : kind === 'ok' ? ' is-ok' : kind === 'err' ? ' is-err' : ''}`;
  };

  return { top, tools, stage, vp, layer, poster, tiles, mask, invert, cursor, hint, hudSize, hudFit, hudOne,
           viewport, painter, compare, adjust, brushSlider, brushVal, fname, fdims, saveFlag, setFlag,
           history, params, presets, film, rail, exportBtn, zoomPct };
}

function railBtn(key, ico, labelKey, on, onclick) {
  return el('button.rail-btn', { type: 'button', dataset: { key }, class: `rail-btn${on ? ' is-on' : ''}`, onclick },
    el('span.ic', { html: icon(ico) }), el('span', { text: t(labelKey) }));
}

const narrow = () => window.matchMedia('(max-width: 1100px)').matches;

function toggleCol(cls, key) {
  /* 窄屏没有三栏可收，图标轨改当抽屉/浮层开关 */
  if (narrow()) {
    if (key === 'prop') toggleDrawer();
    else if (key === 'pre') toastErr(t('ed.narrowHist'), t('ed.narrowHistBody'));
    return;
  }
  const off = $('#editor').classList.toggle(cls);
  const b = $(`.ed-rail [data-key="${key}"]`);
  if (b) b.classList.toggle('is-on', !off);
}

/** 窄屏：属性面板走底部抽屉 */
function toggleDrawer() {
  const ed = $('#editor');
  const on = ed.classList.toggle('is-drawer');
  ed.classList.remove('no-prop');
  if (on) ed.querySelector('.prop-body')?.scrollTo({ top: 0 });
}

/* ==================== 打开 / 关闭 ==================== */
export async function openEditor(projectId, imgId) {
  const ed = $('#editor');
  if (!ctx) {
    ctx = buildShell();
    for (const part of [ctx.top, ctx.tools, ctx.stage, ctx.history.node, ctx.params.node, ctx.rail, ctx.film.node]) ed.append(part);
    wireKeys(ctx);
  }
  /* 数据到手再掀覆盖层：脏 URL（比如 #/p/undefined/e/3）会在这里抛，此时应留在原页面而不是显示一个空编辑器 */
  const { project, images } = store.get();
  if (!project || project.id !== +projectId || !images.some(i => i.id === +imgId)) {
    try { await loadProject(projectId); }
    catch (e) { toastErr(t('ed.projFail'), e.message || String(e)); go('/'); return; }
  }
  ed.hidden = false;
  ed.classList.remove('is-drawer');
  $('#shell').style.visibility = 'hidden';

  ctx.projectId = +projectId;
  ctx.params.sync(store.peek('settings'));
  await showImage(+imgId);
}

export function closeEditor() {
  $('#editor').hidden = true;
  $('#shell').style.visibility = '';
  /* 不等：路由已经要走了。这笔由画笔模块串行排出去，下一次 showImage 的 load() 会 await 到它 */
  Promise.resolve(ctx?.painter.flush()).catch(() => {});
  /* 调整面板同理：最后一次滑杆改动可能还压在 150ms 防抖里，落库要赶在关窗前发出去 */
  ctx?.adjust.exit();
  Promise.resolve(ctx?.adjust.flush()).catch(() => {});
}

/* 关页/刷新/系统休眠前的最后一搏：防抖里的 700ms 和排队中的保存都得此刻冲出去（能不能落地看网络，至少不静默） */
window.addEventListener('pagehide', () => {
  Promise.resolve(ctx?.painter.flush()).catch(() => {});
  Promise.resolve(ctx?.adjust?.flush()).catch(() => {});
});

/* 换语言走的是"存好 → 重载"，pagehide 那条只能尽力（异步保存赶得上卸载就输了），
   所以这里给一次真正 await 的机会：注册制，设置弹窗不认识编辑器内部 */
beforeSwitch(async () => {
  if (!ctx) return;
  await Promise.resolve(ctx.painter.flush());
  await Promise.resolve(ctx.adjust.flush());
});

const goBack = () => { const p = store.peek('project'); go(p ? `/p/${p.id}` : '/'); };

/* ==================== 单图装载 ==================== */
async function showImage(imgId) {
  const row = store.peek('images').find(i => i.id === imgId);
  if (!row) { toastErr(t('ed.notInProject')); return; }
  /* 连点胶片条会并发跑好几次装载：认领 imgId 之后还有三个 await，
     晚到的旧响应必须能认出自己已经过期，否则它会把画布、海报与 painter 的归属写成上一张 */
  const seq = (ctx.showSeq = (ctx.showSeq || 0) + 1);
  const stale = () => seq !== ctx.showSeq;
  ctx.imgId = imgId;

  const busy = toastBusy(t('ed.loadingImage'));
  let info;
  try { info = await api.image(imgId); } catch (e) { busy.close(); toastErr(t('ed.loadFail'), e.message); return; }
  busy.close();
  if (stale()) return;

  ctx.info = info;
  ctx.hudFrame = null;      // 上一张的画幅读数不能跟着换图留下（adjust.onImage 会按这一张重新报）
  ctx.fname.textContent = fmtFile(info.name, 30);
  ctx.fname.title = info.name;
  ctx.fdims.textContent = fmtDims(info.w, info.h);
  ctx.params.setCloud(isCloud());
  paintHud();
  /* 库里有过这行 ≠ 盘上还有这个文件（清过 data/projects、手工删过图都会留下指向空气的记录） */
  ctx.setFlag(info.orig_dead ? t('film.lostFile') : '', info.orig_dead ? 'err' : '');

  // 底图先换掉：切图时旧照片不该还贴在屏幕上，瓦片清单随后异步补
  ctx.tiles.abort();
  if (info.orig_dead) ctx.poster.removeAttribute('src');
  else ctx.poster.src = info.proxy_url || info.thumb_url || info.orig_url;
  ctx.poster.hidden = !!info.orig_dead;
  if (!ctx.poster.hidden) await new Promise(res => {
    const done = () => res();
    ctx.poster.onload = done;
    ctx.poster.onerror = () => { toastErr(t('ed.decodeFail'), info.name); done(); };
    if (ctx.poster.complete && ctx.poster.naturalWidth) done();
  });
  if (stale()) return;

  ctx.viewport.setContentSize(info.w, info.h);
  // 涂抹层按自己的档位走（见 PAINT_EDGE），不再跟着底图：底图可能只是一张 320 的缩略图，
  // 跟着它就把笔迹锁死在粗格子上，边界成块、外扩与羽化全算在粗格子上。上采样到原图尺寸是服务端的事。
  // 原图文件已丢失时底图 naturalWidth 是 0，过去会退化成按库里的报称尺寸开满尺寸缓冲（24MP 的撤销栈≈768MB）
  const s = Math.min(1, PAINT_EDGE / Math.max(1, info.w, info.h));
  const paint = { w: Math.max(1, Math.round(info.w * s)), h: Math.max(1, Math.round(info.h * s)) };
  const had = await ctx.painter.load(info.w, info.h, info.mask_url, imgId, paint);
  if (stale()) return;
  /* 「本地调整」要在这一步把底图换成库里那套参数渲染出来的预览（如果有参数）。
     它排在 fit 之前：裁切/旋转会换画幅尺寸，先定尺寸再适应窗口才不会再歪一次。
     返回 true = 这一张显示的是调整预览，源图瓦片就不该再去摆 */
  const adjustedView = await ctx.adjust.onImage(info);
  if (stale()) return;
  syncInvertHint();
  ctx.viewport.fit();
  showHint(!had && !info.orig_dead);
  setTool('brush');
  // 瓦片是懒切的，首次进去可能要等它几秒；海报先到，画面不会空
  if (!info.orig_dead && info.tiles_url && !adjustedView) {
    api.tiles(imgId).then(m => {
      if (ctx.imgId !== imgId) return;
      ctx.tiles.setMeta(m);
      ctx.viewport.refresh();
    }).catch(() => { /* 没有瓦片就停在海报上，一样能涂能看 */ });
  }

  ctx.history.setResults(info.results, info.last?.status === 'done' ? info.last.id : null);
  ctx.compare.hide();
  /* 从项目页的派生弹窗点进来：把选中的那条摆到对比层上（弹窗只做看与删，回填走这里）。
     必须排在 compare.hide() 之后，否则刚开的对比层会被上面那句关掉 */
  const intent = store.peek('intent');
  if (intent?.focusResult) {
    store.set({ intent: null }, 'intent');
    const hit = info.results.find(x => x.id === intent.focusResult)
      || (await api.result(intent.focusResult).catch(() => null));
    if (stale()) return;
    if (hit?.status === 'done' && !hit.final_dead) {
      ctx.history.setResults(info.results, hit.id);
      showCompare(hit);
    } else {
      toastErr(t('ed.resultBlind'), hit ? (dx(hit.error, hit.error_args) || t('ed.resultBlindWhy')).slice(0, 120) : t('ed.recordGone'));
    }
  }
  ctx.params.stages.reset();
  ctx.params.setBusy(false);
  ctx.params.clearMarks();
  ctx.params.line(had ? t('ed.hasMask') : '');
  rerunFrom = null;
  lastSubmitted = null;

  /* 库里有仍在排或仍在跑的任务（页面刷新或切走过）→ 接回轮询。
     云端行现在也由服务端推进，所以两条链路都要接 */
  const resumable = info.results.filter(r => r.status === 'running' || r.status === 'queued');
  if (resumable.length) {
    ctx.params.setBusy(true);
    for (const key of ['submit', 'queue']) ctx.params.stages.set(key, 'done');
    ctx.params.stages.set('sample', 'run');
    ctx.params.line(t('ed.rejoin'));
    for (const r of resumable) adopt(r.id, imgId, {
      /* 轮询回来时可能已经切图：这一张的阶段条只在这张还是当前图时改 */
      onTick: s => { if (ctx.imgId === imgId && s !== 'running' && s !== 'queued') ctx.params.stages.set('sample', s === 'done' ? 'done' : 'err'); },
      onDone: done => onJobSettled(done),
    });
  }

  syncFilm();
  syncExport();
}

const showHint = on => {
  ctx.hint.hidden = !on;
  if (on) ctx.hint.innerHTML = `${icon('brush', { cls: 'icon icon--sm' })}<span>${t('ed.noMaskHint')}</span>`;
};

/** 左下角那条 HUD：两条出图路的口径不一样，切换之后必须重写，不能停在装载时那一句 */
function paintHud() {
  const info = ctx?.info;
  if (!info) return;
  // 摆的是哪一版就说哪一版的尺寸：带着裁切或旋转时，画幅已与源图不同
  const d = ctx.hudFrame || info;
  const inv = isCloud() && !!store.peek('settings')?.invert;
  ctx.hudSize.textContent = inv
    ? t('ed.dimsA', { wh: `${d.w}×${d.h}` })
    : isCloud()
      ? t('ed.dimsB', { wh: `${d.w}×${d.h}` })
      : t('ed.dimsC', { wh: `${d.w}×${d.h}` });
}

/* ---------------- 反向涂抹（只在这条路上开） ---------------- */
const HINT_W = 512;
let spotCache = '';

/** 画布要的是字面色值，CSS 变量拿不到就直接问 computed style，退档用朱红的默认值 */
function spotColor() {
  if (!spotCache) {
    spotCache = getComputedStyle(document.documentElement).getPropertyValue('--spot').trim() || '#c72c2c';
  }
  return spotCache;
}

function invertOn() {
  const s = store.peek('settings') || {};
  // 整张重绘根本不看遮罩，这时候铺朱红预览是在说一件不会发生的事
  return isCloud() && !!s.invert && !s.full;
}

/** 朱红铺满整幅，再用笔迹按 alpha 挖洞：剩下的红就是"会被重绘的地方" */
function syncInvertHint() {
  if (!ctx?.mask) return;
  const on = invertOn();
  ctx.stage.classList.toggle('is-invert', on);
  ctx.invert.hidden = !on;
  if (!on) return;
  const m = ctx.mask;
  const w = HINT_W;
  const h = Math.max(1, Math.round((HINT_W * m.height) / Math.max(1, m.width)));
  const c = ctx.invert;
  if (c.width !== w || c.height !== h) { c.width = w; c.height = h; }
  const x = c.getContext('2d');
  x.globalCompositeOperation = 'source-over';
  x.clearRect(0, 0, w, h);
  x.fillStyle = spotColor();
  x.fillRect(0, 0, w, h);
  x.globalCompositeOperation = 'destination-out';
  x.drawImage(m, 0, 0, w, h);
  x.globalCompositeOperation = 'source-over';
}

/** 涂要改的 / 涂要保留的 / 整张重绘：换的是这一枪的语义，遮罩文件本身不动 */
function applyScope(mode) {
  if (!ctx?.imgId) return;
  const s = store.peek('settings') || {};
  const next = { invert: mode === 'keep', full: mode === 'full' };
  if (!!s.invert === next.invert && !!s.full === next.full) return;
  patchSettings(next);
  saveSettings().catch(() => { /* 参数写库失败不阻断这次编辑 */ });
  ctx.params.sync(store.peek('settings'));
  ctx.params.setScope();
  paintHud();
  syncInvertHint();
  ctx.params.line(mode === 'full'
    ? t('ed.modeFullNote')
    : mode === 'keep'
      ? t('ed.modeKeepNote')
      : t('ed.modePartNote'));
}

/**
 * 就地切本机 / 云端。模式记在**这个项目**的参数里（不是全局设置），
 * 所以"这个项目的活交给云端、下一个还走本机"是各记各的；全局那一栏是没选过时的默认值。
 */
function applyMode(next) {
  if (!ctx?.imgId) return;
  if (next !== 'cloud' && next !== 'comfyui') return;
  if (next === effMode()) return;
  // 反向涂抹只在这条路上成立；切回本机就把它关掉，别让面板上留着一个不会生效的开关
  patchSettings(next === 'comfyui' ? { mode: next, invert: false } : { mode: next });
  // 写完就落库：切了模式不提交、下次进这个项目还应该是刚才那一个
  saveSettings().catch(() => { /* 参数写库失败不阻断这次编辑 */ });
  ctx.params.setCloud(isCloud());
  paintHud();
  ctx.params.line(next === 'cloud'
    ? t('ed.switchedCloud')
    : t('ed.switchedLocal'));
}

/* ==================== 工具 / 缩放 ==================== */
function setTool(k) {
  /* 裁切/旋转换了画幅，遮罩那一层还是按源图坐标系存的：这时涂上去的笔迹会歪。
     与其静默错位，不如挡住并说清楚出口（先「应用为新图」，新图的源图就是那张裁好的） */
  if ((k === 'brush' || k === 'erase') && ctx.adjust?.geometryActive) {
    toastErr(t('ed.brushLocked'), t('ed.adjustLocked'));
    return;
  }
  if (k === 'brush' || k === 'erase') ctx.adjust?.exit();   // 交回画笔：裁切框与液化盘让位
  ctx.tool = k;
  for (const b of $$('.ed-tools .tool[data-k]')) b.classList.toggle('is-on', b.dataset.k === k);
  ctx.viewport.setMode(k === 'pan' ? 'pan' : 'paint');
  ctx.painter.setTool(k === 'erase' ? 'erase' : 'brush');
  ctx.cursor.style.display = 'none';
}

function onZoom(k) {
  ({ fit: () => ctx.viewport.fit(), one: () => ctx.viewport.one2one(),
     zin: () => ctx.viewport.zoomBy(1.35), zout: () => ctx.viewport.zoomBy(1 / 1.35) })[k]?.();
}

function onEdit(k) {
  if (k === 'undo') {
    if (!ctx.painter.canUndo) { toastErr(t('ed.noUndo')); return; }
    ctx.painter.undo().then(ok => { if (ok) { ctx.setFlag(t('ed.undone')); syncInvertHint(); } });
  } else if (k === 'clear') {
    ctx.painter.clear();
    syncInvertHint();
    showHint(true);
  }
}

function syncZoomPills() {
  if (!ctx) return;
  const fit = ctx.viewport.isFit;
  ctx.hudFit.classList.toggle('is-on', fit);
  ctx.hudOne.classList.toggle('is-on', !fit && ctx.viewport.zoomPct === 100);
}

/* ==================== 遮罩保存 ==================== */
/** id 由画笔模块随笔迹一起交回来：await 之后再读 ctx.imgId，切过图就会把旧图的笔迹写进新图的遮罩文件 */
async function onMaskSaved({ empty, b64, id }) {
  if (!id) return true;
  const here = id === ctx.imgId;                 // 这批笔迹是否还属于屏幕上这张
  if (here) ctx.setFlag(t('ed.saving'), 'busy');
  try {
    await api.saveMask(id, b64);
    touchImage(id, { has_mask: !empty });
    if (here) {
      ctx.setFlag(empty ? t('ed.maskCleared') : t('ed.maskSaved'), 'ok');
      if (!empty) ctx.hint.hidden = true;
    }
    syncFilm();
    return true;
  } catch (e) {
    if (here) ctx.setFlag(t('ed.saveFail'), 'err');
    toastErr(t('ed.maskSaveFail'), e.message);
    return false;                                // 画笔模块据此把这批笔迹重新标脏并排一次重试
  }
}

/* ==================== 提交生成 ==================== */
const parseSettings = r => { try { return JSON.parse(r.settings_json || 'null'); } catch { return null; } };

/** 一条结果用什么名义展示：云端没有步数/CFG/种子，别说本机那套话（批量那条的快照里没有 cloud 节） */
const resultTag = r => {
  const c = parseSettings(r)?.cloud;
  return r.backend === 'cloud'
    ? [c?.model, c?.quality, c?.size].filter(Boolean).join(' · ') || t('pm.modeCloud')
    : t('ed.resultTag', { steps: r.steps, cfg: r.cfg, seed: r.seed });
};

/** 套用预设：LoRA 往当前工作流的骨架上贴，本机没有的置灰 */
function applyPreset(p) {
  const cur = store.peek('settings') || {};
  if (isCloud()) {
    /* 云端只吃指令：预设里的步数 / CFG / LoRA 在这条线上没有对应参数 */
    const next = { prompt: p.prompt || '', negative: p.negative || '' };
    rerunFrom = null;
    ctx.params.applySettings(next, diffSettings(cur, next));
    ctx.params.line(t('ed.appliedCloudPreset', { name: p.name }));
    return;
  }
  const next = settingsFromPreset(p, cur, (store.peek('cfg') || {}).loras);
  const missing = next.loras.filter(l => l.missing);
  rerunFrom = null;
  ctx.params.applySettings(next, diffSettings(cur, next));
  ctx.params.line(missing.length
    ? t('ed.appliedPresetMissing', { name: p.name, n: missing.length })
    : t('ed.appliedPreset', { name: p.name }));
  if (missing.length) toastErr(t('ed.loraPartial'), missing.map(l => l.name).join(t('settings.sepList')));
}

/** 把某条成图复制成项目里的一张新图，并跳过去在它上面涂 */
/** 「本地调整 → 应用为新图」之后：项目要重取（多了一行），并跳到那张新图上 */
async function forkAdjusted(imageId) {
  if (!imageId) return;
  const busy = toastBusy(t('ed.registering'));
  try {
    await loadProject(ctx.projectId);
    busy.close();
    go(`/p/${ctx.projectId}/e/${imageId}`);
  } catch (e) { busy.close(); toastErr(t('ed.newImageFail'), e.message); }
}

async function forkResult(r) {
  if (!r?.final_url) { toastErr(t('ed.noResultYet')); return; }
  if (r.final_dead) { toastErr(t('ed.resultGone'), t('ed.resultGoneBody')); return; }
  const busy = toastBusy(t('ed.copying'));
  try {
    const f = await api.forkResult(r.id);
    await loadProject(ctx.projectId);
    busy.close();
    go(`/p/${ctx.projectId}/e/${f.image_id}`);
    toastOk(t('ed.forked'), t('ed.forkedBody', { name: f.name }));
  } catch (e) { busy.close(); toastErr(t('ed.forkFail'), e.message); }
}

/** 删一条生成记录：连着它的成图文件一起删，原图与遮罩不动 */
async function delResult(r) {
  const ok = await confirm({
    title: t('ed.delRecTitle', { id: r.id }),
    text: r.final_url ? t('ed.delRecText') : t('ed.delRecOnly'),
    danger: true, okLabel: t('ed.delRecBtn'),
  });
  if (!ok) return;
  try {
    await api.delResult(r.id);
    if (ctx.cmpId === r.id) ctx.compare.hide();
    const info = await api.image(ctx.imgId);
    ctx.info = info;
    ctx.history.setResults(info.results, info.last?.status === 'done' ? info.last.id : null);
    syncExport();
    toastOk(t('ed.recDeleted'), `#${r.id}`);
  } catch (e) { toastErr(t('ed.delFail'), e.message); }
}

/** 回填是"接着改"的起点：对比层还开着、工具还停在抓手就涂不了，退回画布并切到画笔 */
function backToCanvas() {
  ctx.compare.hide();
  setTool('brush');
  // 历史行带着当初的 invert（按行存的），回填后预览层要立刻站到同一边
  syncInvertHint();
  paintHud();
}

/** 把某条结果当时的参数搬回面板，等用户自己点提交；种子默认沿用 */
async function restoreFromResult(r) {
  const s = parseSettings(r);
  if (!s) { toastErr(t('ed.noParams'), t('ed.noParamsWhy')); return; }
  const cur = store.peek('settings') || {};
  const dirty = lastSubmitted && diffSettings(lastSubmitted, cur).length;
  if (dirty) {
    const ok = await confirm({
      title: t('ed.restoreTitle'),
      text: t('ed.restoreText', { id: r.id }),
      okLabel: t('ed.restoreBtn'),
    });
    if (!ok) return;
  }
  /* 云端记录没有步数/LoRA/种子可回填，正向里还并进了当初的负面，所以只搬指令、不动本地那几行。
     注意必须把 steps/cfg/loras 按**本机这一路**的合法值重取：云端行是 0 步 0 CFG 的形状，
     整份搬进面板后切回本机再点提交，就把 0 直接送进了 KSampler。 */
  if (r.backend === 'cloud') {
    const d = defaultsFromCfg(store.peek('cfg') || {});
    const legal = (v, lo, hi) => (Number(v) >= lo && Number(v) <= hi ? Number(v) : null);
    const next = {
      ...s,
      prompt: s.prompt || '',
      negative: cur.negative || d.negative,
      steps: legal(cur.steps, 4, 60) ?? d.steps,
      cfg: legal(cur.cfg, 0.5, 14) ?? d.cfg,
      loras: (cur.loras || []).length ? cur.loras : d.loras,
      seed: cur.seed || 0,
      randomSeed: cur.randomSeed !== false,
    };
    rerunFrom = r.id;
    ctx.params.applySettings(next, [...diffSettings(cur, next), 'prompt']);
    ctx.params.line(t('ed.restoredCloud', { id: r.id }));
    toastOk(t('ed.restored'), t('ed.restoredBody', { tag: resultTag(r) }));
    backToCanvas();
    return;
  }
  /* 提交时后端按「面板种子 + 图片序号」取模下发，回填要先把这张图的序号减回去，
     否则重跑用的是另一个种子，"改自 #id" 的复现承诺就破了。 */
  const raw = Number(r.seed ?? s.seed ?? 0);
  const base = (((raw - ctx.imgId) % SEED_MAX) + SEED_MAX) % SEED_MAX;
  const next = { ...s, seed: base, randomSeed: false };
  rerunFrom = r.id;
  ctx.params.applySettings(next, [...diffSettings(cur, next), 'seed', 'randomSeed']);
  ctx.params.line(t('ed.restoredWf', { id: r.id }));
  toastOk(t('ed.restored'), (base + ctx.imgId) % SEED_MAX === raw
    ? t('ed.seedKept', { id: r.id, seed: base })
    : t('ed.seedOut', { id: r.id }));
  backToCanvas();
}

/* ==================== 云端局部重绘 ==================== */

/**
 * 云端 = 裁切-缝合，但整条链路在服务端一次做完：
 * 浏览器只交一个 image_id 与提示词，回来的是库里这一行的进度，不再有二跳回传。
 */
async function doSubmitCloud() {
  const c = store.peek('cloud') || {};
  if (!c.key_saved) { toastErr(t('ed.cloudOff'), t('ed.cloudOffBody')); return; }
  const imgId = ctx.imgId;
  /* 先 flush 再判有没有遮罩：刚涂完 700ms 内点提交，库里的 has_mask 还是 false，
     按原顺序会被"还没有遮罩"打回一次，用户看到的是按钮失灵 */
  await ctx.painter.flush();
  const s = store.peek('settings');
  const full = !!s?.full;
  // 反向与整张重绘都不要求涂过：前者"没涂=整幅"，后者根本不看遮罩；正向才要求先涂出区域
  if (!full && !s?.invert && !store.peek('images').find(i => i.id === imgId)?.has_mask) {
    toastErr(t('ed.noMask'), t('ed.noMaskBody'));
    return;
  }
  const st = ctx.params.stages;
  let cur = 'submit';
  /* 等模型的几十秒里可能已经切图：面板/阶段条是别人家的了，别再往上画 */
  const here = () => ctx.imgId === imgId;
  /* 阶段条是"这一张"的面板：切了图就别再往上画，只推进游标（与本机链路同一口径） */
  const advance = (key, msg) => {
    if (!here()) { cur = key; return; }
    st.set(cur, 'done'); cur = key; st.set(key, 'run'); if (msg) ctx.params.line(msg);
  };
  const fail = msg => {
    const text = String(msg || t('ed.noReason')).slice(0, 200);
    if (here()) {
      st.set(cur, 'err');
      ctx.params.setBusy(false);
      ctx.params.line(text);
    }
    setJob(imgId, { state: 'err', error: text });
    toastErr(t('ed.cloudFail'), text.slice(0, 140));
  };

  st.reset();
  st.set('submit', 'run');
  ctx.params.setBusy(true);
  ctx.params.line(full ? t('ed.queuingFull') : t('ed.queuingCrop'));
  ctx.compare.hide();
  await saveSettings();
  lastSubmitted = { ...s };
  const rerunOf = rerunFrom; rerunFrom = null;

  let r;
  try {
    r = await api.cloudEdit({
      image_id: imgId, rerun_of: rerunOf,
      settings: {
        prompt: cloudPrompt(s), negative: '', steps: 0, cfg: 0, loras: [],
        // 尺寸胶囊选出来的长边交给服务端按这一行执行（0/空 = 用设置里填的 stitch_edge）
        edge: Number(s.edge) || 0,
        // 反向标志按行存进 results.settings_json：历史列回放才认得出"这张是反向生成的"
        invert: !!s.invert,
        // 整张重绘同样按行存：它连遮罩都不读，回来也不缝合
        full,
        cloud: { model: c.model, quality: c.quality },
      },
    });
  } catch (e) { fail(e.message || e); return; }
  if (!r?.result_id) { fail(dx(r?.error, r?.error_args) || t('ed.cloudBadReply')); return; }

  setJob(imgId, { state: 'run', resultId: r.result_id, error: null });
  advance('sample', full ? t('ed.cloudGen', { id: r.result_id }) : t('ed.cloudCrop', { id: r.result_id }));
  // 之后与本机链路同一套：轮库里这一行，缝合与落盘都在服务端推进
  adopt(r.result_id, imgId, {
    onTick: status => {
      if (!here()) return;
      if (status === 'running' || status === 'queued') st.set('sample', 'run');
      else if (status === 'done') { if (full) st.set('sample', 'done'); else advance('stitch', t('ed.serverStitch')); }
      else if (status === 'error') { st.set(cur, 'err'); }
    },
    onDone: done => {
      if (done?.status === 'done' && !full) advance('stitch');
      onJobSettled(done);
    },
  });
}

async function doSubmit() {
  if (!ctx?.imgId) return;
  if (ctx.info?.orig_dead) { toastErr(t('ed.origGone'), t('ed.origGoneBody')); return; }
  /* 提交的是"调整后"那张（服务端在链里先算参数再走裁切缝合）。几何段动了画幅时
     遮罩与画面不同域，这一步必须挡住，不然发出去的是错位的一版 */
  if (ctx.adjust?.geometryActive) { toastErr(t('ed.geomPending'), t('ed.geomPendingBody')); return; }
  await ctx.adjust.flush();          // 面板里最后一次改动还没发出去就先补上，参数与预览要一致
  if (isCloud()) return doSubmitCloud();
  // 整张重绘只做了云端那一条：本机这条要整图重绘得换一张没有裁切/缝合的工作流，别悄悄改用局部重绘去跑
  if (store.peek('settings')?.full) {
    toastErr(t('ed.fullNeedsCloud'), t('ed.fullNeedsCloudBody'));
    return;
  }
  const imgId = ctx.imgId;
  await ctx.painter.flush();          // 同上：先落笔迹，再判这张有没有遮罩
  const img = store.peek('images').find(i => i.id === imgId);
  if (!img?.has_mask) { toastErr(t('ed.noMask'), t('ed.noMaskLocalBody')); return; }

  const settings = store.peek('settings');
  await saveSettings();
  lastSubmitted = { ...settings };
  const rerunOf = rerunFrom; rerunFrom = null;
  const st = ctx.params.stages;
  let cur = 'submit';                 // 当前推进到哪一段，出错时就标红这一段
  /* 采样那几十秒里可能已经切图：阶段条与提示语是"这张图"的，别画到别人家的面板上 */
  const mine = () => ctx.imgId === imgId;
  const advance = (key, msg) => {
    if (!mine()) { cur = key; return; }      // 已经切走：只推进游标，不动别人家面板上的阶段条
    st.set(cur, 'done'); cur = key; st.set(key, 'run'); if (msg) ctx.params.line(msg);
  };

  st.reset();
  st.set('submit', 'run');
  ctx.params.setBusy(true);
  ctx.params.line(t('ed.uploading'));
  ctx.compare.hide();

  const r = await submit([ctx.imgId], settings, {
    quiet: true,
    rerunOf,
    onTick: status => {
      if (status === 'running') advance('sample', t('ed.comfySampling'));
      else if (status === 'error') { if (mine()) { st.set(cur, 'err'); ctx.params.line(t('ed.comfyError'), true); } }
      else advance('stitch', t('ed.returning'));
    },
    onDone: status => onJobSettled(status),
  });

  if (!r.ok) {
    if (mine()) {
      st.set(cur, 'err');
      ctx.params.setBusy(false);
      ctx.params.line(dx(r.error, r.error_args) || t('gen.noneAccepted'), true);
    }
    return;
  }
  advance('queue', t('ed.queued'));
}

/** 任务终态 → 刷新历史并自动进入对比 */
async function onJobSettled(r) {
  if (!ctx?.imgId) return;
  /* 后台任务完成时你可能已经在看另一张：别把它的成图弹到你当前这张上 */
  if (r?.image_id && r.image_id !== ctx.imgId) return;
  ctx.params.setBusy(false);
  // 带得上原因就直接说原因：轮询放弃那两条出口（记录没了、连续读不到）并没有右下角通知可看
  if (r.status !== 'done') { ctx.params.line(r.error ? dx(r.error, r.error_args).slice(0, 120) : t('ed.genFailNote'), true); return; }
  ctx.params.stages.finish(true);
  ctx.params.line(t('ed.doneDrag'));
  try {
    const info = await api.image(ctx.imgId);
    ctx.info = info;
    ctx.history.setResults(info.results, info.last?.id);
    if (info.last?.status === 'done') showCompare(info.last);
    syncExport();
  } catch { /* 刷新失败不影响已完成的生成 */ }
}

/**
 * 中断这一张正在跑的任务。
 * 本机：给 ComfyUI 发 /interrupt 并把它从队列移走；云端那次请求在对面跑着撤不回来，
 * 只把记录与界面解开，结果回来时 adopt 会因为不再是 running 而被拒。
 */
async function stopCurrent() {
  const imgId = ctx?.imgId;
  if (!imgId) return;
  const rid = store.peek('jobs')[imgId]?.resultId;
  if (!rid) {
    toastErr(t('ed.stopCloudNo'), t('ed.stopCloudNoBody'));
    return;
  }
  const ok = await confirm({
    title: t('ed.stopTitle', { id: rid }),
    text: isCloud() ? t('ed.stopCloudText')
                    : t('ed.stopLocalText'),
    okLabel: t('ed.stop'), danger: true,
  });
  if (!ok) return;
  try {
    await api.interruptResult(rid);
    drop(rid);                                   // 别再轮询它，否则下一轮又把状态刷回 running
    setJob(imgId, { state: 'err', resultId: rid, error: t('ed.stoppedByUser') });
    if (ctx.imgId === imgId) {
      ctx.params.setBusy(false);
      ctx.params.stages.finish(false);
      ctx.params.line(t('ed.interrupted'));
      try {
        const info = await api.image(imgId);
        ctx.info = info;
        ctx.history.setResults(info.results, null);
        syncExport();
      } catch { /* 列表刷新失败不影响已中断 */ }
    }
    toastOk(t('ed.stopped'), `#${rid}`);
  } catch (e) { toastErr(t('ed.stopFail'), e.message); }
}

function showCompare(r) {
  if (!r?.final_url) { toastErr(t('ed.noResultYet')); return; }
  if (r.final_dead) { toastErr(t('ed.resultGone'), t('ed.resultGoneShort')); return; }
  if (ctx.info?.orig_dead) { toastErr(t('film.lostFile'), t('ed.compareNeedsOrig')); return; }
  ctx.cmpId = r.id;
  ctx.compare.show({
    // 对比看的是 proxy 档：两张 20–33MB 的 PNG 一起解码是"点大图卡"的直接来源
    origUrl: ctx.info.proxy_url || ctx.info.orig_url,
    resultUrl: r.final_url,
    cropUrl: r.crop_url,
    overlayUrl: r.maskoverlay_url,
    title: `#${r.id} · ${resultTag(r)}`,
    result: r,
  });
}

/* 「导出」「对比」「下载」找的是同一条：最新一张文件还在盘上的成图 */
const aliveDone = () => (ctx.info?.results || []).find(r => r.status === 'done' && !r.final_dead);

function openBestCompare() {
  const done = aliveDone();
  if (!done) { toastErr(t('ed.noResult'), t('ed.noResultBody')); return; }
  showCompare(done);
}

/** 导出：成图直接复制到设置里的本机文件夹；没配目录就带去设置页 */
async function exportCurrent() {
  const done = aliveDone();
  if (!done) { toastErr(t('ed.noExport')); return; }
  const busy = toastBusy(t('ed.exporting'));
  try {
    const r = await api.exportRun([done.id]);
    busy.close();
    const one = r.files?.[0];
    if (!one || one.skipped) { toastErr(t('ed.exportFail'), one?.reason || t('ed.unknown')); return; }
    toastOk(t('ed.exported'), `${r.dir}\\${one.file}`);
  } catch (e) {
    busy.close();
    const m = String(e.message || e);
    if (/导出目录/.test(m)) {
      toastErr(t('ed.exportUnset'), t('ed.exportUnsetBody'));
      settingsModal('export');
    } else toastErr(t('ed.exportFail'), m);
  }
}

function syncExport() {
  ctx.exportBtn.disabled = !aliveDone();
}

/* ==================== 胶片条 ==================== */
function syncFilm() {
  const { images, sel } = store.get();
  ctx.film.sync({ images, curId: ctx.imgId, sel, stateOf });
}

/* ==================== 快捷键 ==================== */
function wireKeys(c) {
  const keydown = e => {
    if ($('#editor').hidden) return;
    const tag = document.activeElement?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA') {
      if (e.key === 'Escape') document.activeElement.blur();
      return;
    }
    /* 对比层开着时，缩放类按键归对比层，别去动背后那张画布 */
    const vp = c.compare.isOn ? c.compare : c.viewport;
    if (e.code === 'Space') { e.preventDefault(); vp.setSpace(true); return; }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z') { e.preventDefault(); onEdit('undo'); return; }
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
      /* .is-busy 只挡鼠标点击，键盘绕得过去：生成中再按就是多排一条任务、多烧一次云端额度 */
      if (c.params.busy) { toastErr(t('ed.busySwitch'), t('ed.busySwitchBody')); return; }
      e.preventDefault(); doSubmit(); return;
    }
    const k = e.key.toLowerCase();
    if (k === 'b' && !c.compare.isOn) setTool('brush');
    else if (k === 'e' && !c.compare.isOn) setTool('erase');
    else if (k === 'h' && !c.compare.isOn) setTool('pan');
    else if (k === '0') vp.fit();
    else if (k === '1') vp.one2one();
    else if (k === '+' || k === '=') vp.zoomBy(1.3);
    else if (k === '-') vp.zoomBy(1 / 1.3);
    else if (k === '?') shortcutsModal();
    else if (e.key === 'Escape' && c.compare.isOn) c.compare.hide();
  };
  const keyup = e => { if (e.code === 'Space') (c.compare.isOn ? c.compare : c.viewport).setSpace(false); };
  window.addEventListener('keydown', keydown);
  window.addEventListener('keyup', keyup);
}
