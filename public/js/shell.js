// 外壳层：左侧栏导航 / 最近打开 / 顶栏右侧全局动作。视图无关，只认路由与回调
'use strict';
import { el, $, $$, fill } from './core/dom.js';
import { icon } from './core/icons.js';
import { go } from './core/router.js';
import { store, statOf } from './state.js';
import { mountWindowControls } from './core/desktop.js';
import { t } from './core/i18n.js';
import { api } from './core/api.js';
import { pendingCount, onTick } from './gen.js';

// 导航条目：href 与图标是代码，文字全在字典里（`nav.<k>`），换语言不用动这里
const NAV = [
  { href: '/',         ico: 'home',   k: 'home' },
  { href: '/f/all',    ico: 'folder', k: 'projects' },
  { href: '/f/nomask', ico: 'brush',  k: 'nomask' },
  { href: '/f/masked', ico: 'gauge',  k: 'masked' },
  { href: '/f/canvas', ico: 'canvas', k: 'canvas' },
];

let hooks = {};

export function initShell(opt) {
  hooks = opt;
  const nav = NAV.map(n => el('button.rail__link', {
    type: 'button', dataset: { href: n.href }, title: t(`nav.${n.k}`), 'aria-label': t(`nav.${n.k}`),
    onclick: () => go(n.href),
    html: icon(n.ico) + `<span>${t(`nav.${n.k}`)}</span>`,
  }));
  nav.push(el('button.rail__link', {
    type: 'button', 'data-tip': t('nav.helpTip'), title: t('nav.help'),
    html: icon('book') + `<span>${t('nav.help')}</span>`,
    onclick: () => hooks.onHelp?.(),
  }));
  fill($('#railNav'), nav);

  fill($('#topRight'),
    el('span.chip', { id: 'wfChip', 'data-tip': t('shell.workflowChip') }),
    el('button.btn.btn--ghost.btn--sm', { html: icon('refresh', { cls: 'icon icon--sm' }) + `<span>${t('shell.refresh')}</span>`, onclick: () => hooks.onRefresh?.() }),
    el('button.btn.btn--ghost.btn--icon.btn--sm', { 'aria-label': t('shell.settings'), 'data-tip': t('shell.settingsTip'), html: icon('sliders', { cls: 'icon icon--sm' }), onclick: () => hooks.onBackends?.() }),
    el('button.btn.btn--ghost.btn--icon.btn--sm', { 'aria-label': t('shell.helpAria'), 'data-tip': t('shell.helpTip'), html: icon('keyboard', { cls: 'icon icon--sm' }), onclick: () => hooks.onHelp?.() }),
  );
  // 无边框之后窗口控件得页面自己画；浏览器版里它是空操作
  mountWindowControls($('#topRight'));

  $('#searchInput').addEventListener('input', e => hooks.onSearch?.(e.target.value));
  initStatusBar();
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
  fill(host, (parts.length ? parts : [{ label: t('nav.home') }]).flatMap((p, i) => [
    i ? el('span.crumb__sep', { text: '/' }) : null,
    p.href
      ? el('button.crumb', { type: 'button', style: { border: '0', background: 'transparent', font: 'inherit', color: 'inherit', cursor: 'pointer', padding: '0' }, onclick: () => go(p.href), text: p.label })
      : el('b', { text: p.label }),
  ]));
}

/** 状态条只展示现有事实；队列请求失败不能伪装成空队列或后端已连接。 */
function initStatusBar() {
  const stats = el('span.workspace-status__stats');
  const backend = el('button.workspace-status__item', { type: 'button', onclick: () => hooks.onBackends?.() });
  const tracked = el('span.workspace-status__item');
  const queue = el('button.workspace-status__item', { type: 'button', onclick: () => hooks.onBackends?.('image') });
  const paint = () => {
    const projects = store.peek('projects');
    const images = projects.reduce((n, p) => n + (statOf(p.id)?.total ?? p.cnt ?? 0), 0);
    stats.textContent = store.peek('projectsAt')
      ? t('shell.statsLine', { projects: projects.length, images }) : t('shell.statsUnavailable');
    const cloud = store.peek('cloud');
    backend.textContent = cloud?.kind === 'cloud'
      ? t('chip.cloud', { model: cloud.model || t('chip.noModel'), size: cloud.size || t('chip.noSize') })
      : t('shell.comfyEntry', { host: store.peek('comfy') || t('shell.comfyOff') });
    backend.title = backend.textContent;
    tracked.textContent = t('shell.trackedTasks', { n: pendingCount() });
  };
  fill($('#workspaceStatus'), stats, backend, tracked, queue);
  store.subscribe(paint);
  onTick(paint);
  paint();
  let loading = false;
  const syncQueue = async () => {
    if (loading || document.hidden || document.documentElement.dataset.appearance !== 'glass') return;
    loading = true;
    try {
      const q = await api.queueState();
      queue.textContent = t('settings.image.queueNote', { queued: q.queued, running: q.running, cap: q.concurrency });
      queue.title = t('settings.image.queue');
    } catch { queue.textContent = t('shell.queueUnavailable'); }
    finally { loading = false; }
  };
  queue.textContent = t('shell.queueUnavailable');
  syncQueue();
  setInterval(syncQueue, 5000);
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

/** 左栏底部：修图流程小票 + 项目/图片总量 + 设置位（大卡、垫底）。
    两颗入口的位次与大小是用户定的：设置常驻最大最下面，教程收成一行小票。 */
export function renderRailFoot({ projects = 0, images = 0, masked = 0 } = {}) {
  const comfy = store.peek('comfy');
  fill($('#railFoot'),
    el('button.rail__comfy', {
      type: 'button', 'data-tip': t('shell.guideSub'), title: t('shell.guideSub'),
      onclick: () => hooks.onGuide?.(),
      html: icon('book', { cls: 'icon icon--sm' }) + `<span class="nowrap">${t('shell.guideTitle')}</span>`,
    }),
    el('div.status-line', {},
      el('span.dot', { class: `dot ${masked ? 'dot--mask' : ''}` }),
      el('span', { text: t('shell.statsLine', { projects, images }) })),
    el('button.promo', { type: 'button', 'data-tip': t('shell.footTip'), title: t('shell.settingsTip'), onclick: () => hooks.onBackends?.() },
      el('span.promo__ico', { html: icon('sliders', { cls: 'icon' }) }),
      el('span', {},
        el('b', { text: t('shell.settings') }),
        el('span', { style: { display: 'flex', alignItems: 'center', gap: '6px' } },
          el('span.dot', { class: `dot ${comfy ? 'dot--done' : 'dot--err'}` }),
          el('span', { text: (comfy || t('shell.comfyOff')).replace(/^https?:\/\//, '') })))),
  );
}

export function refreshStats() {
  const projects = store.peek('projects');
  let images = 0, masked = 0;
  for (const p of projects) { const s = statOf(p.id); images += s?.total ?? p.cnt ?? 0; masked += s?.masked ?? 0; }
  renderRailFoot({ projects: projects.length, images, masked });
}
