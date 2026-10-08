// 桌面壳通道：只有 Tauri 注入的窗口里才有 window.__TAURI__。
// 浏览器里跑（cargo run 起的本机服务）时这些一律返回"不是桌面版"，调用方要自己降级。
'use strict';
import { el } from './dom.js';

const tauri = () => (typeof window !== 'undefined' ? window.__TAURI__ : null);

export const isDesktop = () => !!tauri()?.core?.invoke;

/** 调一个壳里注册的命令；不在桌面版就抛，调用方按"功能不可用"处理 */
export async function call(cmd, args) {
  const t = tauri();
  if (!t?.core?.invoke) throw new Error('这一步要桌面版：用 Synco 打开，或启动本地服务后用浏览器页面');
  return t.core.invoke(cmd, args || {});
}

/** 系统文件夹选择框；取消返回 null */
export async function pickFolder(title) {
  const d = tauri()?.dialog;
  if (!d) throw new Error('要桌面版才能弹系统目录框');
  const p = await d.open({ directory: true, multiple: false, title });
  return p || null;
}

/**
 * 外链一律交给系统浏览器：这个窗口跳去 GitHub 就等于把工坊关掉了。
 * 桌面版走壳里的 `open_url`（它只接逐字符过白名单的 http/https，且不经 `cmd`），
 * 浏览器版退回 `window.open`。
 * 返回 false = 没打开成，调用方要把地址本身说给用户，别静默失败。
 */
export async function openExternal(url) {
  if (isDesktop()) {
    try { await call('open_url', { url }); return true; } catch { return false; }
  }
  return !!window.open(url, '_blank', 'noopener');
}

/** 字节数说人话：设置页要说清"要搬走多大一坨" */
export function human(n) {
  if (!(n > 0)) return '—';
  const u = ['B', 'KB', 'MB', 'GB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v >= 10 || i === 0 ? Math.round(v) : v.toFixed(1)} ${u[i]}`;
}

const glyph = inner => `<svg class="icon icon--sm" viewBox="0 0 12 12" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.15">${inner}</svg>`;

/**
 * 无边框窗口的三颗键，画进给定右槽；浏览器版直接不动（拿不到 __TAURI__.window）。
 * 拖动与双击最大化由壳注入的 data-tauri-drag-region 处理，这里只管三颗键。
 */
export function mountWindowControls(host) {
  const W = tauri()?.window;
  if (!host || !W?.getCurrentWindow) return;
  const w = W.getCurrentWindow();
  const btn = (label, inner, onclick, mod = '') => el(`button.wctl__b${mod}`, {
    type: 'button', 'aria-label': label, 'data-tip': label, html: glyph(inner), onclick,
  });
  const max = btn('最大化', '<rect x="2.4" y="2.4" width="7.2" height="7.2" rx=".8"/>', () => w.toggleMaximize());
  host.append(el('div.wctl', {},
    btn('最小化', '<path d="M2.3 9h7.4"/>', () => w.minimize()),
    max,
    btn('关闭', '<path d="m2.7 2.7 6.6 6.6M9.3 2.7 2.7 9.3"/>', () => w.close(), '.wctl__b--close'),
  ));
  // 最大化时图标要换成"还原"那对错开的方角，否则第二下点下去没人知道会发生什么
  const sync = () => w.isMaximized().then(on => {
    max.innerHTML = glyph(on
      ? '<path d="M4.4 3.2h4.4v4.4"/><path d="M3.2 4.4h4.4v4.4H3.2z"/>'
      : '<rect x="2.4" y="2.4" width="7.2" height="7.2" rx=".8"/>');
    const label = on ? '还原' : '最大化';
    max.setAttribute('aria-label', label);
    max.dataset.tip = label;
  }).catch(() => {});
  sync();
  w.onResized(sync).catch(() => {});
}
