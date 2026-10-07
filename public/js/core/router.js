// Hash 路由：#/ 首页 · #/p/:id 项目 · #/p/:id/e/:imgId 编辑器
'use strict';

let gen = 0;    // 路由世代号：号一变就是用户已经走掉，慢回来的响应不该再往 store 里写

export const routeGen = () => gen;

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

  const parse = () => {
    const raw = location.hash.slice(1) || '/';
    return raw.startsWith('/') ? raw : '/' + raw;
  };

  async function resolve() {
    const path = parse();
    gen++;
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

  window.addEventListener('hashchange', resolve);

  return {
    on(pattern, handler) { routes.push({ ...compile(pattern), handler }); return this; },
    fallback(fn) { notFound = fn; return this; },
    start() { return resolve(); },
    /** 重新拉取当前路由数据但不产生历史记录 */
    refresh() { return resolve(); },
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
