// 减弱动效：把「别动了」这件事收在一处判。
// 两个来源任一成立就一律瞬时——系统的 prefers-reduced-motion 与本应用的「减弱动效」开关。
// CSS 那边靠 body/html 上的类（见 tokens.css 末尾那一档），JS 这边靠 reduce()：
// 视口惯性（viewport.js）、视图过渡（router.js / theme.js）这些 rAF 与快照活儿都得先来问一句，
// 因为「动画时长变 0」对 requestAnimationFrame 和 startViewTransition 本身没有约束力。
// 开关存 localStorage 而不是库里：它和主题一样是这台机器的显示偏好，而且首帧之前就要读到
// （CSP 之下内联脚本不执行，所以首帧那一次由 core/theme-boot.js 抢在 body 存在之前挂 <html>）。
'use strict';

const KEY = 'synco.motion';

const mq = () => matchMedia('(prefers-reduced-motion: reduce)');

/** 应用那一档存了什么（没存过 = 关） */
export function saved() {
  try { return localStorage.getItem(KEY) === '1'; } catch { return false; }
}

/** 生效与否：系统偏好或应用开关，或起来 */
export const reduce = () => saved() || mq().matches;

/** 只有系统那一档开着（应用开关没碰）——设置页要说清是哪一路在起作用 */
export const osReduce = () => mq().matches;

/** 把两个来源都刷到 DOM 上：body 的类给 CSS，<html> 的类兜住首帧之前那一段 */
export function apply() {
  const on = reduce();
  document.body?.classList.toggle('reduce-motion', on);
  document.documentElement.classList.toggle('reduce-motion', on);
  return on;
}

/** 用户在这一台机器上拨的开关 */
export function set(on) {
  try { localStorage.setItem(KEY, on ? '1' : '0'); } catch { /* 隐私模式写不进去，本次显示照样对 */ }
  return apply();
}

/* 判色覆盖层（精修 / 画布）此刻在不在屏幕上。
   它和"减弱动效"回答的是同一个问题——这一会儿能不能演一场淡入淡出——所以放在这里一处判：
   快照会把整块画布抓成一张图再半透明地淡，既费（瓦片视口是原图像素的）又污染判色。
   CSS 侧对应 editor.css / canvas.css 里那几个 view-transition-name: none。 */
export const stageOpen = () => ['#editor', '#canvasView']
  .some(sel => { const n = document.querySelector(sel); return n && !n.hidden; });

/** 能不能演一次视图过渡：API 在不在（特征检测，不假设壳里那档 Chromium 一定带）、
    用户要不要减弱、画布有没有在场。任一不过就是原来的瞬切。 */
export const canTransition = () => typeof document.startViewTransition === 'function'
  && !reduce() && !stageOpen();

/** 系统那一档自己变了也要重挂：应用开关是「或」上去的，不重挂就会停在旧的那一份 */
export function watch(onChange) {
  mq().addEventListener('change', () => { const on = apply(); onChange?.(on); });
}
