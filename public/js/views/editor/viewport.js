// 画布视口引擎：缩放（光标锚点）/ 平移 / 适应 / 1:1 / 双指捏合
// 只管 transform，不碰像素内容；绘制坐标换算由调用方用 getBoundingClientRect 完成
'use strict';
import { clamp, raf } from '../../core/dom.js';

const MIN = 0.02, MAX = 8;

export function createViewport({ stage, layer, onScale, onView }) {
  let s = 1, tx = 0, ty = 0;      // scale / translate
  let w = 0, h = 0;               // 图像自然尺寸
  let mode = 'paint';             // paint | pan
  let space = false;
  const pointers = new Map();     // pointerId -> {x, y}
  let pinch = null;

  /* transform 与百分比读数都直接写：样式/文本写入本身就会被浏览器合帧，套一层
     rAF 反而会在后台标签里被节流，导致画布和读数停在旧值 */
  const apply = () => {
    layer.style.transform = `translate3d(${tx}px, ${ty}px, 0) scale(${s})`;
    onScale?.(s);
    // 瓦片视图要的是完整视口状态，不只是缩放。stage 的尺寸只在布局变化时变，
    // 所以缓存起来 —— 平移时每帧 getBoundingClientRect 会把布局强制算一遍。
    if (onView) {
      if (!stageRect) stageRect = box();
      onView({ s, tx, ty, stageW: stageRect.width, stageH: stageRect.height });
    }
  };
  const box = () => stage.getBoundingClientRect();
  let stageRect = null;
  const fitScale = () => {
    const r = box();
    if (!w || !h) return 1;
    return clamp(Math.min((r.width - 56) / w, (r.height - 56) / h), MIN, 1);
  };

  /** 以视口内某点为锚缩放：该点下的像素保持不动 */
  function zoomAt(px, py, factor) {
    const ns = clamp(s * factor, MIN, MAX);
    const k = ns / s;
    tx = px - (px - tx) * k;
    ty = py - (py - ty) * k;
    s = ns;
    apply();
  }
  function panBy(dx, dy) { tx += dx; ty += dy; apply(); }

  function setMode(m) {
    mode = m;
    const panning = m === 'pan' || space;
    stage.classList.toggle('is-pan', panning);
    stage.classList.toggle('is-paint', !panning);
    /* 平移时让画布层不吃事件，好让 stage 收到拖动 */
    layer.classList.toggle('pan-through', panning);
  }

  function fit() {
    s = fitScale();
    const r = box();
    tx = (r.width - w * s) / 2;
    ty = (r.height - h * s) / 2;
    apply();
  }

  function onWheel(e) {
    e.preventDefault();
    const r = box();
    const px = e.clientX - r.left, py = e.clientY - r.top;
    if (e.shiftKey && !e.ctrlKey) { panBy(-e.deltaY, 0); return; }
    zoomAt(px, py, Math.exp(-(e.ctrlKey ? e.deltaY * 2.4 : e.deltaY) * 0.0018));
  }
  stage.addEventListener('wheel', onWheel, { passive: false });

  function onDown(e) {
    if (!(mode === 'pan' || space || e.button === 1)) return;
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    try { stage.setPointerCapture(e.pointerId); } catch { /* 已捕获 */ }
    if (pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      pinch = { dist: Math.hypot(a.x - b.x, a.y - b.y), base: s };
    }
    e.preventDefault();
  }

  function onMove(e) {
    if (!pointers.has(e.pointerId)) return;
    const prev = pointers.get(e.pointerId);
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });

    if (pointers.size === 2 && pinch) {
      const [a, b] = [...pointers.values()];
      const dist = Math.hypot(a.x - b.x, a.y - b.y);
      const r = box();
      zoomAt((a.x + b.x) / 2 - r.left, (a.y + b.y) / 2 - r.top, (dist / pinch.dist) * pinch.base / s);
      return;
    }
    panBy(e.clientX - prev.x, e.clientY - prev.y);
  }

  const onUp = e => {
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
  };
  stage.addEventListener('pointerdown', onDown);
  stage.addEventListener('pointermove', onMove);
  stage.addEventListener('pointerup', onUp);
  stage.addEventListener('pointercancel', onUp);

  /* ---------- 视口变窄时若原本就是「适应」态则重新贴合 ---------- */
  const ro = new ResizeObserver(raf(() => { stageRect = null; if (wasFit()) fit(); else apply(); }));
  const wasFit = () => Math.abs(s - fitScale()) < 1e-4;
  ro.observe(stage);

  return {
    get scale() { return s; },
    get mode() { return mode; },
    get zoomPct() { return Math.round(s * 100); },
    get isFit() { return wasFit(); },

    setContentSize(cw, ch) {
      w = cw; h = ch;
      layer.style.width = `${cw}px`;
      layer.style.height = `${ch}px`;
    },
    fit,
    one2one() { const r = box(); zoomAt(r.width / 2, r.height / 2, 1 / s); },
    setZoom(t) { const r = box(); zoomAt(r.width / 2, r.height / 2, clamp(t, MIN, MAX) / s); },
    zoomBy(f) { const r = box(); zoomAt(r.width / 2, r.height / 2, f); },
    setMode,
    setSpace(on) { space = on; setMode(mode); },
    centerOn(ix, iy) { const r = box(); stageRect = null; tx = r.width / 2 - ix * s; ty = r.height / 2 - iy * s; apply(); },
    /** 装载完新内容后叫一次：瓦片视图需要按当前视口重算该取哪几格 */
    refresh() { stageRect = null; apply(); },
    destroy() { ro.disconnect(); stage.removeEventListener('wheel', onWheel); stage.removeEventListener('pointerdown', onDown); stage.removeEventListener('pointermove', onMove); stage.removeEventListener('pointerup', onUp); stage.removeEventListener('pointercancel', onUp); },
  };
}
