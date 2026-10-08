// 本地调整（0.3）：右栏「本地调整」页 + 视口上的裁切 overlay + 液化笔刷。
//
// 铁律还是那一条：像素不在浏览器里落盘。这里发的是**参数**，收的是服务端渲染好的预览。
// 液化拖动那几帧的盘面重映射只是视觉反馈（不落盘、不入库），松手一律以服务端那张为准。
'use strict';
import { el, fill, clamp } from '../../core/dom.js';
import { api } from '../../core/api.js';
import { makeProw, makeSlider } from '../../ui/controls.js';
import { toastErr, toastOk } from '../../ui/toast.js';
import { confirm } from '../../ui/modal.js';
import { icon } from '../../core/icons.js';

/** 与后端 photoedit-core 的 EditOps 同形：缺的字段服务端按默认补，多余字段会被拒 */
const blankOps = () => ({
  v: 1,
  geometry: { crop: null, rotate_deg: 0, flip_h: false, flip_v: false, fill: 'edge' },
  warp: { strokes: [], auto: { face_slim: 0, eye_big: 0, nose_slim: 0, chin: 0 } },
  color: { exposure: 0, contrast: 0, highlights: 0, shadows: 0, temp: 0, tint: 0, saturation: 0, vibrance: 0, clarity: 0, sharpen: 0, preset: null },
  beauty: { smooth: 0, brighten: 0, sharpen: 0, by_mask: false },
  lut: null,
});

const RATIOS = [['free', '自由', 0], ['1:1', '1:1', 1], ['4:3', '4:3', 4 / 3], ['3:4', '3:4', 3 / 4], ['16:9', '16:9', 16 / 9], ['3:2', '3:2', 1.5], ['9:16', '9:16', 9 / 16]];
const COLOR_KEYS = [
  ['exposure', '曝光', '满档 = ±1.5 EV'],
  ['contrast', '对比度', '绕中灰缩放，中灰那点几乎不动'],
  ['highlights', '高光', '只作用在亮部'],
  ['shadows', '阴影', '只作用在暗部'],
  ['temp', '色温', '正 = 偏暖：R 升 B 降'],
  ['tint', '色调', '正 = 偏品红：G 降 B 补'],
  ['saturation', '饱和度', '整体绕亮度拉伸'],
  ['vibrance', '自然饱和度', '已经艳的不再加，肤色那一档先保住'],
  ['clarity', '清晰度', '大半径局部对比'],
  ['sharpen', '锐化', '3×3 高频回填'],
];
const BEAUTY_KEYS = [['smooth', '磨皮', '双边滤波在长边 2048 的域里跑，被抹掉的高频按比例回填'], ['brighten', '美白', '只在肤色域提亮，蓝天灰墙跟着动就是坏了'], ['sharpen', '锐化', '亮度域一次 3×3 高频回填']];
const BRUSHES = [['push', '推挤', '顺拖动方向把像素推开——沿身型轮廓向内推就是瘦身'], ['pucker', '收缩', '朝盘心吸，盘内整体变小'], ['bloat', '膨胀', '自盘心向外胀，盘内整体变大'], ['restore', '恢复', '把这一带拉回未变形之前']];

/** 预设就是一组滑杆值，和后端 color::preset 同一份表（改这里要同步改那边） */
const PRESET_VALUES = {
  clean: { exposure: 6, contrast: 10, vibrance: 12, clarity: 6, sharpen: 14 },
  warm: { exposure: 8, highlights: -12, shadows: 14, temp: 34, vibrance: 10 },
  film: { contrast: 22, highlights: -18, shadows: 20, temp: 12, saturation: -14, clarity: 10 },
  mono: { contrast: 40, saturation: -100, clarity: 24, sharpen: 20 },
  cool: { exposure: 4, temp: -32, tint: -6, vibrance: 8 },
  soft: { exposure: 10, contrast: -12, highlights: -10, shadows: 18, temp: 10, saturation: -10, clarity: -14 },
  crisp: { contrast: 26, shadows: -10, clarity: 30, sharpen: 26, vibrance: 14 },
  teal: { contrast: 18, shadows: 12, temp: -22, tint: 10, saturation: 12 },
  faded: { exposure: 8, contrast: -28, shadows: 26, saturation: -22 },
  night: { exposure: -14, contrast: 20, shadows: -16, temp: -26, tint: -10, clarity: 18 },
};

/** 滑杆拖动时每帧一次全图重算付不起 */
const PREVIEW_MS = 150;

export function createAdjust(deps) {
  const { layer, poster, stage, viewport, tiles, line, idOf, infoOf, onForked, brushLocked } = deps;

  let ops = blankOps();
  let frame = { w: 1, h: 1 };      // 当前底图那张的宽高（= 几何段之后的那个域）
  let mode = 'off';               // off | crop | warp
  let seq = 0;                    // 预览序号守卫：晚到的旧响应必须认出自己过期
  let dirty = false;
  let lastCrop = null;            // 「回到上次裁切」
  let ratioPick = 0;              // 当前锁死的宽高比（0 = 自由）
  let tool = 'push';
  let brushPx = 90;
  let pressure = 70;
  let timer = 0;

  /* ==================== 折叠分组与小控件（沿用参数页的 DOM 形状） ==================== */
  function grp(title, collapsed, ...body) {
    const caret = el('span.caret', { html: icon('right', { cls: 'icon icon--sm' }) });
    const bd = el('div.grp__bd', {}, ...body);
    const hd = el('button.grp__hd', { type: 'button', 'aria-expanded': String(!collapsed) }, caret, el('span', { text: title }));
    const node = el('div.grp', { class: `grp${collapsed ? ' is-collapsed' : ''}` }, hd, bd);
    hd.addEventListener('click', () => {
      const c = node.classList.toggle('is-collapsed');
      hd.setAttribute('aria-expanded', String(!c));
    });
    return node;
  }
  function switchRow(label, tip, onChange) {
    let on = false;
    const sw = el('button.toggle', { type: 'button', role: 'switch', 'aria-checked': 'false', 'aria-label': label });
    sw.addEventListener('click', () => { on = !on; sw.setAttribute('aria-checked', String(on)); onChange?.(on); });
    const node = el('div.prow', { style: { display: 'flex', alignItems: 'center', justifyContent: 'space-between', minHeight: '28px' } },
      el('span.prow__name', { text: label, 'data-tip': tip }), sw);
    return { node, set(v) { on = !!v; sw.setAttribute('aria-checked', String(on)); } };
  }

  /* ==================== 裁切重构 ==================== */
  const ratioChips = el('div.chips', {}, ...RATIOS.map(([k, label, r]) => el('button.chip-s', {
    type: 'button', text: label, dataset: { r: String(r), k },
    'data-tip': r ? `锁定 ${label}` : '不锁比例',
    onclick: () => setRatio(r),
  })));
  const fineCtl = makeProw({ label: '微调旋转', min: -15, max: 15, step: 0.5, value: 0, tip: '小角度走双线性重采样，画布会自动扩边', onChange: v => { ops.geometry.rotate_deg = quad() + v; touch(); } });
  const fillSeg = el('div.seg', {},
    el('button.seg__it', { type: 'button', dataset: { f: 'edge' }, text: '边缘延伸', 'data-tip': '不引入新颜色，构图里最看不出来', onclick: () => setFill('edge') }),
    el('button.seg__it', { type: 'button', dataset: { f: 'avg' }, text: '纯色(四角)', 'data-tip': '要留白时用，取源图四角均值', onclick: () => setFill('avg') }));
  const cropBtn = el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '拖裁切框', onclick: () => toggleMode('crop') });
  const geoBox = el('div', { style: { display: 'grid', gap: '10px' } },
    el('div.adj-row', {},
      el('button.btn.btn--ghost.btn--icon.btn--sm', { type: 'button', 'aria-label': '左转 90°', 'data-tip': '左转 90°（无损，不插值）', text: '↺', onclick: () => rotateBy(-90) }),
      el('button.btn.btn--ghost.btn--icon.btn--sm', { type: 'button', 'aria-label': '右转 90°', 'data-tip': '右转 90°（无损，不插值）', text: '↻', onclick: () => rotateBy(90) }),
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '左右翻转', onclick: () => flipIt('flip_h') }),
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '上下翻转', onclick: () => flipIt('flip_v') })),
    el('span.muted', { text: '宽高比 · 锁了之后拖角不变形' }), ratioChips, fineCtl.node,
    el('div.adj-row', {}, el('span.muted', { text: '转出来的边角补' }), fillSeg),
    el('div.adj-row', {}, cropBtn,
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '回到上次裁切', onclick: backToLastCrop }),
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '不裁', onclick: () => { ops.geometry.crop = null; ratioPick = 0; paintCrop(); paintRatio(); touch(); } })),
  );

  /* ==================== 塑形（手动液化） ==================== */
  const brushSeg = el('div.seg', {}, ...BRUSHES.map(([k, label, tip]) => el('button.seg__it', {
    type: 'button', dataset: { b: k }, text: label, 'data-tip': tip, onclick: () => setBrush(k),
  })));
  const radiusCtl = makeSlider({ min: 12, max: 400, step: 2, value: brushPx, ariaLabel: '液化盘半径', onChange: v => { brushPx = v; paintDiscSize(); } });
  const pressureCtl = makeProw({ label: '压力', min: 5, max: 100, step: 1, value: pressure, tip: '一步最多搬掉半径的 45%，再大就会咬到自己上一帧的采样', onChange: v => { pressure = v; } });
  const strokeCount = el('span.badge', { text: '0 笔' });
  const warpBox = el('div', { style: { display: 'grid', gap: '10px' } },
    brushSeg,
    el('div.adj-row', {}, el('span.muted.nowrap', { text: '盘半径' }), radiusCtl.node),
    pressureCtl.node,
    el('div.adj-row', {},
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '开始画形', onclick: () => toggleMode('warp') }),
      strokeCount,
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '退一笔', onclick: undoStroke }),
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '清掉笔画', onclick: clearStrokes })),
    el('p.muted', { text: '拖动时只有盘面内是前端的近似反馈；松手按整条参数链重算，以服务端那张为准。' }));

  /* ==================== 调色 + 美颜 ==================== */
  const COLOR = {};
  const colorBox = el('div', { style: { display: 'grid', gap: '10px' } }, ...COLOR_KEYS.map(([k, label, tip]) => {
    const ctl = makeProw({ label, min: -100, max: 100, step: 1, value: 0, tip, onChange: v => { ops.color[k] = v; if (ops.color.preset) { ops.color.preset = null; paintPreset(); } touch(); } });
    COLOR[k] = ctl;
    return ctl.node;
  }));
  const presetChips = el('div.chips');
  const lutSel = el('select.input', { 'aria-label': 'LUT 文件' });
  const lutCtl = makeProw({ label: 'LUT 强度', min: 0, max: 100, step: 1, value: 100, tip: '与原图混合的比例，0 = 完全不过表', onChange: v => { if (ops.lut) { ops.lut.strength = v; touch(); } } });
  lutSel.addEventListener('change', () => {
    const v = lutSel.value;
    ops.lut = v ? { name: v, strength: ops.lut?.strength ?? 100 } : null;
    lutCtl.setDisabled(!v);
    if (v) lutCtl.set(ops.lut.strength, true);
    touch();
  });
  const BEAUTY = {};
  const maskSw = switchRow('只在涂过的地方生效', '复用遮罩那层笔迹：涂哪儿磨哪儿；关掉就是全图', v => { ops.beauty.by_mask = v; touch(); });
  const beautyBox = el('div', { style: { display: 'grid', gap: '10px' } },
    ...BEAUTY_KEYS.map(([k, label, tip]) => {
      const ctl = makeProw({ label, min: 0, max: 100, step: 1, value: 0, tip, onChange: v => { ops.beauty[k] = v; touch(); } });
      BEAUTY[k] = ctl;
      return ctl.node;
    }),
    maskSw.node,
    el('p.muted', { text: '没涂遮罩 = 全图。这一层的蒙版坐标在变形之后的域里。' }));

  const node = el('div', { style: { display: 'grid', gap: '12px' } },
    el('p.muted', { text: '原图永不改写：下面这些都是参数，随时能退回。预览按 proxy 档算，落盘才走原分辨率。' }),
    grp('裁切重构', false, geoBox),
    grp('塑形（手动液化）', true, warpBox),
    grp('调色', true, el('div', { style: { display: 'grid', gap: '10px' } },
      el('span.muted', { text: '内置预设＝一组滑杆值，点了就是把那几根杆摆过去' }), presetChips, lutSel, lutCtl.node, colorBox)),
    grp('美颜', true, beautyBox),
    el('div.adj-actions', {},
      el('button.btn.btn--primary.btn--sm', { type: 'button', text: '落成图', onclick: renderFull }),
      el('button.btn.btn--accent.btn--sm', { type: 'button', text: '应用为新图', onclick: forkNew }),
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '清空全部调整', onclick: clearAll })),
  );

  /* ==================== overlay：裁切框与液化盘（都挂在 layer 里，跟着缩放走） ==================== */
  const handles = ['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w'].map(d => el(`div.adj-hd`, { dataset: { d }, 'aria-hidden': 'true', class: `adj-hd adj-hd--${d}` }));
  const cropBox = el('div.adj-crop__box', {}, el('div.adj-crop__grid'), ...handles);
  const cropLayer = el('div.adj-crop', { hidden: true }, cropBox);
  const warpCanvas = el('canvas.adj-warp', { 'aria-hidden': 'true', hidden: true });
  const disc = el('div.adj-disc', { hidden: true, 'aria-hidden': 'true' });
  layer.append(el('div.adj-overlay', {}, cropLayer, warpCanvas, disc));

  /* ==================== 参数 ⇄ 界面 ==================== */
  const quad = () => Math.round(ops.geometry.rotate_deg / 90) * 90;
  const geoActive = () => !!(ops.geometry.crop || ops.geometry.rotate_deg || ops.geometry.flip_h || ops.geometry.flip_v);
  // 一键塑形那排滑杆在 0.3.0 里没有生产者（见 service/face.rs），界面上不摆，参数域里留着
  const autoQuiet = !ops.warp.auto || Object.values(ops.warp.auto).every(v => !v);
  const isIdentity = () => !geoActive() && !ops.warp.strokes.length && autoQuiet
    && COLOR_KEYS.every(([k]) => !ops.color[k]) && BEAUTY_KEYS.every(([k]) => !ops.beauty[k]) && !ops.lut;

  function paintGeo() {
    fineCtl.set(ops.geometry.rotate_deg - quad(), true);
    for (const b of fillSeg.children) b.classList.toggle('is-on', b.dataset.f === ops.geometry.fill);
    paintRatio();
  }
  function paintRatio() {
    // 已裁的框自己就是比例：把实际比例最接近的那格点亮，锁没锁都看得出来
    const r = ops.geometry.crop ? (ops.geometry.crop[2] * frame.w) / (ops.geometry.crop[3] * frame.h || 1) : ratioPick;
    for (const c of ratioChips.children) {
      const cr = +c.dataset.r;
      c.classList.toggle('is-on', cr === 0 ? (ratioPick === 0 && !ops.geometry.crop) : Math.abs(cr - r) < 0.03 * Math.max(1, cr));
    }
  }
  function paintPreset() {
    const hit = ops.color.preset || '';
    for (const c of presetChips.children) c.classList.toggle('is-on', (c.dataset.p || '') === hit);
  }
  function paintPaints() {
    for (const [k] of COLOR_KEYS) COLOR[k].set(ops.color[k] ?? 0, true);
    for (const [k] of BEAUTY_KEYS) BEAUTY[k].set(ops.beauty[k] ?? 0, true);
    maskSw.set(!!ops.beauty.by_mask);
    paintPreset();
    lutSel.value = ops.lut?.name || '';
    lutCtl.set(ops.lut?.strength ?? 100, true);
    lutCtl.setDisabled(!ops.lut);
    strokeCount.textContent = `${ops.warp.strokes.length} 笔`;
  }
  function setRatio(r) {
    ratioPick = r;
    if (r > 0) {
      ops.geometry.crop = ops.geometry.crop ? relock(ops.geometry.crop, r) : centerCropFor(r);
      paintCrop();
    }
    paintRatio();
    touch();
  }
  /** 按当前画幅算一个居中的满框内接裁切 */
  function centerCropFor(r) {
    const avail = frame.w / frame.h;
    if (r >= avail) return [0, (1 - avail / r) / 2, 1, avail / r];
    return [(1 - r / avail) / 2, 0, r / avail, 1];
  }
  function relock(c, r) {
    const cx = c[0] + c[2] / 2, cy = c[1] + c[3] / 2;
    let w = c[2], h = (w * frame.w) / (r * frame.h);
    if (h > 1) { h = 1; w = (h * r * frame.h) / frame.w; }
    if (w > 1) { w = 1; h = (w * frame.w) / (r * frame.h); }
    return [clamp(cx - w / 2, 0, 1 - w), clamp(cy - h / 2, 0, 1 - h), w, h];
  }
  function rotateBy(d) { ops.geometry.rotate_deg = clamp(ops.geometry.rotate_deg + d, -180, 180); paintGeo(); touch(); }
  function setFill(f) { ops.geometry.fill = f; paintGeo(); touch(); }
  function flipIt(k) { ops.geometry[k] = !ops.geometry[k]; touch(); }
  function backToLastCrop() {
    if (!lastCrop) { line('这次还没裁过，没有"上次"可回'); return; }
    ops.geometry.crop = [...lastCrop];
    paintCrop();
    paintRatio();
    touch();
  }
  function pickPreset(id) {
    const d = PRESET_VALUES[id] || {};
    ops.color = { exposure: 0, contrast: 0, highlights: 0, shadows: 0, temp: 0, tint: 0, saturation: 0, vibrance: 0, clarity: 0, sharpen: 0, preset: id || null };
    for (const k of Object.keys(d)) ops.color[k] = d[k];
    paintPaints();
    touch();
  }

  /* ==================== 预览与保存 ==================== */
  function touch() { dirty = true; clearTimeout(timer); timer = setTimeout(commit, PREVIEW_MS); }

  /** 一次"存参数 + 拉预览"。seq 守卫：连拖滑杆时旧响应必须闭嘴 */
  async function commit() {
    const id = idOf();
    if (!id) return;
    const s = ++seq;
    const mine = () => s === seq && idOf() === id;
    try {
      const r = await api.saveAdjust(id, ops);
      if (!mine()) return;
      if (r?.clamped?.length) line(`已夹逼 ${r.clamped.length} 项：${r.clamped.slice(0, 2).join('、')}`);
      const p = await api.adjustPreview(id);
      if (!mine()) return;
      applyPreview(p);
      dirty = false;
    } catch (e) {
      if (mine()) line(`预览失败：${short(e)}`);
    }
  }

  function applyPreview(p) {
    if (!p?.preview_url) return;
    if (p.w > 0 && p.h > 0) frame = { w: p.w, h: p.h };
    // 预览档画的是"调整后"的那个域，源图的瓦片与它对不上，先撤干净再换底
    tiles.abort();
    setContentSize(frame.w, frame.h);
    poster.src = p.preview_url;
    poster.hidden = false;
    paintCrop();
    paintRatio();
  }

  function setContentSize(w, h) {
    // 走视口那个入口：它顺手把"刚换过内容"标上，下一次 fit 一步落位不做镜头推拉
    viewport.setContentSize(w, h);
  }

  /** 回到源图那套显示（参数清空、或要把遮罩涂回源图坐标系时） */
  async function restoreSource(reshowTiles = true) {
    const info = infoOf();
    if (!info) return;
    if (mode) exitMode();
    frame = { w: info.w, h: info.h };
    tiles.abort();
    setContentSize(info.w, info.h);
    poster.src = info.proxy_url || info.thumb_url || info.orig_url;
    paintCrop();
    paintRatio();
    if (!reshowTiles || info.orig_dead) return;
    try {
      const m = await api.tiles(info.id);
      if (idOf() === info.id) { tiles.setMeta(m); viewport.refresh(); }
    } catch { /* 没瓦片就停在海报上，与装载时同一套兜底 */ }
  }

  /* ==================== 模式进出 ==================== */
  function toggleMode(m) {
    if (mode === m) { exitMode(); return; }
    if (mode) exitMode();
    if (m === 'crop') {
      mode = 'crop';
      stage.classList.add('is-adjust');
      cropLayer.hidden = false;
      paintCrop();
      // 框要画在"其它都算完、只有没裁"的那一张上，坐标系才和用户看到的画面一致
      const probe = { ...ops, geometry: { ...ops.geometry, crop: null } };
      api.adjustPreviewWith(idOf(), probe).then(p => { if (mode === 'crop') applyPreview(p); }).catch(e => line(`裁切底图取不到：${short(e)}`));
      line('拖框裁画面 · 松手就按这套参数重算');
    } else if (m === 'warp') {
      mode = 'warp';
      stage.classList.add('is-adjust', 'is-warp');
      warpCanvas.hidden = false;
      disc.hidden = false;
      paintDiscSize();
      line(`用「${(BRUSHES.find(b => b[0] === tool) || ['', '推挤'])[1]}」在画面上拖`);
    }
  }

  function exitMode() {
    mode = 'off';
    stage.classList.remove('is-adjust', 'is-warp');
    cropLayer.hidden = true;
    disc.hidden = true;
    const c = warpCanvas.getContext('2d');
    warpCanvas.hidden = true;
    if (c) c.clearRect(0, 0, warpCanvas.width, warpCanvas.height);
  }

  /* ==================== 裁切框的指针 ==================== */
  /** 指针 → 当前帧的图像坐标。整笔复用一次 rect，与 paint.js 同一手法 */
  const toFrame = (e, rect) => ({
    x: (e.clientX - rect.left) * frame.w / Math.max(1, rect.width),
    y: (e.clientY - rect.top) * frame.h / Math.max(1, rect.height),
  });

  /** 框的位置与大小按百分比写：layer 本身就是按图像像素定尺寸的，百分比天然跟着缩放走 */
  function paintCrop() {
    const c = ops.geometry.crop;
    cropBox.style.opacity = c ? '1' : '0';
    if (!c) return;
    cropBox.style.left = `${c[0] * 100}%`;
    cropBox.style.top = `${c[1] * 100}%`;
    cropBox.style.width = `${c[2] * 100}%`;
    cropBox.style.height = `${c[3] * 100}%`;
  }

  let drag = null;
  cropBox.addEventListener('pointerdown', ev => {
    if (mode !== 'crop' || viewport.mode === 'pan') return;
    ev.preventDefault();
    ev.stopPropagation();
    drag = {
      dir: ev.target.dataset?.d || 'move',
      rect: layer.getBoundingClientRect(),
      start: toFrame(ev, layer.getBoundingClientRect()),
      base: [...(ops.geometry.crop || [0, 0, 1, 1])],
    };
    try { cropBox.setPointerCapture?.(ev.pointerId); } catch { /* 合成事件或已被捕获：指针捕获只是优化，不是必需 */ }
  });
  cropBox.addEventListener('pointermove', ev => {
    if (!drag) return;
    const p = toFrame(ev, drag.rect);
    ops.geometry.crop = moveRect(drag.base, drag.dir, (p.x - drag.start.x) / frame.w, (p.y - drag.start.y) / frame.h);
    paintCrop();
  });
  const endCrop = () => {
    if (!drag) return;
    drag = null;
    lastCrop = ops.geometry.crop ? [...ops.geometry.crop] : lastCrop;
    touch();
  };
  cropBox.addEventListener('pointerup', endCrop);
  cropBox.addEventListener('pointercancel', endCrop);

  function moveRect(b, dir, dx, dy) {
    let [x, y, w, h] = b;
    if (dir === 'move') return [clamp(x + dx, 0, 1 - w), clamp(y + dy, 0, 1 - h), w, h];
    if (dir.includes('w')) { const nx = clamp(x + dx, 0, x + w - 0.02); w += x - nx; x = nx; }
    if (dir.includes('e')) w = clamp(w + dx, 0.02, 1 - x);
    if (dir.includes('n')) { const ny = clamp(y + dy, 0, y + h - 0.02); h += y - ny; y = ny; }
    if (dir.includes('s')) h = clamp(h + dy, 0.02, 1 - y);
    if (ratioPick > 0 && dir !== 'n' && dir !== 's' && dir !== 'e' && dir !== 'w') {
      // 角柄锁比：以横向为准重算纵向，越界就整体缩回画面
      h = (w * frame.w) / (ratioPick * frame.h);
      if (y + h > 1) { h = 1 - y; w = (h * ratioPick * frame.h) / frame.w; }
      if (x + w > 1) { w = 1 - x; h = (w * frame.w) / (ratioPick * frame.h); }
    }
    if (ratioPick > 0 && (dir === 'n' || dir === 's')) { w = (h * ratioPick * frame.h) / frame.w; x = clamp(x + (b[2] - w) / 2, 0, Math.max(0, 1 - w)); }
    if (ratioPick > 0 && (dir === 'e' || dir === 'w')) { h = (w * frame.w) / (ratioPick * frame.h); y = clamp(y + (b[3] - h) / 2, 0, Math.max(0, 1 - h)); }
    return [clamp(x, 0, 1 - w), clamp(y, 0, 1 - h), Math.max(0.02, w), Math.max(0.02, h)];
  }

  /* ==================== 液化：盘面反馈只在盘内那一小块 ==================== */
  let stroke = null;
  layer.addEventListener('pointerdown', ev => {
    if (mode !== 'warp' || viewport.mode === 'pan') return;
    ev.preventDefault();
    ev.stopPropagation();
    const rect = layer.getBoundingClientRect();
    const p = toFrame(ev, rect);
    stroke = { pts: [[p.x / frame.w, p.y / frame.h]], rect, raf: 0, last: p };
    try { layer.setPointerCapture?.(ev.pointerId); } catch { /* 同上 */ }
    paintDiscAt(p);
  });
  layer.addEventListener('pointermove', ev => {
    if (!stroke || mode !== 'warp') return;
    const p = toFrame(ev, stroke.rect);
    const nx = p.x / frame.w, ny = p.y / frame.h;
    const tail = stroke.pts[stroke.pts.length - 1];
    // 抖动滤波：半个像素以内的回弹不记点，也不值得重算一帧
    if (Math.hypot(nx - tail[0], ny - tail[1]) * Math.max(frame.w, frame.h) < 0.6) return;
    stroke.pts.push([nx, ny]);
    stroke.last = p;
    paintDiscAt(p);
    if (!stroke.raf) stroke.raf = requestAnimationFrame(() => { stroke.raf = 0; feedback(); });
  });
  const endStroke = () => {
    if (!stroke) return;
    if (stroke.raf) cancelAnimationFrame(stroke.raf);
    const pts = stroke.pts;
    stroke = null;
    disc.hidden = true;
    warpCanvas.hidden = true;
    if (pts.length < 2) { line('这一笔没拖出轨迹，没有记进来'); return; }
    const unit = Math.sqrt(frame.w * frame.h) || 1;
    ops.warp.strokes.push({ tool, points: pts, radius: clamp(brushPx / unit, 0.002, 0.5), strength: pressure });
    paintPaints();
    touch();
  };
  layer.addEventListener('pointerup', endStroke);
  layer.addEventListener('pointercancel', endStroke);

  function paintDiscSize() {
    disc.style.width = `${brushPx * 2}px`;
    disc.style.height = `${brushPx * 2}px`;
    if (stroke) paintDiscAt(stroke.last);
  }
  function paintDiscAt(p) {
    disc.style.left = `${p.x - brushPx}px`;
    disc.style.top = `${p.y - brushPx}px`;
    disc.hidden = false;
  }

  /**
   * 盘面局部重映射：与内核同一套抛物线衰减，但只算盘内那一小块、只取最近一段的方向。
   * 它唯一的目的就是"手跟着走"，落定以服务端渲染为准，所以宁可粗也绝不碰整张图。
   */
  function feedback() {
    if (!stroke || stroke.pts.length < 2 || !poster.naturalWidth) return;
    const c = warpCanvas.getContext('2d');
    if (!c) return;
    const R = Math.max(8, Math.min(400, brushPx));
    const p = stroke.last;
    const x0 = Math.max(0, Math.round(p.x - R)), x1 = Math.min(frame.w, Math.round(p.x + R));
    const y0 = Math.max(0, Math.round(p.y - R)), y1 = Math.min(frame.h, Math.round(p.y + R));
    const bw = x1 - x0, bh = y1 - y0;
    if (bw < 2 || bh < 2) return;
    const sc = Math.min(1, 256 / Math.max(bw, bh));   // 反馈域封顶：盘再大也不该按盘面积付钱
    const cw = Math.max(2, Math.round(bw * sc)), ch = Math.max(2, Math.round(bh * sc));
    warpCanvas.width = cw; warpCanvas.height = ch;
    warpCanvas.style.left = `${x0}px`; warpCanvas.style.top = `${y0}px`;
    warpCanvas.style.width = `${bw}px`; warpCanvas.style.height = `${bh}px`;
    warpCanvas.hidden = false;
    // 源就是当前底图，按归一化区域从它身上取那一块
    try {
      c.drawImage(poster, (x0 / frame.w) * poster.naturalWidth, (y0 / frame.h) * poster.naturalHeight,
        (bw / frame.w) * poster.naturalWidth, (bh / frame.h) * poster.naturalHeight, 0, 0, cw, ch);
    } catch { return; }
    let img;
    try { img = c.getImageData(0, 0, cw, ch); } catch { return; }   // 底图取不到位图就只画盘，不重映射
    const src = img.data;
    const out = c.createImageData(cw, ch);
    const a = stroke.pts[stroke.pts.length - 2], b = stroke.pts[stroke.pts.length - 1];
    const shift = 0.45 * (pressure / 100);
    const dirSign = tool === 'bloat' ? 1 : -1;
    for (let j = 0; j < ch; j++) {
      for (let i = 0; i < cw; i++) {
        const o = (j * cw + i) * 4;
        const gx = x0 + i / sc, gy = y0 + j / sc;
        const dist = Math.hypot(gx - p.x, gy - p.y) / R;
        if (dist >= 1) {
          out.data[o] = src[o]; out.data[o + 1] = src[o + 1]; out.data[o + 2] = src[o + 2]; out.data[o + 3] = src[o + 3];
          continue;
        }
        const t = 1 - dist * dist, wgt = t * t;
        let ux, uy;
        if (tool === 'push') { ux = (b[0] - a[0]) * frame.w * shift * wgt; uy = (b[1] - a[1]) * frame.h * shift * wgt; }
        else {
          const vx = gx - p.x, vy = gy - p.y, n = Math.hypot(vx, vy) || 1;
          ux = (vx / n) * R * 0.5 * shift * dirSign * wgt;
          uy = (vy / n) * R * 0.5 * shift * dirSign * wgt;
        }
        const sx = clamp(Math.round((gx - ux - x0) * sc), 0, cw - 1);
        const sy = clamp(Math.round((gy - uy - y0) * sc), 0, ch - 1);
        const k = (sy * cw + sx) * 4;
        out.data[o] = src[k]; out.data[o + 1] = src[k + 1]; out.data[o + 2] = src[k + 2]; out.data[o + 3] = src[k + 3];
      }
    }
    c.putImageData(out, 0, 0);
  }

  function setBrush(k) {
    tool = k;
    for (const b of brushSeg.children) b.classList.toggle('is-on', b.dataset.b === k);
    if (mode === 'warp') line(`用「${(BRUSHES.find(b => b[0] === k) || ['', '推挤'])[1]}」在画面上拖`);
  }
  function undoStroke() {
    if (!ops.warp.strokes.length) { line('还没有笔画可退'); return; }
    ops.warp.strokes.pop();
    paintPaints();
    touch();
  }
  async function clearStrokes() {
    if (!ops.warp.strokes.length) return;
    if (!await confirm({ title: '清掉液化笔画', body: `这一张共 ${ops.warp.strokes.length} 笔。清掉后画面回到只有裁切/调色的状态，原图不受影响。`, ok: '清掉' })) return;
    ops.warp.strokes = [];
    paintPaints();
    touch();
  }

  /* ==================== 落成图 / 另存为新图 / 清空 ==================== */
  async function renderFull() {
    const id = idOf();
    if (!id) return;
    if (isIdentity()) { line('一根滑杆都没动，没有要落的盘'); return; }
    await flushNow();
    line('按原分辨率落盘中…');
    try {
      const r = await api.adjustRender(id);
      line(`成图已落盘（${(r.ms || 0).toLocaleString('zh-CN')} ms）· 原图没动`);
      toastOk('成图已落盘', r.reused ? '这套参数之前渲过，直接复用' : '');
    } catch (e) { line(`落盘失败：${short(e)}`); toastErr('落盘失败', short(e)); }
  }

  async function forkNew() {
    const id = idOf();
    if (!id) return;
    if (isIdentity()) { line('这张图还没调整，另存为新图没有意义'); return; }
    await flushNow();
    line('渲染并按新图登记…');
    try {
      const r = await api.adjustFork(id);
      toastOk('已应用为新图', '新图从空白参数开始，父图这套调整还在');
      onForked?.(r.image_id);
    } catch (e) { line(`另存失败：${short(e)}`); toastErr('另存为新图失败', short(e)); }
  }

  async function clearAll() {
    if (isIdentity()) { line('本来就是空的'); return; }
    if (!await confirm({ title: '清空全部调整', body: '裁切、液化笔画、调色、美颜都退回未调整，画面回到源图。原图一直没被改过，清空只是不再叠加这些参数。', ok: '清空' })) return;
    if (ops.geometry.crop) lastCrop = [...ops.geometry.crop];
    ops = blankOps();
    ratioPick = 0;
    paintPaints();
    paintGeo();
    touch();
    await commit();
    await restoreSource();
    line('已清空，画面回到源图');
  }

  /* ==================== 换图 ==================== */
  async function onImage(info) {
    if (mode) exitMode();
    seq++;                       // 上一张图在飞的响应全部作废
    clearTimeout(timer);
    dirty = false;
    stroke = null;
    drag = null;
    ops = blankOps();
    frame = { w: info.w, h: info.h };
    lastCrop = null;
    ratioPick = 0;
    paintCrop();
    paintPaints();
    paintGeo();
    setBrush(tool);
    buildPresets();
    try {
      const r = await api.adjust(info.id);
      if (idOf() !== info.id) return;
      const d = blankOps();
      ops = {
        v: r?.ops?.v || 1,
        geometry: { ...d.geometry, ...(r?.ops?.geometry || {}) },
        warp: { ...d.warp, ...(r?.ops?.warp || {}), auto: { ...d.warp.auto, ...(r?.ops?.warp?.auto || {}) } },
        color: { ...d.color, ...(r?.ops?.color || {}) },
        beauty: { ...d.beauty, ...(r?.ops?.beauty || {}) },
        lut: r?.ops?.lut ?? null,
      };
      ratioPick = 0;
      buildPresets(r?.presets || []);
      buildLuts(r?.luts || []);
      paintPaints();
      paintGeo();
      paintCrop();
      if (isIdentity()) return false;   // 空参数不用换底图：装载流程那侧已经把源图与瓦片摆好了
      // 库里带着参数进来：底图直接换成这套参数渲染的那一张，别让用户对着源图想象结果
      const p = await api.adjustPreview(info.id);
      if (idOf() !== info.id) return false;
      applyPreview(p);
      if (geoActive()) brushLocked?.('裁切/旋转后的画面和遮罩不是同一个坐标系：要先「应用为新图」再继续涂');
      return true;   // 这一张的显示已经是调整预览，装载流程就别再去摆源图瓦片了
    } catch (e) {
      if (idOf() === info.id) line(`调整参数读不到：${short(e)}`);
    }
    return false;
  }

  function buildPresets(list) {
    const items = list && list.length ? list : Object.entries(PRESET_VALUES).map(([id]) => ({ id, name: PRESET_NAMES[id] || id }));
    fill(presetChips, el('button.chip-s', { type: 'button', text: '无预设', dataset: { p: '' }, onclick: () => pickPreset(null) }),
      ...items.map(p => el('button.chip-s', { type: 'button', text: p.name, dataset: { p: p.id }, 'data-tip': `预设＝一组滑杆值（${p.id}）`, onclick: () => pickPreset(p.id) })));
    paintPreset();
  }
  function buildLuts(names) {
    fill(lutSel, el('option', { value: '', text: '不用 LUT' }), ...(names || []).map(n => el('option', { value: n, text: n.replace(/\.cube$/i, '') })));
    lutSel.value = ops.lut?.name || '';
    lutCtl.setDisabled(!ops.lut);
  }

  const short = e => String(e?.message || e || '未知错误').slice(0, 90);

  return {
    node,
    onImage,
    /** 缩放变了要通知 overlay：手柄、盘边框与盘位置都按 1/scale 反向 sizing */
    setScale(s) {
      layer.style.setProperty('--iz', String(1 / (s || 1)));
      if (stroke) paintDiscAt(stroke.last);
    },
    flush: flushNow,
    restoreSource,
    exit() { if (mode) exitMode(); },
    get mode() { return mode; },
    get geometryActive() { return geoActive(); },
    get dirty() { return dirty; },
  };

  /** 把没发出去的那次改动立刻发出去（提交生成、关编辑器前都要过一道） */
  async function flushNow() {
    clearTimeout(timer);
    if (dirty) await commit();
  }
}

const PRESET_NAMES = { clean: '纯净', warm: '暖阳', film: '胶片', mono: '黑白对比', cool: '冷调', soft: '人像柔和', crisp: '通透', teal: '青橙', faded: '褪色', night: '夜色' };
