// 项目页（图库）：批量操作条 + 筛选头 + 图片网格
// 卡片状态来自 state.jobs，生成完成后原地重绘而不是整页刷新
'use strict';
import { el, $, $$, fill, raf } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api, importPhotos } from '../core/api.js';
import { store, loadProject, saveSettings, selectWhere, clearSel, invertSel, toggleSel, isSel, stateOf, isCloud, cloudPrompt } from '../state.js';
import { go, back } from '../core/router.js';
import { toastOk, toastErr, toastBusy } from '../ui/toast.js';
import { confirm } from '../ui/modal.js';
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

function header(p, images) {
  const masked = images.filter(i => i.has_mask).length;
  return el('div.pj-hd', {},
    el('button.btn.btn--ghost.btn--icon', { type: 'button', 'aria-label': '返回主页', 'data-tip': '返回主页', html: icon('left'), onclick: () => go('/') }),
    el('div', {},
      el('div.pj-hd__t', { text: p.name }),
      el('div.pj-hd__meta', {},
        el('span', { text: `${images.length} 张` }), el('span.muted', { text: '·' }),
        el('span', { text: `已涂 ${masked}` }), el('span.muted', { text: '·' }),
        el('span', { text: `更新于 ${fmtStamp(p.updated_at)}` }))),
    el('div.pj-hd__tools', {},
      importBtn(),
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
    b('check', '全选已涂', images.filter(i => i.has_mask).length, () => { selectWhere(i => i.has_mask); paint(); }),
    b('brush', '全选未涂', images.filter(i => !i.has_mask).length, () => { selectWhere(i => !i.has_mask); paint(); }),
    b('images', '反选', sel.length, () => { invertSel(); paint(); }),
    b('close', '清除选择', null, () => { clearSel(); paint(); }, !sel.length),
    b('trash', '删除已选', null, () => removeSelected(), !sel.length),
    b('play', '提交已选', sel.length, submitSelected, !sel.length, 'bulk--go'),
  );
}

function listHd(images) {
  const n = {
    all: images.length,
    nomask: images.filter(i => !i.has_mask).length,
    masked: images.filter(i => !!i.has_mask).length,
    done: images.filter(i => stateOf(i) === 'done').length,
  };
  /* 列表接口不返回结果状态，done 只能统计本次会话跑完的 */
  const tabs = [['all', '全部'], ['nomask', '未涂遮罩'], ['masked', '已涂遮罩'], ['done', '本次出图']];
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
  nomask: !i.has_mask,
  masked: !!i.has_mask,
  done: stateOf(i) === 'done',
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
  const job = store.peek('jobs')[img.id];
  const card = el('div.icard', {
    class: `icard${isSel(img.id) ? ' is-sel' : ''}`,
    style: { '--i': String(Math.min(i, 24)) },
    tabindex: '0', role: 'button', dataset: { id: String(img.id) },
    onclick: e => { if (e.target.closest('.icard__cb, .icard__acts')) return; openImg(img.id); },
    onkeydown: e => { if (e.key === 'Enter') openImg(img.id); },
  },
    el('div.icard__cov', {},
      img.orig_dead ? el('span.icard__gone', { text: '原图文件已丢失' })
                    : el('img', { src: img.thumb_url || img.orig_url, alt: img.name, loading: 'lazy', decoding: 'async' }),
      // 蒙版是原地覆写的文件，服务端对它发 ETag + no-cache，判新交给浏览器条件请求，不再自己拼 ?t=
      img.mask_url ? el('img.icard__mask', { src: img.mask_url, alt: '', 'aria-hidden': 'true' }) : null,
      st === 'run' ? el('span.icard__run', {}, el('i')) : null,
      el('button.icard__cb', {
        type: 'button', 'aria-label': '选择', 'data-tip': '加入提交队列',
        html: icon('check', { cls: 'icon icon--sm' }),
        onclick: e => { e.stopPropagation(); toggleSel(img.id); syncSel(); },
      }),
      el('div.icard__acts', {},
        el('button.btn.btn--primary.btn--sm', { type: 'button', html: icon('brush', { cls: 'icon icon--sm' }) + `<span>${img.has_mask ? '继续涂' : '涂遮罩'}</span>`, onclick: e => { e.stopPropagation(); openImg(img.id); } }),
        el('button.btn.btn--danger.btn--icon.btn--sm', { type: 'button', 'aria-label': '删除图片', html: icon('trash', { cls: 'icon icon--sm' }), onclick: e => { e.stopPropagation(); removeImage(img); } }),
      ),
      el('span.icard__badge', { class: `icard__badge st-${st}`, html: st === 'done' ? '<i>已出图</i>' : st === 'err' ? '失败' : st === 'run' ? '生成中' : fmtDims(img.w, img.h) }),
    ),
    el('div.icard__ft', {},
      el('span.nm', { title: img.name, text: fmtFile(img.name, 22) }),
      el('span', { text: img.has_mask ? '已涂' : '未涂' }),
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
const openImg = id => { const p = store.peek('project'); if (p) go(`/p/${p.id}/e/${id}`); };

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
  if (nomask.length === ids.length) { toastErr('选中的都还没涂遮罩', '打开图片涂出要修的区域'); return; }
  await saveSettings();
  if (isCloud()) return submitSelectedCloud(ids, settings, nomask.length);
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
    r = await api.cloudQueue(ids, { prompt, negative: '', steps: 0, cfg: 0, loras: [] });
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
