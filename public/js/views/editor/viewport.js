// 画布视口引擎：缩放（光标锚点）/ 平移 / 适应 / 1:1 / 双指捏合
// 只管 transform，不碰像素内容；绘制坐标换算由调用方用 getBoundingClientRect 完成
'use strict';
import { clamp, raf } from '../../core/dom.js';
import { reduce } from '../../core/motion.js';

const MIN = 0.02, MAX = 8;

/* ---------- F7a 惯性参数 ----------
   离散输入（滚轮 / 工具条那几个缩放键 / 方向键 / Shift+滚轮）算出的是"目标那一档"，
   画面按指数平滑靠过去：每帧按真实时长（毫秒）折算衰减量 k = 1 - e^(-dt/τ)，掉一帧也不会突然跳。
   τ=60ms → 每帧收 24% 的残差，约 19 帧（320ms）落到阈值内；再叠一条 380ms 的硬上限（<400ms 是硬要求），
   任何情况下都不会"飘"着不落地（上限那一刀直接把值定在目标上，不留半个像素的"差一点"）。
   靠近 1:1 时 τ 收到 32ms：那一档用户在抠像素边缘，画面必须跟手，不许有余晃。
   连续手势（拖拽平移、双指捏合）不走这套：它们本来就 1:1 贴着指针，插值只会拖出橡皮感。 */
const SETTLE_MS = 380;
const TAU = 60, TAU_STIFF = 32, NEAR_ONE = 0.06;      // 全部毫秒；dt 也用毫秒算，单位别混
const EPS_PX = 0.3, EPS_SCALE = 0.0006;

export function createViewport({ stage, layer, onScale, onView }) {
  let s = 1, tx = 0, ty = 0;      // scale / translate：真正写在 transform 上的那三个值
  let sT = 1, txT = 0, tyT = 0;   // 目标值：手势算出来的"应该到哪儿"，静止时与上面完全相等
  let w = 0, h = 0;               // 图像自然尺寸
  let mode = 'paint';             // paint | pan
  let space = false;
  let land = false;               // 刚换过内容：下一次 fit() 一步落位，不做镜头推拉
  let tick = 0, last = 0, deadline = 0;
  const pointers = new Map();     // pointerId -> {x, y}
  let pinch = null;

  /* transform 与百分比读数都直接写：样式/文本写入本身就会被浏览器合帧，套一层
     rAF 反而会在后台标签里被节流，导致画布和读数停在旧值。
     F7a 之后仍然成立——apply() 没被包进任何循环里，谁调用它谁就把当前值立刻落屏；
     唯一的 rAF 循环只在惯性真的在跑的那几百毫秒里存在（tick 为空就是没在跑），
     到点即停，标签页一进后台就直接落位（见 flat / onVis）。 */
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

  /* ---------- 惯性步进 ---------- */
  const stop = () => { if (tick) cancelAnimationFrame(tick); tick = 0; };
  /* 减弱动效（系统偏好或应用开关）或标签页在后台：一步到位。
     后台里 rAF 根本不跑，留着动画等于把画布停在半路，读数也跟着停在旧值。 */
  const flat = () => reduce() || document.hidden;
  const jump = () => { stop(); s = sT; tx = txT; ty = tyT; apply(); };
  const moving = () => Math.abs(sT - s) > sT * EPS_SCALE
    || Math.abs(txT - tx) > EPS_PX || Math.abs(tyT - ty) > EPS_PX;

  function step(now) {
    tick = 0;
    /* 循环跑着的时候被切到减弱动效 / 切到后台：就地落位收尾，不留一帧一帧的空转 */
    if (flat()) { s = sT; tx = txT; ty = tyT; apply(); return; }
    const dt = clamp(now - last, 1, 64);   // 毫秒：τ 也是毫秒，单位别混（混了会慢四千倍，只靠上限那一刀收）
    last = now;
    const stiff = Math.abs(sT - 1) <= NEAR_ONE || Math.abs(s - 1) <= NEAR_ONE;
    const k = 1 - Math.exp(-dt / (stiff ? TAU_STIFF : TAU));
    s += (sT - s) * k; tx += (txT - tx) * k; ty += (tyT - ty) * k;
    /* 到位或超过硬上限就收口：停在目标值上，不留下半个像素的"差一点" */
    if (!moving() || now >= deadline) { s = sT; tx = txT; ty = tyT; apply(); return; }
    apply();
    tick = requestAnimationFrame(step);
  }

  /** 离散输入用：改目标，然后开始往目标靠 */
  function glide() {
    if (flat()) { jump(); return; }
    deadline = performance.now() + SETTLE_MS;   // 新输入进来，400ms 的预算从这一刻重算
    if (tick) return;                           // 已经在跑就接着跑：不另起一条循环去抢同一组值
    last = performance.now();
    tick = requestAnimationFrame(step);
  }

  /** 连续手势用：当前值与目标值一起改，画面 1:1 跟着指针 */
  function follow(px, py, ns) {
    ns = clamp(ns, MIN, MAX);
    const k = ns / s;
    tx = px - (px - tx) * k;
    ty = py - (py - ty) * k;
    s = ns;
    sT = s; txT = tx; tyT = ty;   // 在飞的那段惯性交还手势，不跟手动画抢同一次拖动
    apply();
  }

  /** 以视口内某点为锚缩放到 ns：该点下的像素保持不动 */
  function zoomAt(px, py, factor) {
    const ns = clamp(sT * factor, MIN, MAX);
    const k = ns / sT;
    txT = px - (px - txT) * k;
    tyT = py - (py - tyT) * k;
    sT = ns;
    glide();
  }
  function panBy(dx, dy) { txT += dx; tyT += dy; glide(); }

  function setMode(m) {
    mode = m;
    const panning = m === 'pan' || space;
    stage.classList.toggle('is-pan', panning);
    stage.classList.toggle('is-paint', !panning);
    /* 平移时让画布层不吃事件，好让 stage 收到拖动 */
    layer.classList.toggle('pan-through', panning);
  }

  function fit() {
    const r = box();
    sT = fitScale();
    txT = (r.width - w * sT) / 2;
    tyT = (r.height - h * sT) / 2;
    /* 装载完新内容的那一次 fit 是"把照片摆正"，不是手势：判色区里不该来一回镜头推拉。
       setContentSize 每换一张图都会置位，用户自己按「适应」时才走惯性。 */
    if (land) { land = false; jump(); return; }
    glide();
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
    /* 手一落就把手势的起点定在"屏幕此刻这档"上：在飞的平移不再追自己的尾巴 */
    if (pointers.size === 1) { txT = tx; tyT = ty; }
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
      follow((a.x + b.x) / 2 - r.left, (a.y + b.y) / 2 - r.top, pinch.base * dist / pinch.dist);
      return;
    }
    tx += e.clientX - prev.x; ty += e.clientY - prev.y;
    txT += e.clientX - prev.x; tyT += e.clientY - prev.y;   // 目标同步平移：在飞的缩放仍朝同一落点收
    apply();
  }

  const onUp = e => {
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
  };
  stage.addEventListener('pointerdown', onDown);
  stage.addEventListener('pointermove', onMove);
  stage.addEventListener('pointerup', onUp);
  stage.addEventListener('pointercancel', onUp);

  /* 切到后台：惯性就地落位。回来时看见的是已经停稳的画布，不是半路的动画 */
  const onVis = () => { if (document.hidden) jump(); };
  document.addEventListener('visibilitychange', onVis);

  /* ---------- 视口变窄时若原本就是「适应」态则重新贴合 ---------- */
  const ro = new ResizeObserver(raf(() => { stageRect = null; if (wasFit()) fit(); else apply(); }));
  // 判"是不是适应态"看目标值：惯性在跑的时候 s 还在路上，拿它比会误判成"用户自己缩放过"
  const wasFit = () => Math.abs(sT - fitScale()) < 1e-4;
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
      land = true;
    },
    fit,
    one2one() { const r = box(); zoomAt(r.width / 2, r.height / 2, 1 / sT); },
    setZoom(t) { const r = box(); zoomAt(r.width / 2, r.height / 2, clamp(t, MIN, MAX) / sT); },
    zoomBy(f) { const r = box(); zoomAt(r.width / 2, r.height / 2, f); },
    setMode,
    setSpace(on) { space = on; setMode(mode); },
    centerOn(ix, iy) { const r = box(); stageRect = null; txT = r.width / 2 - ix * sT; tyT = r.height / 2 - iy * sT; glide(); },
    /** 装载完新内容后叫一次：瓦片视图需要按当前视口重算该取哪几格 */
    refresh() { stageRect = null; apply(); },
    destroy() {
      stop();
      ro.disconnect();
      document.removeEventListener('visibilitychange', onVis);
      stage.removeEventListener('wheel', onWheel);
      stage.removeEventListener('pointerdown', onDown);
      stage.removeEventListener('pointermove', onMove);
      stage.removeEventListener('pointerup', onUp);
      stage.removeEventListener('pointercancel', onUp);
    },
  };
}
