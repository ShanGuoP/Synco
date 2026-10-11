// 桌面壳垫片（回退期）：Tauri 壳已随 Flutter 迁移移除，这里只保留原来的导出签名，
// 让旧前端的调用方原样工作——isDesktop 恒为 false，所有桌面分支自然走浏览器降级路径。
// Flutter 宿主落地后，整个旧前端连同本文件一起退役。
'use strict';
import { t as tr } from './i18n.js';

export const isDesktop = () => false;

/** 壳命令已不存在：真被调到就抛，调用方按"功能不可用"处理（正常都会被 isDesktop 拦在前面） */
export async function call() {
  throw new Error(tr('desktop.needDesktop'));
}

/** 系统文件夹选择框：浏览器页面弹不了，直接抛 */
export async function pickFolder() {
  throw new Error(tr('desktop.needFolder'));
}

/**
 * 外链交给系统浏览器：这个窗口跳去 GitHub 就等于把工坊关掉了，所以用新开标签。
 * 返回 false = 没打开成，调用方要把地址本身说给用户，别静默失败。
 */
export async function openExternal(url) {
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
