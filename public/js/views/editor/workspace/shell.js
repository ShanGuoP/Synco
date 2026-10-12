// 工作区骨架与组件接线；瞬态状态由入口和各组件持有。
import { el } from '../../../core/dom.js';
import { icon } from '../../../core/icons.js';
import { mountWindowControls } from '../../../core/desktop.js';
import { go } from '../../../core/router.js';
import { store, toggleSel, selectWhere, invertSel, clearSel } from '../../../state.js';
import { toastErr } from '../../../ui/toast.js';
import { makeSlider } from '../../../ui/controls.js';
import { helpModal } from '../../../ui/dialogs.js';
import { createPresetMenu } from '../../../ui/presets.js';
import { createViewport } from '../viewport.js';
import { createTileView } from '../tiles.js';
import { createPainter } from '../paint.js';
import { createParams } from '../params.js';
import { createAdjust } from '../adjust.js';
import { createHistory } from '../history.js';
import { createFilmstrip } from '../filmstrip.js';
import { createCompare } from '../compare.js';
import { t } from '../../../core/i18n.js';
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

export function buildEditorShell({
  context, syncZoomPills, restoreFromResult, forkResult, onZoom, setTool, onEdit,
  exportCurrent, toggleDrawer, openBestCompare, goBack, toggleCol,
  showCompare, delResult, doSubmit, stopCurrent, applyMode, applyScope,
  applyPreset, showImage, syncFilm, onMaskSaved, syncInvertHint, forkAdjusted, paintHud,
}) {
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
    context()?.painter?.invalidate?.();   // 缩放变了，光标尺寸/定位缓存作废
    context()?.adjust?.setScale(s);       // 液化盘跟着缩放重定位（手柄尺寸走 CSS 的 --iz）
  } });

  const toolBtn = (k, ico, label, tip, onclick) => el('button.tool', {
    type: 'button', dataset: { k }, 'data-tip': tip, 'aria-label': label, onclick,
  }, el('span.tool__ico', { html: icon(ico) }), el('span', { text: label }));

  const brushVal = el('span.tool-slider__val', { text: '70' });
  const zoomPct = el('span.tool__val', { text: '100%' });
  const brushSlider = makeSlider({
    min: 8, max: 320, step: 2, value: 70, ariaLabel: t('ed.brushAria'),
    onChange: v => { brushVal.textContent = String(v); context().painter?.setBrush(v); },
  });
  const tools = el('div.ed-tools', {},
    el('span.ed-tools__context', { text: t('ed.compare') }),
    el('button.tool-zoom', { type: 'button', 'data-tip': t('ed.zoomTip'), onclick: () => onZoom('fit') }, zoomPct),
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
    onPick: id => { if (id !== context().imgId) showImage(id); },
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
    idOf: () => context()?.imgId || 0,
    infoOf: () => context()?.info,
    onForked: id => forkAdjusted(id),
    brushLocked: msg => { if (msg) toastErr(t('ed.brushLocked'), msg); },
    // 画幅尺寸交给左下角那枚读数：带着裁切/旋转时，屏幕上那张已与源图不同
    onFrame: (w, h) => { if (context()) { context().hudFrame = { w, h }; paintHud(); } },
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
  return el('button.rail-btn', { type: 'button', dataset: { key }, 'aria-label': t(labelKey), 'data-tip': t(labelKey), class: `rail-btn${on ? ' is-on' : ''}`, onclick },
    el('span.ic', { html: icon(ico) }), el('span', { text: t(labelKey) }));
}
