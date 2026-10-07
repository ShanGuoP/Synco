// 项目页（图库）：批量操作条 + 筛选头 + 图片网格
// 卡片状态来自 state.jobs，生成完成后原地重绘而不是整页刷新
'use strict';
import { el, $, $$, fill, raf } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api, importPhotos } from '../core/api.js';
import { store, loadProject, saveSettings, selectWhere, clearSel, invertSel, toggleSel, isSel, stateOf, isCloud, cloudPrompt } from '../state.js';
import { go, back } from '../core/router.js';
import { setCrumb } from '../shell.js';
import { toastOk, toastErr, toastBusy } from '../ui/toast.js';
import { confirm, askName, modal } from '../ui/modal.js';
import { newCanvas } from './canvas/index.js';
import { emptyState } from '../ui/empty.js';
import { submit, adopt } from '../gen.js';
import { fmtStamp, fmtFile, fmtDims } from '../core/format.js';

let filter = 'all';
let first = true;

/* ==================== 入口 ==================== */
export async function renderProject(id) {
  const busy = toastBusy('打开项目…');
  try { await loadProject(id); } catch (e) { busy.close(); toastErr('打开失败', e.message); go('/'); return; }
  busy.close();
  applyIntent();
  first = true;
  paint();
}

/** 首页快捷卡带过来的意图 */
function applyIntent() {
  const intent = store.peek('intent');
  if (!intent) return;
  store.set({ intent: null });
  if (intent === 'selectMasked') { selectWhere(i => i.has_mask); toastOk(`已选中 ${store.peek('sel').length} 张`, '确认参数后点「提交已选」'); }
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
      el('div.sec-hd', {}, el('h2', { text: '批量操作' })),
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
    el('button.btn.btn--ghost.btn--icon', { type: 'button', 'aria-label': '返回主页', 'data-tip': '返回主页', html: icon('left'), onclick: () => go('/') }),
    el('div', {},
      el('button.pj-hd__t', {
        type: 'button', 'data-tip': '改名', 'aria-label': `项目名 ${p.name}，点按改名`,
        onclick: () => renameProject(),
      }, el('span.pj-hd__n', { text: p.name }), el('span.ic', { html: icon('edit', { cls: 'icon icon--sm' }) })),
      el('div.pj-hd__meta', {},
        el('span', { text: `${images.length} 张` }), el('span.muted', { text: '·' }),
        el('span', { text: `已涂 ${masked}` }), el('span.muted', { text: '·' }),
        el('span', { text: `更新于 ${fmtStamp(p.updated_at)}` }))),
    el('div.pj-hd__tools', {},
      importBtn(),
      el('button.btn.btn--ghost', { type: 'button', html: icon('canvas', { cls: 'icon icon--sm' }) + '<span>新建画布</span>', onclick: newCanvasHere }),
      el('button.btn.btn--ghost', { type: 'button', html: icon('check', { cls: 'icon icon--sm' }) + '<span>全选已涂</span>', onclick: () => { selectWhere(i => i.has_mask); paint(); } }),
      el('button.btn.btn--primary', { type: 'button', id: 'pjSubmit', html: icon('play', { cls: 'icon icon--sm' }) + `<span>提交已选</span><b id="pjSelN">${store.peek('sel').length}</b>`, onclick: submitSelected }),
    ));
}

function importBtn() {
  const picker = el('input', { type: 'file', id: 'pjPicker', accept: 'image/*', multiple: true, hidden: true,
    onchange: e => appendFiles(e.target.files) });
  return el('label.btn.btn--ghost', { 'data-tip': '追加照片到本项目' },
    el('span.ic', { html: icon('upload', { cls: 'icon icon--sm' }) }), el('span', { text: '导入更多' }), picker);
}

function bulkRow(images) {
  const sel = store.peek('sel');
  const b = (ico, label, n, onclick, dis, cls = '') => el('button.bulk', {
    type: 'button', class: `bulk ${cls}`, disabled: !!dis, onclick,
  }, el('span.ic', { html: icon(ico, { cls: 'icon icon--sm' }) }), el('span', { text: label }),
     n != null ? el('b', { text: String(n) }) : null);

  return el('div.bulk-row', {},
    b('check', '全选已涂', images.filter(i => isPhoto(i) && i.has_mask).length, () => { selectWhere(i => isPhoto(i) && i.has_mask); paint(); }),
    b('brush', '全选未涂', images.filter(i => isPhoto(i) && !i.has_mask).length, () => { selectWhere(i => isPhoto(i) && !i.has_mask); paint(); }),
    b('images', '反选', sel.length, () => { invertSel(); paint(); }),
    b('close', '清除选择', null, () => { clearSel(); paint(); }, !sel.length),
    b('trash', '删除已选', null, () => removeSelected(), !sel.length),
    b('play', '提交已选', sel.length, submitSelected, !sel.length, 'bulk--go'),
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
  const tabs = [['all', '全部'], ['nomask', '未涂遮罩'], ['masked', '已涂遮罩'], ['done', '已出图']];
  return el('div.list-hd', {},
    ...tabs.map(([k, label]) => el('button.tab', {
      type: 'button', class: `tab${filter === k ? ' is-on' : ''}`,
      html: `${label} <i style="font-style:normal;color:var(--spot-txt);font-family:var(--f-mono);font-size:11px">${n[k]}</i>`,
      onclick: () => { filter = k; paint(); },
    })),
    el('div.list-hd__tools', {},
      el('span.chip', {}, '已选 ', el('b', { text: String(store.peek('sel').length) })),
      el('button.chip', { type: 'button', html: icon('refresh', { cls: 'icon icon--sm' }) + '<span>刷新</span>',
        onclick: async () => {
          /* 项目在别的窗口被删掉时，这里不兜住就停在半重绘的网格上，只剩一句通用"请求失败" */
          try { await loadProject(store.peek('project').id); } catch (e) { toastErr('刷新失败', e.message); return; }
          paint(); toastOk('已刷新');
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
      images.length ? '这个筛选下没有图' : '项目还是空的',
      images.length ? '换个标签看看，或点右上角「导入更多」' : '点右上角「导入更多」把照片加进来',
      images.length
        ? el('button.btn.btn--ghost', { type: 'button', text: '看全部', onclick: () => { filter = 'all'; paint(); } })
        : el('button.btn.btn--primary', { type: 'button', html: icon('upload', { cls: 'icon icon--sm' }) + '<span>导入照片</span>', onclick: () => $('#pjPicker')?.click() }));
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
      img.orig_dead ? el('span.icard__gone', { text: '原图文件已丢失' })
                    : sketch ? el('img.icard__sketch', { src: img.orig_url, alt: img.name, loading: 'lazy', decoding: 'async' })
                    : el('img', { src: img.thumb_url || img.orig_url, alt: img.name, loading: 'lazy', decoding: 'async' }),
      // 蒙版是原地覆写的文件，服务端对它发 ETag + no-cache，判新交给浏览器条件请求，不再自己拼 ?t=
      img.mask_url ? el('img.icard__mask', { src: img.mask_url, alt: '', 'aria-hidden': 'true' }) : null,
      st === 'run' ? el('span.icard__run', {}, el('i')) : null,
      el('button.icard__cb', {
        type: 'button', 'aria-label': '选择', 'data-tip': '加入提交队列',
        html: icon('check', { cls: 'icon icon--sm' }),
        onclick: e => { e.stopPropagation(); toggleSel(img.id); syncSel(); },
      }),
      // 派生查看：常驻角标，不放进 .icard__acts（那块在窄屏整块 display:none，手机上就点不到了）
      // 两个数分别是「出过几张成图」与「另存出去几张子图」——它们是两件事，别混成一个
      img.result_done > 0 || img.derived_count > 0 ? el('button.icard__derived', {
        type: 'button',
        'aria-label': `成图 ${img.result_done || 0} 张、派生 ${img.derived_count || 0} 张`,
        'data-tip': `成图 ${img.result_done || 0} · 派生的图 ${img.derived_count || 0}`,
        html: `${icon('layers', { cls: 'icon icon--sm' })}<b>${img.result_done || 0}</b>`
          + (img.derived_count > 0 ? `${icon('copy', { cls: 'icon icon--sm' })}<b>${img.derived_count}</b>` : ''),
        onclick: e => { e.stopPropagation(); showDerived(img); },
      }) : null,
      el('div.icard__acts', {},
        el('button.btn.btn--primary.btn--sm', { type: 'button', html: icon(sketch ? 'canvas' : 'brush', { cls: 'icon icon--sm' }) + `<span>${sketch ? '打开画布' : img.has_mask ? '继续涂' : '涂遮罩'}</span>`, onclick: e => { e.stopPropagation(); openCard(img); } }),
        el('button.btn.btn--danger.btn--icon.btn--sm', { type: 'button', 'aria-label': '删除图片', html: icon('trash', { cls: 'icon icon--sm' }), onclick: e => { e.stopPropagation(); removeImage(img); } }),
      ),
      el('span.icard__badge', {
        class: `icard__badge st-${st}`,
        html: st === 'done' ? '<i>已出图</i>' : st === 'err' ? '失败' : st === 'run' ? '生成中' : sketch ? '画布' : fmtDims(img.w, img.h),
      }),
    ),
    el('div.icard__ft', {},
      el('span.nm', { title: img.name, text: fmtFile(img.name, 22) }),
      el('span', { text: sketch ? `${img.w}×${img.h}` : img.has_mask ? '已涂' : '未涂' }),
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
  const goBtn = $('#pjSubmit'); if (goBtn) goBtn.disabled = !n;
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
  const counter = el('span.muted', { text: '读取中…' });
  const listBox = el('div.pre-list', {});
  async function reload() {
    let d;
    try {
      d = await api.imageDerived(img.id);
    } catch (e) {
      counter.textContent = '读不到派生的图';
      fill(listBox, el('p.muted', { style: { padding: '12px 6px' }, text: String(e.message || e) }));
      return;
    }
    const rows = d.images || [];
    counter.textContent = rows.length
      ? `${rows.length} 张派生的图 · 这张图自己有 ${img.result_done || 0} 张成图（在编辑器右侧的历史里看）`
      : '还没有从这张图另存出新图';
    fill(listBox, rows.length ? rows.map(kid => el('div.pre-it', {
      role: 'button', tabindex: '0',
      onclick: () => { handle?.close('open'); openCard(kid); },
      onkeydown: e => { if (e.key === 'Enter') { handle?.close('open'); openCard(kid); } },
    },
      kid.orig_dead ? el('span.pre-it__ph.pre-it__ph--gone')
                    : el('img.pre-it__th', { src: kid.thumb_url || kid.orig_url, alt: '', loading: 'lazy' }),
      el('span.pre-it__tx', {},
        el('b', { text: kid.name }),
        el('span.pre-it__meta', { text: `${fmtDims(kid.w, kid.h)} · 来自 #${kid.derived_result || '?'}`, title: kid.name })),
      el('span.pre-it__acts', {},
        el('span.pre-it__btn', { text: '打开' }),
        el('button.pre-it__btn', {
          type: 'button', title: '删除这张派生图（父图与它的成图都不动）',
          html: icon('trash', { cls: 'icon icon--sm' }),
          onclick: e => { e.stopPropagation(); delDerived(kid, reload); },
        })),
    )) : el('p.muted', { style: { padding: '14px 6px', lineHeight: '1.7' },
      text: '在修图界面点开某张成图，选「另存为新图」就会得到一张派生图——它是独立的一张图，可以再涂遮罩、再生成，与父图互不影响。' }));
  }
  const handle = modal({
    title: `${fmtFile(img.name, 26)} 派生的图`,
    wide: true,
    body: el('div', {}, el('div', { style: { padding: '0 2px 8px' } }, counter), listBox),
    onClose: () => { loadProject(p.id).then(paint).catch(() => { /* 角标数字没刷新不算事 */ }); },
    actions: [{ label: '关闭', kind: 'ghost' }],
  });
  reload();
}

async function delDerived(kid, reload) {
  const ok = await confirm({
    title: `删除「${fmtFile(kid.name, 20)}」`,
    text: '只删这张派生出来的图（它的原图、遮罩与成图）。父图一动不动。',
    danger: true, okLabel: '删除图片',
  });
  if (!ok) return;
  try {
    await api.delImage(kid.id);
    toastOk('已删除', kid.name);
    await loadProject(store.peek('project').id);   // 角标上的数字要跟着落
    paint();
    await reload();
  } catch (e) { toastErr('删除失败', e.message); }
}

/** 只改库里的显示名：图片、遮罩、成图都挂在数字 id 的目录下，一个文件都不会动 */
async function renameProject() {
  const p = store.peek('project');
  if (!p) return;
  const n = await askName('重命名项目', p.name, {
    maxlength: 60,
    placeholder: '如 0925 漫展',
    hint: '只改显示名字。原图、遮罩和成图都按项目号存在资料目录里，改名不会动任何一个文件。',
    okLabel: '改名',
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
    setCrumb([{ label: '主页', href: '/' }, { label: r.name }]);
    paint();
    toastOk('已改名', r.name);
  } catch (e) { toastErr('改名失败', e.message); }
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
  const busy = toastBusy('准备追加…');
  try {
    const r = await importPhotos(files, {
      projectId: p.id,
      onProgress: (done, total) => busy.update({ msg: `追加 ${done}/${total} 张…` }),
    });
    busy.close();
    if (!r.image_ids.length) { toastErr('没有可用的图片', '支持 jpg / png / webp'); return; }
    toastOk('已追加', `${r.image_ids.length} 张进入项目` + (r.skipped ? ` · 跳过 ${r.skipped} 个非图片` : ''));
    await loadProject(p.id); paint();
  } catch (e) { busy.close(); toastErr('追加失败', e.message); }
}

async function removeImage(img) {
  const ok = await confirm({ title: '删除这张图片', text: `「${img.name}」的原图、遮罩和生成结果都会删除。`, danger: true, okLabel: '删除' });
  if (!ok) return;
  try {
    await api.delImage(img.id);
    toastOk('已删除', fmtFile(img.name, 20));
    await loadProject(store.peek('project').id); paint();
  } catch (e) { toastErr('删除失败', e.message); }
}

async function removeSelected() {
  const ids = store.peek('sel');
  if (!ids.length) return;
  const ok = await confirm({ title: `删除 ${ids.length} 张图片`, text: '原图、遮罩和结果一并删除，不可恢复。', danger: true, okLabel: '全部删除' });
  if (!ok) return;
  const busy = toastBusy(`删除 ${ids.length} 张…`);
  let n = 0;
  for (const id of ids) { try { await api.delImage(id); n++; } catch { /* 单张失败继续删其余 */ } }
  busy.close();
  toastOk(`已删除 ${n} 张`);
  clearSel();
  try { await loadProject(store.peek('project').id); } catch (e) { toastErr('删完了，但列表没刷新', e.message); return; }
  paint();
}

async function submitSelected() {
  const ids = store.peek('sel');
  if (!ids.length) { toastErr('没有可提交的图', '先勾选，或点「全选已涂」'); return; }
  const settings = store.peek('settings');
  const nomask = ids.filter(id => !store.peek('images').find(i => i.id === id)?.has_mask);
  // 云端 + 反向涂抹时"一笔没涂"是合法状态（= 整幅重绘），整张重绘更是不看遮罩；正向才要先涂出区域
  const full = !!settings?.full;
  const reverse = isCloud() && (full || !!settings?.invert);
  if (!reverse && nomask.length === ids.length) { toastErr('选中的都还没涂遮罩', '打开图片涂出要修的区域'); return; }
  await saveSettings();
  if (isCloud()) return submitSelectedCloud(ids, settings, reverse ? 0 : nomask.length);
  const r = await submit(ids, settings);
  if (r.ok) { toastOk(`已提交 ${r.ok} 张`, r.skipped.length ? `${r.skipped.length} 张没提交，原因见卡片` : ''); paint(); }
  else toastErr('没有提交成功', r.error || '选中的图片都还没有遮罩');
  clearSel();
}

/**
 * 云端批量：一次建 N 行 queued 交给服务端队列，并发按设置里的 concurrency 排。
 * 页面可以关掉——进度在库里，不在这里。
 */
async function submitSelectedCloud(ids, settings, skippedNoMask) {
  const prompt = cloudPrompt(settings);
  if (!prompt) { toastErr('提示词是空的', '云端只会照原图描一遍，先写要改成什么样'); return; }
  let r;
  try {
    r = await api.cloudQueue(ids, {
      prompt, negative: '', steps: 0, cfg: 0, loras: [],
      edge: settings.edge ?? null,
      invert: !!settings.invert,
      // 整张重绘同样按行存：这一批里每张都不读遮罩、回来也不缝合
      full: !!settings.full,
    });
  } catch (e) { toastErr('提交失败', e.message || String(e)); return; }
  const rows = r.results || [];
  if (!rows.length) { toastErr('没有提交成功', r.skipped?.[0]?.reason || '选中的图片都还没有遮罩'); return; }
  for (const one of rows) adopt(one.result_id, one.image_id, { onDone: () => paint() });
  const skip = (r.skipped || []).length + skippedNoMask;
  toastOk(`已排进云端队列 ${rows.length} 张`, skip ? `${skip} 张没进队列，原因见卡片` : '可以关页面，服务端会继续跑');
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
