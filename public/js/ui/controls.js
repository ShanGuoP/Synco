// 表单控件工厂：滑杆 / 「滑杆+数值直输」参数行
// 只负责交互与外观，不掺业务；业务通过 onChange 回调拿值
'use strict';
import { el, clamp } from '../core/dom.js';
import { fmtNum } from '../core/format.js';

const snap = (v, min, max, step) => {
  const q = Math.round((clamp(v, min, max) - min) / step) * step + min;
  return Number(q.toFixed(6));
};

/**
 * 自定义滑杆（替代原生 range，带数值气泡 + 键盘可操作）
 * @returns {{node:HTMLElement, get value():number, set(v:number,silent?:boolean):void}}
 */
export function makeSlider({ min = 0, max = 100, step = 1, value = min, onChange, format, ariaLabel = '' }) {
  let v = snap(value, min, max, step);
  let dragging = false;

  const bubble = el('span.slider__bubble');
  const track = el('div.slider__track', {}, el('div.slider__fill'), el('div.slider__thumb', {}, bubble));
  const node = el('div.slider', {
    tabindex: '0', role: 'slider',
    'aria-valuemin': min, 'aria-valuemax': max, 'aria-label': ariaLabel,
  }, track);

  const paint = () => {
    node.style.setProperty('--fill', `${((v - min) / (max - min)) * 100}%`);
    node.setAttribute('aria-valuenow', v);
    bubble.textContent = format ? format(v) : fmtNum(v, step);
  };
  const emit = () => onChange?.(v);
  let disabled = false;

  const fromClient = x => {
    const r = track.getBoundingClientRect();
    return snap(min + ((x - r.left) / Math.max(1, r.width)) * (max - min), min, max, step);
  };
  /* 只有用户驱动的 commit 才派发 input：参数面板用 body 上的 input 捕获来清"改自 #id"的高亮，
     自绘滑杆原来只走 JS 回调，拖完滑杆高亮赖着不走（程序性 set() 不该被当成用户改动） */
  const commit = nv => {
    if (disabled || nv === v) return;
    v = nv; paint();
    node.dispatchEvent(new Event('input', { bubbles: true }));
    emit();
  };

  node.addEventListener('pointerdown', e => {
    if (disabled) { e.preventDefault(); return; }     // 禁用态连拖都不能起
    dragging = true; node.classList.add('is-drag');
    try { node.setPointerCapture(e.pointerId); } catch { /* 合成事件或已捕获 */ }
    commit(fromClient(e.clientX)); e.preventDefault();
  });
  node.addEventListener('pointermove', e => { if (dragging) commit(fromClient(e.clientX)); });
  const end = e => { if (!dragging) return; dragging = false; node.classList.remove('is-drag'); try { node.releasePointerCapture(e.pointerId); } catch { /* 已释放 */ } };
  node.addEventListener('pointerup', end);
  node.addEventListener('pointercancel', end);
  node.addEventListener('keydown', e => {
    if (disabled) return;               // 原来只切 class 与 tabIndex，方向键照样改值并写进 settings
    const big = step * (max - min) / 10;
    const map = { ArrowRight: step, ArrowUp: step, ArrowLeft: -step, ArrowDown: -step, PageUp: big, PageDown: -big };
    if (e.key in map) { commit(snap(v + map[e.key], min, max, step)); e.preventDefault(); }
    else if (e.key === 'Home') { commit(min); e.preventDefault(); }
    else if (e.key === 'End') { commit(max); e.preventDefault(); }
  });

  paint();
  return {
    node,
    get value() { return v; },
    set(nv, silent) {
      const n = snap(nv, min, max, step);
      if (n === v && !silent) return;
      v = n; paint();
      if (!silent) emit();
    },
    setDisabled(b) {
      disabled = !!b;
      node.classList.toggle('is-disabled', disabled);
      node.tabIndex = disabled ? -1 : 0;
      node.setAttribute('aria-disabled', String(disabled));
      if (disabled) { dragging = false; node.classList.remove('is-drag'); }
    },
  };
}

/**
 * 参数行：上行「标签 ←→ 数值直输」，下行整宽滑杆
 * 滑杆与直输框双向同步，直输只在 change/Enter 时提交，避免边打字边触发
 */
export function makeProw({ label, tip = '', min = 0, max = 100, step = 1, value, onChange, format, flag = '' }) {
  let v = snap(value ?? min, min, max, step);
  const num = el('input.input.input--num', { type: 'number', min, max, step, value: fmtNum(v, step), 'aria-label': label });
  const sl = makeSlider({
    min, max, step, value: v, format, ariaLabel: label,
    onChange: nv => { v = nv; num.value = fmtNum(nv, step); onChange?.(nv); },
  });
  const row = el('div.prow', {},
    el('div.prow__top', {},
      el('span.prow__name', { text: label, ...(tip ? { 'data-tip': tip } : {}) },
        flag ? el('span.new', { text: flag }) : null),
      el('span.prow__ctl', {}, num)),
    sl.node,
  );
  const commit = () => {
    const raw = parseFloat(num.value);
    if (!Number.isFinite(raw)) { num.value = fmtNum(v, step); return; }
    sl.set(raw);
  };
  num.addEventListener('change', commit);
  num.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); commit(); num.blur(); } });

  return {
    node: row,
    get value() { return v; },
    set(nv, silent) { sl.set(nv, silent); v = sl.value; num.value = fmtNum(v, step); },
    setDisabled(b) { sl.setDisabled(b); num.disabled = !!b; row.classList.toggle('is-off', !!b); },
  };
}
