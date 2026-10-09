// 首页：英雄导入卡 + 三张信息卡 + 六张快捷卡 + 筛选头 + 拼贴封面项目网格
'use strict';
import { el, $, fill } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api, importPhotos, dropToFiles } from '../core/api.js';
import { t } from '../core/i18n.js';
import { store, loadProjects, setHome, statOf, setStat } from '../state.js';
import { go } from '../core/router.js';
import { toastOk, toastErr, toastBusy } from '../ui/toast.js';
import { confirm } from '../ui/modal.js';
import { emptyState } from '../ui/empty.js';
import { helpModal, flowModal, shortcutsModal, workflowModal } from '../ui/dialogs.js';
import { presetManager } from '../ui/presets.js';
import { newCanvas } from './canvas/index.js';
import { fmtStamp } from '../core/format.js';

const detail = new Map();        // projectId -> images[]
let sort = 'updated';            // updated | name | count
// 排序按钮的三个名字存键名：字典是 boot 里异步装的，模块级常量取文案只会拿到 ⟨键名⟩
const SORTS = { updated: 'home.sortUpdated', name: 'home.sortName', count: 'home.sortCount' };

/* ==================== 入口 ==================== */
export async function renderHome(filter) {
  if (filter) setHome({ filter });
  const busy = toastBusy(t('home.loading'));
  try { await loadProjects(); } catch (e) { busy.close(); toastErr(t('home.loadFail'), e.message); return; }
  busy.close();
  paint();
  hydrate().then(paint);           // 详情回来后再升级成拼贴封面 + 统计
}

/** 只按现有数据重画一次：搜索与筛选走这里，不该每敲一个字都去要一遍网络 */
export function repaintHome() { paint(); }

/* ==================== 数据 ==================== */
async function hydrate() {
  /* 缓存只用来"先画个有内容的版本"，每次进首页仍要全部重取：
     在编辑器里涂完遮罩回来，"已涂 X/Y" 与封面必须跟着变，不然首页一直在撒谎 */
  const need = store.peek('projects');
  await Promise.all(need.map(async p => {
    try {
      const d = await api.project(p.id);
      detail.set(p.id, d.images);
      setStat(p.id, {
        total: d.images.length,
        masked: d.images.filter(i => i.has_mask).length,
        sketches: d.images.filter(i => i.kind === 'sketch').length,
      });
    } catch { detail.set(p.id, []); }
  }));
}

const visible = () => {
  const { filter, query } = store.peek('home');
  const q = query.trim().toLowerCase();
  let list = store.peek('projects').filter(p => {
    if (q && !String(p.name).toLowerCase().includes(q)) return false;
    const s = statOf(p.id);
    if (filter === 'nomask') return !s || s.masked < s.total;
    if (filter === 'masked') return !!s && s.masked > 0;
    if (filter === 'canvas') return !!s && s.sketches > 0;
    return true;
  });
  const by = {
    updated: (a, b) => String(b.updated_at).localeCompare(String(a.updated_at)),
    name: (a, b) => String(a.name).localeCompare(String(b.name), 'zh'),
    count: (a, b) => (b.cnt || 0) - (a.cnt || 0),
  }[sort];
  return [...list].sort(by);
};

/* ==================== 渲染 ==================== */
function paint() {
  const host = $('#viewHome');
  fill(host,
    el('div.wrap', {},
      el('div.page-hd', {}, el('h1', { text: t('nav.home') }),
        el('span.sub', { text: t('home.sub') })),
      heroRow(),
      el('div.sec-hd', {}, el('h2', { text: t('home.quick') })),
      quickRow(),
      el('div.sec-hd', {}, el('h2', { text: t('home.projectsHd') })),
      listHd(),
      grid(),
    ));
}

/* ---------- 四卡行 ---------- */
function heroRow() {
  const cfg = store.peek('cfg') || {};
  return el('div.hero-row', {},
    heroCard(),
    tile('sliders', t('settings.sec.workflow'), t('home.wfSub'),
      () => workflowModal(cfg),
      el('div.tile-card__kv', {}, el('span', {}, `${t('home.kvSteps')} `, el('b', { text: String(cfg.steps ?? '—') })),
        el('span', {}, 'CFG ', el('b', { text: String(cfg.cfg ?? '—') })),
        el('span', {}, 'LoRA ', el('b', { text: String((cfg.loras || []).length) })))),
    tile('book', t('dlg.flow'), t('shell.guideSub'), () => flowModal()),
    tile('keyboard', t('dlg.shortcuts'), t('home.keysSub'), () => shortcutsModal()),
  );
}

function heroCard() {
  const name = el('input.input', { type: 'text', placeholder: t('home.namePh'), maxlength: '60' });
  const picker = el('input', { type: 'file', accept: 'image/*', multiple: true, hidden: true,
    onchange: e => importFiles(e.target.files, name.value) });

  const card = el('div.hero-card', {
    tabindex: '0', role: 'button', 'aria-label': t('home.importAria'),
    onclick: e => { if (!e.target.closest('input,button')) picker.click(); },
    onkeydown: e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); picker.click(); } },
    ondragover: e => { e.preventDefault(); card.classList.add('is-drop'); },
    // 扫过卡里的输入框和按钮也会抛 dragleave，直接摘 class 的话高亮会一路闪
    ondragleave: e => { if (!card.contains(e.relatedTarget)) card.classList.remove('is-drop'); },
    ondrop: async e => {
      e.preventDefault(); card.classList.remove('is-drop');
      importFiles(await dropToFiles(e.dataTransfer), name.value);
    },
  },
    el('span.hero-card__badge', { html: icon('plus', { cls: 'icon icon--lg' }) }),
    el('div.hero-card__t', { text: t('home.importAria') }),
    el('div.hero-card__d', { text: t('home.importD') }),
    el('div.hero-card__form', {}, name,
      el('button.btn.btn--primary.btn--sm', { type: 'button', html: icon('upload', { cls: 'icon icon--sm' }) + `<span>${t('home.pick')}</span>`, onclick: () => picker.click() })),
    el('div.hero-card__alts', {},
      el('button.btn.btn--ghost.btn--sm', {
        type: 'button', 'data-tip': t('home.emptyTip'),
        html: icon('folder', { cls: 'icon icon--sm' }) + `<span>${t('home.emptyBtn')}</span>`, onclick: () => createEmpty(name.value) }),
      el('button.btn.btn--ghost.btn--sm', {
        type: 'button', 'data-tip': t('home.canvasTip'),
        html: icon('canvas', { cls: 'icon icon--sm' }) + `<span>${t('home.canvasBtn')}</span>`, onclick: () => createCanvas() }),
    ),
    picker,
  );
  return card;
}

function tile(ico, ttl, sub, onclick, extra) {
  return el('button.tile-card', { type: 'button', onclick },
    el('span.tile-card__ico', { html: icon(ico, { cls: 'icon icon--lg' }) }),
    el('div.tile-card__t', { text: ttl }),
    el('div.tile-card__d', { text: sub }),
    extra || null);
}

/* ---------- 六张快捷卡 ---------- */
function quickRow() {
  const projects = store.peek('projects');
  const latest = projects[0];
  const cover = p => p?.cover_thumb_url || p?.cover_url;
  const unmasked = projects.map(p => ({ p, imgs: detail.get(p.id) })).find(x => x.imgs?.some(i => !i.has_mask));
  const withMask = projects.map(p => ({ p, imgs: detail.get(p.id) })).find(x => x.imgs?.some(i => i.has_mask));
  const doneJob = Object.entries(store.peek('jobs')).find(([, j]) => j.state === 'done');

  return el('div.quick-row', {},
    quick('right', t('home.qContinue'), latest ? latest.name : t('home.qNoProject'), cover(latest),
      () => latest && go(`/p/${latest.id}`), !latest),
    quick('brush', t('nav.nomask'), unmasked ? t('home.qUnmasked', { name: unmasked.p.name }) : t('home.qAllMasked'), cover(unmasked?.p),
      () => { if (!unmasked) return; store.set({ intent: 'pickUnmasked' }); go(`/p/${unmasked.p.id}`); }, !unmasked),
    quick('play', t('nav.masked'), withMask ? t('home.qMasked', { name: withMask.p.name }) : t('home.qNoneMasked'), cover(withMask?.p),
      () => { if (!withMask) return; store.set({ intent: 'selectMasked' }); go(`/p/${withMask.p.id}`); }, !withMask),
    quick('compare', t('home.qCompare'), t(doneJob ? 'home.qBackToLast' : 'home.qNoResult'), null,
      () => { if (!doneJob) return; const img = findImage(+doneJob[0]); if (img) go(`/p/${img.project_id}/e/${img.id}`); }, !doneJob),
    quick('cpu', t('settings.sec.workflow'), t('home.qWfSub'), null, () => workflowModal(store.peek('cfg') || {})),
    quick('sparkles', t('settings.sec.presets'), t('home.qPresetSub'), null, () => presetManager()),
    quick('keyboard', t('dlg.shortcuts'), t('home.qKeysSub'), null, () => shortcutsModal()),
  );
}

/* detail 的键就是项目 id：后端 images 里没有 project_id，从这里补 */
function findImage(imgId) {
  for (const [pid, imgs] of detail) { const hit = imgs.find(i => i.id === imgId); if (hit) return { ...hit, project_id: pid }; }
  return null;
}

function quick(ico, ttl, sub, thumb, onclick, off) {
  return el('button.quick-card', { type: 'button', class: `quick-card${off ? ' is-off' : ''}`, disabled: !!off, onclick },
    el('span.quick-card__txt', {},
      el('span.quick-card__ico', { html: icon(ico, { cls: 'icon' }) }),
      el('span.quick-card__t', { text: ttl })),
    el('span.quick-card__thumb', { class: `quick-card__thumb${thumb ? '' : ' quick-card__thumb--art'}` },
      thumb ? el('img', { src: thumb, alt: '', loading: 'lazy' })
            : el('span.ic', { html: icon(ico, { cls: 'icon icon--xl' }) })),
    el('span.sr-only', { text: sub }),
  );
}

/* ---------- 筛选头 ---------- */
function listHd() {
  const { filter } = store.peek('home');
  const all = store.peek('projects').length;
  const shown = visible().length;
  const tabs = [['all', 'nav.projects'], ['nomask', 'nav.nomask'], ['masked', 'nav.masked'], ['canvas', 'nav.canvas']];
  return el('div.list-hd', {},
    ...tabs.map(([k, key]) => el('button.tab', {
      type: 'button', class: `tab${filter === k ? ' is-on' : ''}`, text: t(key),
      onclick: () => { setHome({ filter: k }); paint(); },
    })),
    el('div.list-hd__tools', {},
      // 这两个 html: 里进的只有整数与字典常量，没有用户文本；数字加粗靠标签
      el('span.chip', { html: t('home.countChip', { shown, all }) }),
      el('button.chip', { type: 'button', html: icon('sort', { cls: 'icon icon--sm' }) + `<span>${t(SORTS[sort])}</span>`,
        onclick: e => { sort = { updated: 'name', name: 'count', count: 'updated' }[sort]; paint(); e.currentTarget.blur(); } }),
      el('button.chip', { type: 'button', html: icon('refresh', { cls: 'icon icon--sm' }) + `<span>${t('shell.refresh')}</span>`,
        onclick: async () => { detail.clear(); await renderHome(); toastOk(t('home.refreshed')); } }),
    ),
  );
}

/* ---------- 项目网格 ---------- */
function grid() {
  const list = visible();
  if (!list.length) {
    const blank = !store.peek('projects').length;
    if (store.peek('home').filter === 'canvas') {
      return emptyState('canvas', t('home.noCanvas'), t('home.canvasEmptySub'),
        el('button.btn.btn--primary', { type: 'button', html: icon('canvas', { cls: 'icon icon--sm' }) + `<span>${t('home.canvasBtn')}</span>`, onclick: () => createCanvas() }));
    }
    return emptyState('photos',
      t(blank ? 'home.noProject' : 'home.noMatch'),
      t(blank ? 'home.importHint' : 'home.tryFilter'),
      blank ? el('div.hero-card__form', { style: { width: 'auto', marginTop: '0' } },
        el('button.btn.btn--primary', { type: 'button', html: icon('upload', { cls: 'icon icon--sm' }) + `<span>${t('home.importBtn')}</span>`, onclick: () => $('.hero-card')?.click() }),
        el('button.btn.btn--ghost', { type: 'button', html: icon('folder', { cls: 'icon icon--sm' }) + `<span>${t('home.emptyBtn')}</span>`, onclick: () => createEmpty('') })) : null);
  }
  return el('div.pgrid', {}, list.map((p, i) => pcard(p, i)));
}

function pcard(p, i) {
  const imgs = detail.get(p.id) || [];
  const s = statOf(p.id) || { total: p.cnt || imgs.length, masked: 0 };
  const cov = el('div.pcard__cov', {}, ...collage(p, imgs),
    el('span.pcard__badge', {
      html: s.masked && s.masked < s.total
        ? t('home.badgeMasked', { masked: s.masked, total: s.total })
        : t('home.badgeAll', { total: s.total }),
    }),
    el('span.pcard__prog', {}, el('i', { style: { width: `${s.total ? (s.masked / s.total) * 100 : 0}%` } })),
    el('button.pcard__del', {
      type: 'button', 'aria-label': t('home.delAria'), 'data-tip': t('home.delAria'),
      html: icon('trash', { cls: 'icon icon--sm' }),
      onclick: async e => { e.stopPropagation(); await removeProject(p); },
    }),
  );
  return el('div.pcard', {
    style: { '--i': String(i) }, tabindex: '0', role: 'button',
    onclick: () => go(`/p/${p.id}`),
    onkeydown: e => { if (e.key === 'Enter') go(`/p/${p.id}`); },
  },
    cov,
    el('div.pcard__nm.nowrap', { text: p.name }),
    el('div.pcard__meta', {}, el('span', { text: t('home.updatedAt', { when: fmtStamp(p.updated_at) }) })),
  );
}

/** 拼贴封面：左 1 竖 + 右 2 横，缺图用斜纹占位。小档由服务端切，卡片不再解码原图 */
function collage(p, imgs) {
  const src = [imgs[0]?.thumb_url || imgs[0]?.orig_url || p.cover_thumb_url || p.cover_url, imgs[1]?.thumb_url || imgs[1]?.orig_url, imgs[2]?.thumb_url || imgs[2]?.orig_url];
  const ph = () => el('div.ph');
  const im = u => (u ? el('img', { src: u, alt: '', loading: 'lazy' }) : ph());
  return [im(src[0]), el('div', {}, im(src[1]), im(src[2]))];
}

/* ==================== 动作 ==================== */
/** 画布是图生图那条线：建完直接进画布视图，项目归属由服务端顺手建好 */
async function createCanvas() {
  const r = await newCanvas({ projectId: null });
  if (!r) return;
  toastOk(t('home.canvasDone'), t('home.canvasDoneBody'));
  go(`/p/${r.project_id}/c/${r.image_id}`);
}

/** 空项目：一张照片都没有也能先把坑占下，进去后再导入 */
async function createEmpty(name) {
  try {
    const r = await api.createProject((name || '').trim() || defaultName(), []);
    detail.set(r.id, []);
    setStat(r.id, { total: 0, masked: 0 });
    toastOk(t('home.emptyDone'), t('home.emptyDoneBody', { name: r.name }));
    go(`/p/${r.id}`);
  } catch (e) { toastErr(t('home.createFail'), e.message); }
}

async function importFiles(files, name) {
  const busy = toastBusy(t('home.importBusy'));
  try {
    const r = await importPhotos(files, {
      name: (name || '').trim() || defaultName(),
      onProgress: (done, total) => busy.update({ msg: t('home.importProg', { done, total }) }),
    });
    busy.close();
    if (!r.image_ids.length) { toastErr(t('home.noImages'), t('home.supported')); return; }
    toastOk(t('home.importDone'), r.skipped
      ? t('home.importDoneSkipped', { n: r.image_ids.length, skipped: r.skipped })
      : t('home.importDone', { n: r.image_ids.length }));
    detail.delete(r.id);
    go(`/p/${r.id}`);
  } catch (e) { busy.close(); toastErr(t('home.importFail'), e.message); }
}

const defaultName = () => {
  const d = new Date();
  return t('home.defaultName', { date: `${String(d.getMonth() + 1).padStart(2, '0')}${String(d.getDate()).padStart(2, '0')}` });
};

async function removeProject(p) {
  const ok = await confirm({
    title: t('home.delTitle', { name: p.name }),
    html: el('div', {},
      el('p', { text: t('home.delBody', { n: p.cnt || 0 }) }),
      el('p', { style: { color: 'var(--danger)', fontSize: '12px', marginTop: '8px' }, text: t('home.delWarn') })),
    danger: true, okLabel: t('home.delAria'),
  });
  if (!ok) return;
  try {
    await api.deleteProject(p.id);
    detail.delete(p.id);
    toastOk(t('home.deleted'), p.name);
    await renderHome();
  } catch (e) { toastErr(t('home.delFail'), e.message); }
}

export { helpModal };
