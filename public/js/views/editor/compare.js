// 结果对比：两层图叠加 + 中缝拖拽 + 下载入口。
// 修图页拿它比"原图 ↔ 成图"，画布拿它比"当时那版线稿 ↔ 成图"——
// 两边共用一个组件，措辞与动作由调用方给，别再抄第二份中缝拖拽的数学。
'use strict';
import { el, fill, clamp } from '../../core/dom.js';
import { icon } from '../../core/icons.js';
import { createViewport } from './viewport.js';
import { t } from '../../core/i18n.js';

/* 取用时才查字典：模块在 boot 装载字典之前就求值了，写在这里会是 ⟨键名⟩ */
const defaults = () => ({
  left: t('compare.left'), right: t('compare.right'),
  restore: t('compare.restore'), restoreTip: t('compare.restoreTip'),
  useSketch: null, useSketchTip: null,
  overOnWhite: false,
});

export function createCompare({ onClose, onRestore, onFork, onUseSketch, copy }) {
  const T = { ...defaults(), ...(copy || {}) };
  const top = el('div.cmp__top');
  const line = el('div.cmp__line', {}, el('span.cmp__knob', { html: icon('compare', { cls: 'icon icon--sm' }) }));
  const base = el('img', { alt: T.right, draggable: 'false' });
  const over = el('img', { alt: T.left, draggable: 'false', style: T.overOnWhite ? { background: '#fff' } : null });
  top.append(over);

  const box = el('div.cmp__box', {}, base, top, line);
  const zoomLabel = el('span.cmp__zoom');
  const tagL = el('span.cmp__tag.cmp__tag--l', { text: T.left });
  const tagR = el('span.cmp__tag.cmp__tag--r', { text: T.right });
  /* 标签与缩放控件挂在 stage 上，不参与缩放，字才不会跟着糊 */
  const stage = el('div.cmp__stage', {}, box, tagL, tagR,
    el('div.cmp__ctl', {},
      el('button.btn.btn--ghost.btn--icon.btn--sm', { type: 'button', 'data-tip': t('compare.zoomOut'), 'aria-label': t('compare.zoomOut'), html: icon('zoomOut', { cls: 'icon icon--sm' }), onclick: () => vp.zoomBy(1 / 1.35) }),
      zoomLabel,
      el('button.btn.btn--ghost.btn--icon.btn--sm', { type: 'button', 'data-tip': t('compare.zoomIn'), 'aria-label': t('compare.zoomIn'), html: icon('zoomIn', { cls: 'icon icon--sm' }), onclick: () => vp.zoomBy(1.35) }),
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '1:1', 'data-tip': t('compare.one2one'), onclick: () => vp.one2one() }),
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('compare.fit'), 'data-tip': t('compare.fitTip'), onclick: () => vp.fit() })));
  const bar = el('div.cmp__bar');
  const node = el('div.cmp', {}, stage, bar);

  const vp = createViewport({
    stage, layer: box,
    onScale: z => {
      zoomLabel.textContent = `${Math.round(z * 100)}%`;
      /* 中缝线与把手按倒数补偿，放大后不会变成粗杠 */
      box.style.setProperty('--z', String(z));
      line.style.setProperty('--z', String(z));
    },
  });

  let dragging = false;
  let space = false;

  /* 滚轮只归对比层，别让背后那张画布跟着缩放 */
  stage.addEventListener('wheel', e => e.stopPropagation(), { passive: true });

  const setSpace = on => { space = !!on; vp.setSpace(on); };

  const setPos = pct => {
    const p = clamp(pct, 0, 100);
    top.style.width = `${p}%`;
    line.style.left = `${p}%`;
  };
  const fromEvent = e => {
    const r = box.getBoundingClientRect();
    return ((e.clientX - r.left) / r.width) * 100;
  };

  /* 左键拖 = 中缝；空格或中键 = 平移（交给 viewport） */
  const panning = () => space || vp.mode === 'pan';
  box.addEventListener('pointerdown', e => {
    if (e.button !== 0 || panning()) return;
    dragging = true;
    try { box.setPointerCapture(e.pointerId); } catch { /* 合成事件 */ }
    setPos(fromEvent(e));
    e.preventDefault();
  });
  box.addEventListener('pointermove', e => { if (dragging) setPos(fromEvent(e)); });
  const stop = () => { dragging = false; };
  box.addEventListener('pointerup', stop);
  box.addEventListener('pointercancel', stop);
  box.addEventListener('dblclick', () => (vp.isFit ? vp.one2one() : vp.fit()));

  /** 两张图按同一自然尺寸铺，缩放由父层 transform 统一作用，左右天然同尺度 */
  function align() {
    if (!base.naturalWidth) return;
    vp.setContentSize(base.naturalWidth, base.naturalHeight);
    over.style.width = `${base.naturalWidth}px`;
    over.style.height = `${base.naturalHeight}px`;
    vp.fit();
    setPos(parseFloat(top.style.width) || 50);
  }
  base.addEventListener('load', align);
  over.addEventListener('load', align);

  const dl = (url, label) => url && el('a.btn.btn--ghost.btn--sm', { href: url, target: '_blank', download: '', rel: 'noreferrer',
    html: icon('download', { cls: 'icon icon--sm' }) + `<span>${label}</span>` });

  function show({ origUrl, resultUrl, cropUrl, overlayUrl, title, result, leftLabel }) {
    base.src = resultUrl;
    /* 只有一张图时（老结果没留线稿快照）不摆中缝：拖一条把子去分一张图是骗人的交互 */
    const two = !!origUrl;
    over.src = origUrl || '';
    over.hidden = !two;
    top.hidden = !two;
    line.hidden = !two;
    tagL.textContent = leftLabel || T.left;
    tagR.textContent = T.right;
    top.style.width = '50%';
    line.style.left = '50%';
    fill(bar,
      el('span.muted', { style: { marginRight: 'auto' }, text: title || '' }),
      onRestore && result ? el('button.btn.btn--ghost.btn--sm', {
        type: 'button', 'data-tip': T.restoreTip,
        html: icon('sliders', { cls: 'icon icon--sm' }) + `<span>${T.restore}</span>`, onclick: () => onRestore(result) }) : null,
      // 画布那一路才有"取回这一版当时的线稿"：照片的原图一直在那儿，不需要还
      onUseSketch && result?.sketch_url ? el('button.btn.btn--ghost.btn--sm', {
        type: 'button', 'data-tip': T.useSketchTip || t('compare.useSketchTip'),
        html: icon('brush', { cls: 'icon icon--sm' }) + `<span>${t('compare.useSketch')}</span>`, onclick: () => onUseSketch(result) }) : null,
      onFork && result?.final_url ? el('button.btn.btn--ghost.btn--sm', {
        type: 'button', 'data-tip': t('compare.forkTip'),
        html: icon('copy', { cls: 'icon icon--sm' }) + `<span>${t('compare.fork')}</span>`, onclick: () => onFork(result) }) : null,
      dl(cropUrl, t('compare.crop')),
      dl(overlayUrl, t('compare.overlay')),
      // 无损优先：走重算那道口。成图那一档现在是 q95，直链下去拿到的就不是原件了
      dl(result?.id ? `/api/results/${result.id}/lossless` : resultUrl, t('compare.download')),
      el('button.btn.btn--primary.btn--sm', { type: 'button', html: icon('close', { cls: 'icon icon--sm' }) + `<span>${t('compare.back')}</span>`, onclick: () => hide() }),
    );
    node.classList.add('is-on');
    requestAnimationFrame(align);
  }

  function hide() {
    node.classList.remove('is-on');
    setSpace(false);
    onClose?.();
  }

  /* 键盘在对比态下由这里接管，见 index.js 的 wireKeys */
  return {
    node, show, hide,
    get isOn() { return node.classList.contains('is-on'); },
    fit: () => vp.fit(), one2one: () => vp.one2one(),
    zoomBy: f => vp.zoomBy(f), setSpace,
  };
}
