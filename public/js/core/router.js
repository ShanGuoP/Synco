// Hash 路由：#/ 首页 · #/p/:id 项目 · #/p/:id/e/:imgId 编辑器
'use strict';
import { $ } from './dom.js';
import { canTransition } from './motion.js';

let gen = 0;    // 路由世代号：号一变就是用户已经走掉，慢回来的响应不该再往 store 里写

export const routeGen = () => gen;

/* F7b 视图过渡。两道门槛，任一不过就是原来的瞬切（页面照常换，只是不淡）：
   · 这一条路由要开精修 / 画布覆盖层——那里铺的是原图像素尺寸的瓦片视口，
     过渡会把整块画布抓成一张快照再半透明地淡，判色区不容污染，纹理拷贝也远超"淡一下"的价值。
     （canTransition 里还叠了第二条：离开覆盖层时它还挂在屏幕上，一并挡掉，
       所以进出画布两头都不演。API 不在、或减弱动效开着，也在同一处判。） */
const NO_TRANSITION = /^\/p\/[^/]+\/[ec]\//;
/* 外壳里不跟着翻页的两块（书脊 / 刊头）：过渡期间给它们挂上 view-transition-name，
   页面在淡、框不动。名字只能挂唯一元素，所以一档一个类名（同名挂两处会直接把过渡判失败）。
   选择器限定在 #shell 里面：编辑器那条 .ed-rail 与它无关；刊头在 .main 里，得用后代选择器。 */
const HOLD = [['#shell > .rail', 'vt-rail'], ['#shell .topbar', 'vt-topbar']];

const hold = on => { for (const [sel, cls] of HOLD) $(sel)?.classList.toggle(cls, on); };

const compile = pattern => {
  const keys = [];
  const re = new RegExp('^' + pattern
    .replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    .replace(/\\?\$(?::(\w+))/g, (_, k) => { keys.push(k); return '([^/]+)'; })
    .replace(/:(\w+)/g, (_, k) => { keys.push(k); return '([^/]+)'; }) + '/?$');
  return { re, keys };
};

export function createRouter() {
  const routes = [];
  let notFound = null;
  let current = null;
  let first = true;     // 首屏那一次不演：应用刚打开就整页淡一遍是多余

  const parse = () => {
    const raw = location.hash.slice(1) || '/';
    return raw.startsWith('/') ? raw : '/' + raw;
  };

  /* 换页动作本身：与包过渡之前一模一样（世代号先自增，慢响应靠它作废） */
  async function swap(path) {
    for (const r of routes) {
      const m = r.re.exec(path);
      if (!m) continue;
      const params = Object.fromEntries(r.keys.map((k, i) => [k, decodeURIComponent(m[i + 1])]));
      current = { path, params };
      try { await r.handler(params); } catch (e) { console.error('[route]', path, e); }
      return;
    }
    current = { path, params: {} };
    notFound?.(path);
  }

  /* animate=false 的两种来路：首屏、手动刷新（同一路径再拉一遍，本来就没换页）。
     canTransition() 是第三道闸：API 不在 / 减弱动效 / 画布覆盖层正挂在屏幕上，任一成立就瞬切。 */
  async function resolve(animate) {
    const path = parse();
    gen++;
    if (!animate || first || NO_TRANSITION.test(path) || !canTransition()) {
      first = false;
      await swap(path);
      return;
    }
    hold(true);
    // 回调里的 await 会推迟"新快照"的抓取：数据没回来之前屏幕上还是上一页，回来才淡。
    // 拉得太久（>5s）浏览器会放弃这次过渡直接换页，那也是对的——过渡是锦上添花，不是流程。
    const t = document.startViewTransition(() => swap(path));
    try { await t.finished; } catch { /* 被后一次过渡顶掉，不是错误 */ }
    hold(false);
  }

  window.addEventListener('hashchange', () => resolve(true));

  return {
    on(pattern, handler) { routes.push({ ...compile(pattern), handler }); return this; },
    fallback(fn) { notFound = fn; return this; },
    start() { return resolve(false); },
    /** 重新拉取当前路由数据但不产生历史记录 */
    refresh() { return resolve(false); },
    get at() { return current; },
  };
}

export const go = (path, replace = false) => {
  const hash = '#' + (path.startsWith('/') ? path : '/' + path);
  if (location.hash === hash) return;
  replace ? history.replaceState(null, '', hash) : (location.hash = hash);
};

export const back = () => history.length > 1 ? history.back() : go('/');

/** 项目 / 编辑器路径构造器，避免各处手拼字符串 */
export const routeProject = id => `/p/${id}`;
export const routeEditor = (id, imgId) => `/p/${id}/e/${imgId}`;
