// 主题：纸白 / 墨黑 / 跟随系统。
// 选择记在 localStorage 而不是库里——它是这台机器的显示偏好，换台机器不该被带走，
// 也不该为了上色多等一次接口：首帧之前由 core/theme-boot.js 先定好 data-theme。
'use strict';
import { call, isDesktop } from './desktop.js';
import { canTransition } from './motion.js';

const KEY = 'synco.theme';
const STYLE_KEY = 'synco.appearance';
export const STYLES = [['magazine', 'settings.theme.magazine'], ['glass', 'settings.theme.glass']];

function readAppearance() {
  try { return localStorage.getItem(STYLE_KEY) === 'magazine' ? 'magazine' : 'glass'; }
  catch { return 'glass'; }
}
// 存储被禁用时仍保留本次页面选择，设置选中态不能退回默认风格。
let currentAppearance = readAppearance();
let appearanceRevision = 0;
export const appearance = () => currentAppearance;

/** 风格与明暗独立；复用主题过渡及减弱动效的判断。 */
export function applyAppearance(value) {
  const next = value === 'magazine' ? 'magazine' : 'glass';
  currentAppearance = next;
  const revision = ++appearanceRevision;
  try { localStorage.setItem(STYLE_KEY, next); } catch { /* 本次选择仍生效 */ }
  transition(() => {
    if (revision === appearanceRevision) document.documentElement.dataset.appearance = next;
  },
    next !== document.documentElement.dataset.appearance);
  return next;
}

// 只有键：模块求值早于字典装载，标签必须在渲染时查
export const MODES = [['system', 'theme.follow'], ['light', 'theme.paper'], ['dark', 'theme.ink']];

export function mode() {
  try { return localStorage.getItem(KEY) || 'system'; } catch { return 'system'; }
}

export function resolved(m = mode()) {
  if (m === 'light' || m === 'dark') return m;
  return matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

/* F7d 主题 crossfade：整根淡入淡出，不逐属性过渡。
   逐条 transition 会让纸面、墨线、文字各自漂到不同的时间上（底色先变、字后变），比瞬切更难看；
   根级那一下用 View Transition 的快照做，时长读 --vt-fade（tokens.css），主题这一档给 300ms。
   能不能演由 core/motion.js 一处判：API 不在（特征检测）、减弱动效、或画布/精修覆盖层在场，
   任一成立就落回原来的瞬切——判色区不该被一张半透明的快照污染，也不该为它花一次大纹理拷贝。 */
const VT_FLAG = 'vt-theme';
const paint = r => { document.documentElement.dataset.theme = r; tellShell(r); };
let themeRevision = 0;
let transitionRevision = 0;
let activeTransition = null;

/** 传 m 就是"用户选了这一档"（顺带记住），不传只是按当前选择重新上色 */
export function apply(m) {
  if (m) { try { localStorage.setItem(KEY, m); } catch { /* 隐私模式写不进去，本次显示照样对 */ } }
  const r = resolved(m);
  const revision = ++themeRevision;
  const flip = r !== document.documentElement.dataset.theme;   // 首帧与重复上色都不演一遍
  transition(() => { if (revision === themeRevision) paint(r); }, flip);
  return r;
}

function transition(update, flip) {
  const revision = ++transitionRevision;
  // 跳过快照动画并不会取消旧 update 回调，所以每个偏好还各自校验版本。
  activeTransition?.skipTransition();
  activeTransition = null;
  if (!flip || !canTransition()) {
    document.documentElement.classList.remove(VT_FLAG);
    update();
    return;
  }
  // 类名要在 startViewTransition 之前挂：新旧两侧的 animation-duration 才取到同一个值
  document.documentElement.classList.add(VT_FLAG);
  const done = () => {
    if (revision !== transitionRevision) return;
    activeTransition = null;
    document.documentElement.classList.remove(VT_FLAG);
  };
  try {
    // finished 会被"后一次过渡顶掉"而 reject，那不是错误：接住它，只为了收尾摘类名
    activeTransition = document.startViewTransition(update);
    activeTransition.finished.then(done, done);
  } catch {   // 快照阶段出问题（极少）就把这一次当普通上色，颜色不能不落地
    done();
    update();
  }
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
