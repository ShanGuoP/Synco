// 项目页（图库）：批量操作条 + 筛选头 + 图片网格
// 卡片状态来自 state.jobs，生成完成后原地重绘而不是整页刷新
'use strict';
import { el, $, $$, fill, raf } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api, importPhotos } from '../core/api.js';
import { store, loadProject, saveSettings, selectWhere, clearSel, invertSel, toggleSel, isSel, stateOf, isCloud, cloudPrompt } from '../state.js';
import { go, back, routeGen } from '../core/router.js';
import { setCrumb } from '../shell.js';
import { toastOk, toastErr, toastBusy } from '../ui/toast.js';
import { confirm, askName, modal } from '../ui/modal.js';
import { newCanvas } from './canvas/index.js';
import { emptyState } from '../ui/empty.js';
import { submit, adopt } from '../gen.js';
import { fmtStamp, fmtFile, fmtDims } from '../core/format.js';
import { t } from '../core/i18n.js';

let filter = 'all';
let first = true;

/* ==================== 入口 ==================== */
export async function renderProject(id) {
  const g = routeGen();
  const busy = toastBusy(t('pj.openBusy'));
  try { await loadProject(id); } catch (e) { busy.close(); toastErr(t('pj.openFail'), e.message); go('/'); return; }
  busy.close();
  if (g !== routeGen()) return;              // 这一页已经被切走了，别再重绘别人的网格
  applyIntent();
  first = true;
  paint();
}

/** 首页快捷卡带过来的意图 */
function applyIntent() {
  const intent = store.peek('intent');
  if (!intent) return;
  store.set({ intent: null });
  if (intent === 'selectMasked') { selectWhere(i => i.has_mask); toastOk(t('pj.selected', { n: store.peek('sel').length }), t('pj.selectedBody')); }
  if (intent === 'pickUnmasked') { filter = 'nomask'; }
}

/* ==================== 渲染 ==================== */
function paint() {
  const { project, images } = store.get();
  if (!project) return;
  $('#viewProject').classList.toggle('no-in', !first);

  fill($('#viewProject'),
    el('div.wrap', {},
      header(project, images),
      el('div.sec-hd', {}, el('h2', { text: t('pj.batchHd') })),
      bulkRow(images),
      listHd(images),
      grid(images),
    ));
  first = false;
}

/* 画布行也在项目页里，但"未涂/已涂遮罩"这两个口径只对照片成立 */
const isPhoto = i => i.kind !== 'sketch';

function header(p, images) {
  const masked = images.filter(i => i.has_mask).length;
  return el('div.pj-hd', {},
    el('button.btn.btn--ghost.btn--icon', { type: 'button', 'aria-label': t('pj.backHome'), 'data-tip': t('pj.backHome'), html: icon('left'), onclick: () => go('/') }),
    el('div', {},
      el('button.pj-hd__t', {
        type: 'button', 'data-tip': t('pj.rename'), 'aria-label': t('pj.renameTip', { name: p.name }),
        onclick: () => renameProject(),
      }, el('span.pj-hd__n', { text: p.name }), el('span.ic', { html: icon('edit', { cls: 'icon icon--sm' }) })),
      el('div.pj-hd__meta', {},
        el('span', { text: t('pj.countN', { n: images.length }) }), el('span.muted', { text: '·' }),
        el('span', { text: t('pj.maskedN', { n: masked }) }), el('span.muted', { text: '·' }),
        el('span', { text: t('home.updatedAt', { when: fmtStamp(p.updated_at) }) }))),
    el('div.pj-hd__tools', {},
      importBtn(),
      el('button.btn.btn--ghost', { type: 'button', html: icon('canvas', { cls: 'icon icon--sm' }) + `<span>${t('home.canvasBtn')}</span>`, onclick: newCanvasHere }),
      el('button.btn.btn--ghost', { type: 'button', html: icon('check', { cls: 'icon icon--sm' }) + `<span>${t('pj.selectMasked')}</span>`, onclick: () => { selectWhere(i => i.has_mask); paint(); } }),
      el('button.btn.btn--primary', { type: 'button', id: 'pjSubmit', disabled: submitting || !store.peek('sel').length, html: icon('play', { cls: 'icon icon--sm' }) + `<span>${t('pj.submitSel')}</span><b id="pjSelN">${store.peek('sel').length}</b>`, onclick: submitSelected }),
    ));
}

function importBtn() {
  const picker = el('input', { type: 'file', id: 'pjPicker', accept: 'image/*', multiple: true, hidden: true,
    onchange: e => appendFiles(e.target.files) });
  return el('label.btn.btn--ghost', { 'data-tip': t('pj.appendTip') },
    el('span.ic', { html: icon('upload', { cls: 'icon icon--sm' }) }), el('span', { text: t('pj.appendBtn') }), picker);
}

function bulkRow(images) {
  const sel = store.peek('sel');
  const b = (ico, label, n, onclick, dis, cls = '') => el('button.bulk', {
    type: 'button', class: `bulk ${cls}`, disabled: !!dis, onclick,
  }, el('span.ic', { html: icon(ico, { cls: 'icon icon--sm' }) }), el('span', { text: label }),
     n != null ? el('b', { text: String(n) }) : null);

  return el('div.bulk-row', {},
    b('check', t('pj.selectMasked'), images.filter(i => isPhoto(i) && i.has_mask).length, () => { selectWhere(i => isPhoto(i) && i.has_mask); paint(); }),
    b('brush', t('pj.bulkUnmasked'), images.filter(i => isPhoto(i) && !i.has_mask).length, () => { selectWhere(i => isPhoto(i) && !i.has_mask); paint(); }),
    b('images', t('film.invert'), sel.length, () => { invertSel(); paint(); }),
    b('close', t('pj.clearSelBtn'), null, () => { clearSel(); paint(); }, !sel.length),
    b('trash', t('pj.delSel'), null, () => removeSelected(), !sel.length),
    b('play', t('pj.submitSel'), sel.length, submitSelected, !sel.length || submitting, 'bulk--go'),
  );
}

function listHd(images) {
  const n = {
    all: images.length,
    nomask: images.filter(i => isPhoto(i) && !i.has_mask).length,
    masked: images.filter(i => isPhoto(i) && !!i.has_mask).length,
    done: images.filter(i => i.result_done > 0).length,
  };
  /* 结果数由项目详情一次算出来（result_count / result_done），刷新后也认得"这张出过图" */
  const tabs = [['all', 'hist.all'], ['nomask', 'status.nomask'], ['masked', 'pj.tabMasked'], ['done', 'status.done']];
  return el('div.list-hd', {},
    ...tabs.map(([k, label]) => el('button.tab', {
      type: 'button', class: `tab${filter === k ? ' is-on' : ''}`,
      html: `${t(label)} <i class="tab-count">${n[k]}</i>`,
      onclick: () => { filter = k; paint(); },
    })),
    el('div.list-hd__tools', {},
      el('span.chip', { html: t('pj.selChip', { n: store.peek('sel').length }) }),
      el('button.chip', { type: 'button', html: icon('refresh', { cls: 'icon icon--sm' }) + `<span>${t('shell.refresh')}</span>`,
        onclick: async () => {
          /* 项目在别的窗口被删掉时，这里不兜住就停在半重绘的网格上，只剩一句通用"请求失败" */
          try { await loadProject(store.peek('project').id); } catch (e) { toastErr(t('pj.refreshFail'), e.message); return; }
          paint(); toastOk(t('home.refreshed'));
        } }),
    ),
  );
}

const passFilter = i => ({
  all: true,
  nomask: isPhoto(i) && !i.has_mask,
  masked: isPhoto(i) && !!i.has_mask,
  done: i.result_done > 0,
}[filter]);

function grid(images) {
  const list = images.filter(passFilter);
  if (!list.length) {
    return emptyState(images.length ? 'brush' : 'folder',
      images.length ? t('pj.noImages') : t('pj.emptyProject'),
      images.length ? t('pj.noImagesBody') : t('pj.emptyBody'),
      images.length
        ? el('button.btn.btn--ghost', { type: 'button', text: t('pj.showAll'), onclick: () => { filter = 'all'; paint(); } })
        : el('button.btn.btn--primary', { type: 'button', html: icon('upload', { cls: 'icon icon--sm' }) + `<span>${t('home.importBtn')}</span>`, onclick: () => $('#pjPicker')?.click() }));
  }
  return el('div.igrid', { id: 'igrid' }, list.map((i, n) => icard(i, n)));
}

function icard(img, i) {
  const st = stateOf(img);
  const sketch = img.kind === 'sketch';
  const job = store.peek('jobs')[img.id];
  const card = el('div.icard', {
    class: `icard${isSel(img.id) ? ' is-sel' : ''}${sketch ? ' is-sketch' : ''}`,
    style: { '--i': String(Math.min(i, 24)) },
    tabindex: '0', role: 'button', dataset: { id: String(img.id) },
    onclick: e => { if (e.target.closest('.icard__cb, .icard__acts, .icard__derived')) return; openCard(img); },
    onkeydown: e => { if (e.key === 'Enter') openCard(img); },
  },
    el('div.icard__cov', {},
      img.orig_dead ? el('span.icard__gone', { text: t('film.lostFile') })
                    : sketch ? el('img.icard__sketch', { src: img.orig_url, alt: img.name, loading: 'lazy', decoding: 'async' })
                    : el('img', { src: img.thumb_url || img.orig_url, alt: img.name, loading: 'lazy', decoding: 'async' }),
      // 蒙版是原地覆写的文件，服务端对它发 ETag + no-cache，判新交给浏览器条件请求，不再自己拼 ?t=
      img.mask_url ? el('img.icard__mask', { src: img.mask_url, alt: '', 'aria-hidden': 'true' }) : null,
      st === 'run' ? el('span.icard__run', {}, el('i')) : null,
      el('button.icard__cb', {
        type: 'button', 'aria-label': t('pj.pickAria'), 'data-tip': t('pj.pickTip'),
        html: icon('check', { cls: 'icon icon--sm' }),
        onclick: e => { e.stopPropagation(); toggleSel(img.id); syncSel(); },
      }),
      // 派生查看：常驻角标，不放进 .icard__acts（那块在窄屏整块 display:none，手机上就点不到了）
      // 两个数分别是「出过几张成图」与「另存出去几张子图」——它们是两件事，别混成一个
      img.result_done > 0 || img.derived_count > 0 ? el('button.icard__derived', {
        type: 'button',
        'aria-label': t('pj.resAria', { done: img.result_done || 0, derived: img.derived_count || 0 }),
        'data-tip': t('pj.resTip', { done: img.result_done || 0, derived: img.derived_count || 0 }),
        html: `${icon('layers', { cls: 'icon icon--sm' })}<b>${img.result_done || 0}</b>`
          + (img.derived_count > 0 ? `${icon('copy', { cls: 'icon icon--sm' })}<b>${img.derived_count}</b>` : ''),
        onclick: e => { e.stopPropagation(); showDerived(img); },
      }) : null,
      el('div.icard__acts', {},
        el('button.btn.btn--primary.btn--sm', { type: 'button', html: icon(sketch ? 'canvas' : 'brush', { cls: 'icon icon--sm' }) + `<span>${sketch ? t('pj.openCanvas') : img.has_mask ? t('pj.keepPaint') : t('pj.paint')}</span>`, onclick: e => { e.stopPropagation(); openCard(img); } }),
        el('button.btn.btn--danger.btn--icon.btn--sm', { type: 'button', 'aria-label': t('pj.delImg'), html: icon('trash', { cls: 'icon icon--sm' }), onclick: e => { e.stopPropagation(); removeImage(img); } }),
      ),
      el('span.icard__badge', {
        class: `icard__badge st-${st}`,
        html: st === 'done' ? `<i>${t('status.done')}</i>` : st === 'err' ? t('status.err') : st === 'run' ? t('status.run') : sketch ? t('nav.canvas') : fmtDims(img.w, img.h),
      }),
    ),
    el('div.icard__ft', {},
      el('span.nm', { title: img.name, text: fmtFile(img.name, 22) }),
      el('span', { text: sketch ? `${img.w}×${img.h}` : img.has_mask ? t('film.masked') : t('pj.unpainted') }),
    ),
  );
  return card;
}

/* 只重绘选择相关的部分，避免整页重排 */
function syncSel() {
  const n = store.peek('sel').length;
  const badge = $('#pjSelN'); if (badge) badge.textContent = String(n);
  const chips = $$('.list-hd__tools .chip b'); if (chips[0]) chips[0].textContent = String(n);
  for (const c of $$('.icard')) c.classList.toggle('is-sel', isSel(+c.dataset.id));
  const goBtn = $('#pjSubmit'); if (goBtn) goBtn.disabled = !n || submitting;
}

/* ==================== 动作 ==================== */
/** 画稿与照片走两个视图：画布没有"蒙版外要保持"这回事，别拿修图界面开它 */
const openCard = img => {
  const p = store.peek('project');
  if (p) go(img.kind === 'sketch' ? `/p/${p.id}/c/${img.id}` : `/p/${p.id}/e/${img.id}`);
};

/**
 * 派生查看弹窗：这张图「另存为新图」出去的那些**子图**。
 * 生成历史不在这里——那是编辑器右侧那一列，两件事别再混成一个入口。
 * 面板只做看/打开/删（方案 D12），提交留在编辑器里。
 */
async function showDerived(img) {
  const p = store.peek('project');
  if (!p) return;
  const counter = el('span.muted', { text: t('pj.reading') });
  const listBox = el('div.pre-list', {});
  async function reload() {
    let d;
    try {
      d = await api.imageDerived(img.id);
    } catch (e) {
      counter.textContent = t('pj.derivedFail');
      fill(listBox, el('p.muted', { style: { padding: '12px 6px' }, text: String(e.message || e) }));
      return;
    }
    const rows = d.images || [];
    counter.textContent = rows.length
      ? t('pj.derivedMeta', { n: rows.length, own: img.result_done || 0 })
      : t('pj.derivedNone');
    fill(listBox, rows.length ? rows.map(kid => el('div.pre-it', {
      role: 'button', tabindex: '0',
      onclick: () => { handle?.close('open'); openCard(kid); },
      onkeydown: e => { if (e.key === 'Enter') { handle?.close('open'); openCard(kid); } },
    },
      kid.orig_dead ? el('span.pre-it__ph.pre-it__ph--gone')
                    : el('img.pre-it__th', { src: kid.thumb_url || kid.orig_url, alt: '', loading: 'lazy' }),
      el('span.pre-it__tx', {},
        el('b', { text: kid.name }),
        el('span.pre-it__meta', { text: t('pj.derivedFrom', { dims: fmtDims(kid.w, kid.h), id: kid.derived_result || '?' }), title: kid.name })),
      el('span.pre-it__acts', {},
        el('span.pre-it__btn', { text: t('pj.open') }),
        el('button.pre-it__btn', {
          type: 'button', title: t('pj.delDerivedTip'),
          html: icon('trash', { cls: 'icon icon--sm' }),
          onclick: e => { e.stopPropagation(); delDerived(kid, reload); },
        })),
    )) : el('p.muted', { style: { padding: '14px 6px', lineHeight: '1.7' },
      text: t('pj.derivedExplain') }));
  }
  const handle = modal({
    title: t('pj.derivedTitle', { name: fmtFile(img.name, 26) }),
    wide: true,
    body: el('div', {}, el('div', { style: { padding: '0 2px 8px' } }, counter), listBox),
    onClose: () => { loadProject(p.id).then(paint).catch(() => { /* 角标数字没刷新不算事 */ }); },
    actions: [{ label: t('common.close'), kind: 'ghost' }],
  });
  reload();
}

async function delDerived(kid, reload) {
  const ok = await confirm({
    title: t('pj.delDerivedTitle', { name: fmtFile(kid.name, 20) }),
    text: t('pj.delDerivedText'),
    danger: true, okLabel: t('pj.delImg'),
  });
  if (!ok) return;
  try {
    await api.delImage(kid.id);
    toastOk(t('pj.deleted'), kid.name);
    await loadProject(store.peek('project').id);   // 角标上的数字要跟着落
    paint();
    await reload();
  } catch (e) { toastErr(t('ed.delFail'), e.message); }
}

/** 只改库里的显示名：图片、遮罩、成图都挂在数字 id 的目录下，一个文件都不会动 */
async function renameProject() {
  const p = store.peek('project');
  if (!p) return;
  const n = await askName(t('pj.renameAsk'), p.name, {
    maxlength: 60,
    placeholder: t('home.namePh'),
    hint: t('pj.renameHint'),
    okLabel: t('pj.rename'),
  });
  if (!n || n === p.name) return;
  try {
    const r = await api.renameProject(p.id, n);
    /* 页头读 project，首页网格与搜索读 projects[]，两处都要落，否则导航栏留着旧名；
       面包屑是路由进来时由壳画的，改完名要自己补一次，不然顶栏还写着旧名字 */
    store.set({
      project: { ...p, name: r.name },
      projects: store.peek('projects').map(x => (x.id === p.id ? { ...x, name: r.name } : x)),
    }, 'project');
    setCrumb([{ label: t('crumb.home'), href: '/' }, { label: r.name }]);
    paint();
    toastOk(t('pj.renamed'), r.name);
  } catch (e) { toastErr(t('pj.renameFail'), e.message); }
}

async function newCanvasHere() {
  const p = store.peek('project');
  if (!p) return;
  const r = await newCanvas({ projectId: p.id });
  if (!r) return;
  try { await loadProject(p.id); } catch { /* 列表刷新失败不影响这张画布 */ }
  paint();
  go(`/p/${p.id}/c/${r.image_id}`);
}

async function appendFiles(files) {
  const p = store.peek('project'); if (!p) return;
  const busy = toastBusy(t('pj.appendBusy'));
  try {
    const r = await importPhotos(files, {
      projectId: p.id,
      onProgress: (done, total) => busy.update({ msg: t('pj.appendProg', { done, total }) }),
    });
    busy.close();
    if (!r.image_ids.length) { toastErr(t('home.noImages'), t('home.supported')); return; }
    toastOk(t('pj.appended'), r.skipped
      ? t('pj.appendedSkipped', { n: r.image_ids.length, skipped: r.skipped })
      : t('pj.appendedBody', { n: r.image_ids.length }));
    await loadProject(p.id); paint();
  } catch (e) { busy.close(); toastErr(t('pj.appendFail'), e.message); }
}

async function removeImage(img) {
  const ok = await confirm({ title: t('pj.delImgTitle'), text: t('pj.delImgText', { name: img.name }), danger: true, okLabel: t('ph.del') });
  if (!ok) return;
  try {
    await api.delImage(img.id);
    toastOk(t('pj.deleted'), fmtFile(img.name, 20));
    await loadProject(store.peek('project').id); paint();
  } catch (e) { toastErr(t('ed.delFail'), e.message); }
}

async function removeSelected() {
  const ids = store.peek('sel');
  if (!ids.length) return;
  const ok = await confirm({ title: t('pj.delManyTitle', { n: ids.length }), text: t('pj.delManyText'), danger: true, okLabel: t('pj.delAllBtn') });
  if (!ok) return;
  const busy = toastBusy(t('pj.delManyBusy', { n: ids.length }));
  let n = 0;
  for (const id of ids) { try { await api.delImage(id); n++; } catch { /* 单张失败继续删其余 */ } }
  busy.close();
  toastOk(t('pj.delManyDone', { n }));
  clearSel();
  try { await loadProject(store.peek('project').id); } catch (e) { toastErr(t('pj.delListFail'), e.message); return; }
  paint();
}

/* 提交在飞的闸门：按钮 disabled 只挡鼠标，连点与键盘绕得过来，
   而一次批量提交要飞几秒——这期间再点就是第二批任务 */
let submitting = false;

async function submitSelected() {
  if (submitting) return;
  const ids = store.peek('sel');
  if (!ids.length) { toastErr(t('pj.noSel'), t('pj.noSelBody')); return; }
  const settings = store.peek('settings');
  const nomask = ids.filter(id => !store.peek('images').find(i => i.id === id)?.has_mask);
  // 云端 + 反向涂抹时"一笔没涂"是合法状态（= 整幅重绘），整张重绘更是不看遮罩；正向才要先涂出区域
  const full = !!settings?.full;
  const reverse = isCloud() && (full || !!settings?.invert);
  if (!reverse && nomask.length === ids.length) { toastErr(t('pj.selNoMask'), t('pj.selNoMaskBody')); return; }
  submitting = true;
  paint();
  try {
    await saveSettings();
    if (isCloud()) return await submitSelectedCloud(ids, settings, reverse ? 0 : nomask.length);
    const r = await submit(ids, settings);
    if (r.ok) { toastOk(t('pj.submitted', { n: r.ok }), r.skipped.length ? t('pj.submittedSkipped', { n: r.skipped.length }) : ''); paint(); }
    else toastErr(t('pj.noneSubmitted'), r.error || t('pj.noneSubmittedWhy'));
    clearSel();
  } finally {
    submitting = false;
    paint();
  }
}

/**
 * 云端批量：一次建 N 行 queued 交给服务端队列，并发按设置里的 concurrency 排。
 * 页面可以关掉——进度在库里，不在这里。
 */
async function submitSelectedCloud(ids, settings, skippedNoMask) {
  const prompt = cloudPrompt(settings);
  if (!prompt) { toastErr(t('cv.emptyPrompt'), t('pj.cloudPromptBody')); return; }
  let r;
  try {
    r = await api.cloudQueue(ids, {
      prompt, negative: '', steps: 0, cfg: 0, loras: [],
      edge: settings.edge ?? null,
      invert: !!settings.invert,
      // 整张重绘同样按行存：这一批里每张都不读遮罩、回来也不缝合
      full: !!settings.full,
    });
  } catch (e) { toastErr(t('gen.submitFail'), e.message || String(e)); return; }
  const rows = r.results || [];
  if (!rows.length) { toastErr(t('pj.noneSubmitted'), r.skipped?.[0]?.reason || t('pj.noneSubmittedWhy')); return; }
  for (const one of rows) adopt(one.result_id, one.image_id, { onDone: () => paint() });
  const skip = (r.skipped || []).length + skippedNoMask;
  toastOk(t('pj.cloudQueued', { n: rows.length }), skip ? t('pj.cloudQueuedSkip', { n: skip }) : t('pj.cloudQueuedBody'));
  paint();
  clearSel();
}

/* 任务状态变化时局部刷新网格 */
let unsub = null;
export function watchJobs() {
  if (unsub) return;
  /* 每张图每次状态变化都会 setJob：批量 8 张一轮就是十几次整棵网格重绘，合并到一帧 */
  const repaint = raf(paint);
  unsub = store.subscribe((s, key) => {
    if (key !== 'jobs' || !$('#viewProject').classList.contains('is-active')) return;
    repaint();
  });
}
