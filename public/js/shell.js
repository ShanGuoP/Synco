// 外壳层：左侧栏导航 / 最近打开 / 顶栏右侧全局动作。视图无关，只认路由与回调
'use strict';
import { el, $, $$, fill } from './core/dom.js';
import { icon } from './core/icons.js';
import { go } from './core/router.js';
import { store, statOf } from './state.js';
import { mountWindowControls } from './core/desktop.js';

const NAV = [
  { href: '/',         ico: 'home',   label: '主页' },
  { href: '/f/all',    ico: 'folder', label: '全部项目' },
  { href: '/f/nomask', ico: 'brush',  label: '待涂遮罩' },
  { href: '/f/masked', ico: 'gauge',  label: '已涂待提交' },
];

let hooks = {};

export function initShell(opt) {
  hooks = opt;
  const nav = NAV.map(n => el('button.rail__link', {
    type: 'button', dataset: { href: n.href },
    onclick: () => go(n.href),
    html: icon(n.ico) + `<span>${n.label}</span>`,
  }));
  nav.push(el('button.rail__link', {
    type: 'button', 'data-tip': '快捷键见问号键',
    html: icon('book') + '<span>帮助与快捷键</span>',
    onclick: () => hooks.onHelp?.(),
  }));
  fill($('#railNav'), nav);

  fill($('#topRight'),
    el('span.chip', { id: 'wfChip', 'data-tip': '来自工作流的默认采样参数' }),
    el('button.btn.btn--ghost.btn--sm', { html: icon('refresh', { cls: 'icon icon--sm' }) + '<span>刷新</span>', onclick: () => hooks.onRefresh?.() }),
    el('button.btn.btn--ghost.btn--icon.btn--sm', { 'aria-label': '设置', 'data-tip': '后端 / 工作流 / ComfyUI 目录', html: icon('sliders', { cls: 'icon icon--sm' }), onclick: () => hooks.onBackends?.() }),
    el('button.btn.btn--ghost.btn--icon.btn--sm', { 'aria-label': '帮助', 'data-tip': '快捷键', html: icon('keyboard', { cls: 'icon icon--sm' }), onclick: () => hooks.onHelp?.() }),
  );
  // 无边框之后窗口控件得页面自己画；浏览器版里它是空操作
  mountWindowControls($('#topRight'));

  $('#searchInput').addEventListener('input', e => hooks.onSearch?.(e.target.value));
  document.addEventListener('keydown', e => {
    if (e.key === '/' && !/^(INPUT|TEXTAREA)$/.test(document.activeElement?.tagName)) {
      e.preventDefault(); $('#searchInput').focus();
    }
  });
}

export function setWorkflowChip(text) {
  const chip = $('#wfChip');
  if (!chip) return;
  /* 图标是我们自己编译期的常量，走 innerHTML 没问题；
     文字里有用户手填的模型名/挡位（app.js 从 /api/cloud 拿的），只能走 textContent */
  chip.innerHTML = icon('cpu', { cls: 'icon icon--sm' });
  chip.append(el('span', { text: String(text ?? '') }));
  // 收成一行带省略号之后，全文只能靠原生 title 找回
  chip.title = String(text ?? '');
}

export function setActiveNav(path) {
  for (const b of $$('#railNav .rail__link')) {
    b.classList.toggle('is-active', b.dataset.href === path || (path === '/' && b.dataset.href === '/'));
  }
}

/** 顶栏左侧面包屑；传 onBack 则出现返回按钮 */
export function setCrumb(parts = []) {
  const host = $('#topLeft');
  if (!host) return;
  fill(host, parts.flatMap((p, i) => [
    i ? el('span.crumb__sep', { text: '/' }) : null,
    p.href
      ? el('button.crumb', { type: 'button', style: { border: '0', background: 'transparent', font: 'inherit', color: 'inherit', cursor: 'pointer', padding: '0' }, onclick: () => go(p.href), text: p.label })
      : el('b', { text: p.label }),
  ]));
}

/** 左栏「最近打开」：最多 5 项，带封面 */
export function renderRecent(projects) {
  const list = projects.slice(0, 5);
  $('#railRecentLabel').hidden = !list.length;
  fill($('#railRecent'), list.map(p => el('button.rail__recent', {
    type: 'button', title: p.name, onclick: () => go(`/p/${p.id}`),
  },
    p.cover_url ? el('img.cov', { src: p.cover_url, alt: '' }) : el('span.cov'),
    el('span.nowrap', { text: p.name }),
  )));
}

/** 左栏底部：设置入口（当前后端）+ 项目/图片总量 + 教程位 */
export function renderRailFoot({ projects = 0, images = 0, masked = 0 } = {}) {
  const comfy = store.peek('comfy') || '未连接';
  fill($('#railFoot'),
    el('button.rail__comfy', { type: 'button', 'data-tip': '后端 / 工作流 / ComfyUI 目录 / 预设', onclick: () => hooks.onBackends?.() },
      el('span.dot', { class: `dot ${store.peek('comfy') ? 'dot--done' : 'dot--err'}` }),
      el('span.nowrap', { text: '设置 · ' + comfy.replace(/^https?:\/\//, '') })),
    el('div.status-line', {},
      el('span.dot', { class: `dot ${masked ? 'dot--mask' : ''}` }),
      el('span', { text: `${projects} 个项目 · ${images} 张图` })),
    el('button.promo', { type: 'button', onclick: () => hooks.onGuide?.() },
      el('span.promo__ico', { html: icon('book', { cls: 'icon' }) }),
      el('span', {}, el('b', { text: '修图流程' }), el('span', { text: '导入 → 涂抹 → 提交 → 对比' }))),
  );
}

export function refreshStats() {
  const projects = store.peek('projects');
  let images = 0, masked = 0;
  for (const p of projects) { const s = statOf(p.id); if (s) { images += s.total; masked += s.masked; } }
  renderRailFoot({ projects: projects.length, images, masked });
}
