// 界面文案的唯一入口。中英各一份 JSON，放在 `public/locales/` 里，跟着整目录内嵌进 exe。
//
// 三条规矩（都是定过调的，别绕）：
//  · 一段话一个键。要带变量就写 `{name}` 占位，绝不在代码里拼句子——中英语序不同，
//    `预览失败：${x}` 这种拼法翻成英文就成了没法读的东西。
//  · 缺键不静默：找不到就回 ⟨键名⟩，让界面当场露洞，比悄悄退回另一种语言好查。
//  · 换语言 = 重新载入界面。视图是启动时一次性建好的，逐个重建比整体重载贵得多。
//
// 字典由调用方装进来（`use()`），这里不碰 fetch——R5 规定 fetch 只在 core/api.js。
'use strict';

/** 当前语言的字典；中文那份永远单独留着当兜底 */
let dict = {};
let zh = {};
let lang = 'zh';
const subs = new Set();

const leaf = (obj, key) => key.split('.').reduce((x, k) => (x && typeof x === 'object' && !Array.isArray(x) ? x[k] : undefined), obj);

const dig = key => {
  const hit = leaf(dict, key);
  return hit === undefined ? leaf(zh, key) : hit;
};

/** 取一条文案。args 走 `{name}` 插值 */
export function t(key, args) {
  const raw = dig(key);
  if (raw === undefined || raw === null) return `⟨${key}⟩`;
  if (typeof raw !== 'string') return raw;
  return args ? raw.replace(/\{(\w+)\}/g, (_, k) => String(args[k] ?? '')) : raw;
}

/** 取一组（导航项、笔刷名单这类成条目的） */
export function tl(key) {
  const v = dig(key);
  return Array.isArray(v) ? v : [];
}

/** 有没有这条：给"可选文案"用，缺了就当没有，而不是露出键名 */
export const has = key => dig(key) !== undefined;

export const getLang = () => lang;
export const isZh = () => lang === 'zh';

/**
 * 装上字典。zh 只装一次（它是所有缺键的兜底），其余语言切换时整体换掉。
 * `boot` 为 true 时不通知订阅者——启动阶段还没人订阅，通知是空转。
 */
export function use(nextLang, strings, { boot = false } = {}) {
  if (nextLang === 'zh') zh = strings;
  dict = strings;
  lang = nextLang;
  document.documentElement.lang = nextLang === 'zh' ? 'zh' : nextLang;
  document.title = t('app.title');
  if (!boot) for (const fn of subs) fn(nextLang);
}

/** 语言变了要做的事（目前只有"整体重载"那一条路，订阅者留给以后需要热替换的视图） */
export function watch(fn) {
  subs.add(fn);
  return () => subs.delete(fn);
}

/* 换语言要重载界面，而重载会带走还没落盘的东西——编辑器和涂遮罩那层把各自的 flush 挂到这里，
   切换流程在 reload 之前把它们挨个await掉。注册制而不是让设置弹窗去摸编辑器内部：那两层本来互不认识。 */
const preSwitch = new Set();

export function beforeSwitch(fn) {
  preSwitch.add(fn);
  return () => preSwitch.delete(fn);
}

export async function flushForSwitch() {
  for (const fn of [...preSwitch]) {
    try { await fn(); } catch { /* 存不上也不该拦住切换：界面本来就要重载 */ }
  }
}

/**
 * 静态节点的文案：`data-i18n`（文本）、`data-i18n-tip`、`data-i18n-aria`、`data-i18n-placeholder`。
 * 只在启动时对 `index.html` 那批跑一次；JS 建的节点直接用 t()。
 */
export function applyDom(root = document) {
  const scope = root || document;
  const put = (sel, attr, fn) => {
    for (const node of scope.querySelectorAll(sel)) {
      const key = node.getAttribute(attr);
      if (key) fn(node, t(key));
    }
  };
  put('[data-i18n]', 'data-i18n', (n, v) => { n.textContent = v; });
  put('[data-i18n-tip]', 'data-i18n-tip', (n, v) => { n.setAttribute('data-tip', v); });
  put('[data-i18n-aria]', 'data-i18n-aria', (n, v) => { n.setAttribute('aria-label', v); });
  put('[data-i18n-placeholder]', 'data-i18n-placeholder', (n, v) => { n.setAttribute('placeholder', v); });
}
