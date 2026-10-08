// 主题：纸白 / 墨黑 / 跟随系统。
// 选择记在 localStorage 而不是库里——它是这台机器的显示偏好，换台机器不该被带走，
// 也不该为了上色多等一次接口：首帧之前由 core/theme-boot.js 先定好 data-theme。
'use strict';
import { call, isDesktop } from './desktop.js';

const KEY = 'synco.theme';

export const MODES = [['system', '跟随系统'], ['light', '纸白'], ['dark', '墨黑']];

export function mode() {
  try { return localStorage.getItem(KEY) || 'system'; } catch { return 'system'; }
}

export function resolved(m = mode()) {
  if (m === 'light' || m === 'dark') return m;
  return matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

/** 传 m 就是"用户选了这一档"（顺带记住），不传只是按当前选择重新上色 */
export function apply(m) {
  if (m) { try { localStorage.setItem(KEY, m); } catch { /* 隐私模式写不进去，本次显示照样对 */ } }
  const r = resolved(m);
  document.documentElement.dataset.theme = r;
  tellShell(r);
  return r;
}

/* 窗口第一帧的底色是 Rust 在页面之前画的，而它读不到 localStorage：把生效档位递一份给壳，
   它记进 desktop.json，下次启动就不会在墨黑档下闪一下纸白。同一档位不重复报。 */
let told = null;
function tellShell(r) {
  if (!isDesktop() || told === r) return;
  told = r;
  call('set_window_theme', { mode: r }).catch(() => { told = null; });
}

/** 跟随系统时才跟：固定成纸白/墨黑之后，系统换深色不该把应用悄悄带走 */
export function watch(onChange) {
  const mq = matchMedia('(prefers-color-scheme: dark)');
  mq.addEventListener('change', () => { if (mode() === 'system') onChange?.(apply()); });
}
