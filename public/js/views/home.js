// 首页：英雄导入卡 + 三张信息卡 + 六张快捷卡 + 筛选头 + 拼贴封面项目网格
'use strict';
import { el, $, fill } from '../core/dom.js';
import { icon } from '../core/icons.js';
import { api, importPhotos, dropToFiles } from '../core/api.js';
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

/* ==================== 入口 ==================== */
export async function renderHome(filter) {
  if (filter) setHome({ filter });
  const busy = toastBusy('读取项目…');
  try { await loadProjects(); } catch (e) { busy.close(); toastErr('读取项目失败', e.message); return; }
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
      setStat(p.id, { total: d.images.length, masked: d.images.filter(i => i.has_mask).length });
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
    return true;
  });
  const by = {
    updated: (a, b) => String(b.updated_at).localeCompare(String(a.updated_at)),
    name: (a, b) => String(a.name).localeCompare(String(b.name), 'zh'),
    count: (a, b) => (b.cnt || 0) - (a.cnt || 0),
  }[sort];
  return [...list].sort(by);
};

/** 最近一个「有未涂图」的项目，快捷卡用 */
const firstWith = pred => store.peek('projects').map(p => detail.get(p.id)).find(imgs => imgs && imgs.length && pred(imgs));
const latestImages = () => detail.get(store.peek('projects')[0]?.id);

/* ==================== 渲染 ==================== */
function paint() {
  const host = $('#viewHome');
  fill(host,
    el('div.wrap', {},
      el('div.page-hd', {}, el('h1', { text: '主页' }),
        el('span.sub', { text: '本地 ComfyUI 局部重绘 · 批量修图' })),
      heroRow(),
      el('div.sec-hd', {}, el('h2', { text: '快捷功能' })),
      quickRow(),
      el('div.sec-hd', {}, el('h2', { text: '项目' })),
      listHd(),
      grid(),
    ));
}

/* ---------- 四卡行 ---------- */
function heroRow() {
  const cfg = store.peek('cfg') || {};
  return el('div.hero-row', {},
    heroCard(),
    tile('sliders', '工作流参数', '读自本机 Qwen 高分局部编辑工作流',
      () => workflowModal(cfg),
      el('div.tile-card__kv', {}, el('span', {}, '步数 ', el('b', { text: String(cfg.steps ?? '—') })),
        el('span', {}, 'CFG ', el('b', { text: String(cfg.cfg ?? '—') })),
        el('span', {}, 'LoRA ', el('b', { text: String((cfg.loras || []).length) })))),
    tile('book', '修图流程', '导入 → 涂抹 → 提交 → 对比', () => flowModal()),
    tile('keyboard', '快捷键', '画笔 B · 橡皮 E · 滚轮缩放 · 0 适应', () => shortcutsModal()),
  );
}

function heroCard() {
  const name = el('input.input', { type: 'text', placeholder: '项目名，如 0925 漫展', maxlength: '60' });
  const picker = el('input', { type: 'file', accept: 'image/*', multiple: true, hidden: true,
    onchange: e => importFiles(e.target.files, name.value) });

  const card = el('div.hero-card', {
    tabindex: '0', role: 'button', 'aria-label': '照片导入',
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
    el('div.hero-card__t', { text: '照片导入' }),
    el('div.hero-card__d', { text: '拖动图片到此处，或点击选择（可多选）' }),
    el('div.hero-card__form', {}, name,
      el('button.btn.btn--primary.btn--sm', { type: 'button', html: icon('upload', { cls: 'icon icon--sm' }) + '<span>选择</span>', onclick: () => picker.click() })),
    el('div.hero-card__alts', {},
      el('button.btn.btn--ghost.btn--sm', {
        type: 'button', 'data-tip': '一张照片也没有时先把项目建出来，进去后再导入',
        html: icon('folder', { cls: 'icon icon--sm' }) + '<span>建空项目</span>', onclick: () => createEmpty(name.value) }),
      el('button.btn.btn--ghost.btn--sm', {
        type: 'button', 'data-tip': '空白画面上画几笔，交给云端按提示词生成',
        html: icon('canvas', { cls: 'icon icon--sm' }) + '<span>新建画布</span>', onclick: () => createCanvas() }),
    ),
    picker,
  );
  return card;
}

function tile(ico, t, d, onclick, extra) {
  return el('button.tile-card', { type: 'button', onclick },
    el('span.tile-card__ico', { html: icon(ico, { cls: 'icon icon--lg' }) }),
    el('div.tile-card__t', { text: t }),
    el('div.tile-card__d', { text: d }),
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
    quick('right', '继续修图', latest ? latest.name : '暂无项目', cover(latest),
      () => latest && go(`/p/${latest.id}`), !latest),
    quick('brush', '待涂遮罩', unmasked ? `${unmasked.p.name} · 还有未涂的` : '全部已涂', cover(unmasked?.p),
      () => { if (!unmasked) return; store.set({ intent: 'pickUnmasked' }); go(`/p/${unmasked.p.id}`); }, !unmasked),
    quick('play', '已涂待提交', withMask ? `${withMask.p.name} · 批量跑` : '还没有涂好的', cover(withMask?.p),
      () => { if (!withMask) return; store.set({ intent: 'selectMasked' }); go(`/p/${withMask.p.id}`); }, !withMask),
    quick('compare', '结果对比', doneJob ? '回到最近出图的那张' : '本次还没有出图', null,
      () => { if (!doneJob) return; const img = findImage(+doneJob[0]); if (img) go(`/p/${img.project_id}/e/${img.id}`); }, !doneJob),
    quick('cpu', '工作流参数', '查看默认步数 / CFG / LoRA', null, () => workflowModal(store.peek('cfg') || {})),
    quick('sparkles', '参数预设', '管理全局预设模板', null, () => presetManager()),
    quick('keyboard', '快捷键', '画布与工具的全部按键', null, () => shortcutsModal()),
  );
}

/* detail 的键就是项目 id：后端 images 里没有 project_id，从这里补 */
function findImage(imgId) {
  for (const [pid, imgs] of detail) { const hit = imgs.find(i => i.id === imgId); if (hit) return { ...hit, project_id: pid }; }
  return null;
}

function quick(ico, t, d, thumb, onclick, off) {
  return el('button.quick-card', { type: 'button', class: `quick-card${off ? ' is-off' : ''}`, disabled: !!off, onclick },
    el('span.quick-card__txt', {},
      el('span.quick-card__ico', { html: icon(ico, { cls: 'icon' }) }),
      el('span.quick-card__t', { text: t })),
    el('span.quick-card__thumb', { class: `quick-card__thumb${thumb ? '' : ' quick-card__thumb--art'}` },
      thumb ? el('img', { src: thumb, alt: '', loading: 'lazy' })
            : el('span.ic', { html: icon(ico, { cls: 'icon icon--xl' }) })),
    el('span.sr-only', { text: d }),
  );
}

/* ---------- 筛选头 ---------- */
function listHd() {
  const { filter } = store.peek('home');
  const all = store.peek('projects').length;
  const shown = visible().length;
  const tabs = [['all', '全部项目'], ['nomask', '待涂遮罩'], ['masked', '已涂待提交']];
  return el('div.list-hd', {},
    ...tabs.map(([k, label]) => el('button.tab', {
      type: 'button', class: `tab${filter === k ? ' is-on' : ''}`, text: label,
      onclick: () => { setHome({ filter: k }); paint(); },
    })),
    el('div.list-hd__tools', {},
      el('span.chip', {}, '共 ', el('b', { text: String(shown) }), ` / ${all}`),
      el('button.chip', { type: 'button', html: icon('sort', { cls: 'icon icon--sm' }) + `<span>${{ updated: '上次打开时间', name: '名称', count: '张数' }[sort]}</span>`,
        onclick: e => { sort = { updated: 'name', name: 'count', count: 'updated' }[sort]; paint(); e.currentTarget.blur(); } }),
      el('button.chip', { type: 'button', html: icon('refresh', { cls: 'icon icon--sm' }) + '<span>刷新</span>',
        onclick: async () => { detail.clear(); await renderHome(); toastOk('已刷新'); } }),
    ),
  );
}

/* ---------- 项目网格 ---------- */
function grid() {
  const list = visible();
  if (!list.length) {
    const blank = !store.peek('projects').length;
    return emptyState('photos',
      blank ? '还没有项目' : '没有匹配的项目',
      blank ? '把照片拖到上方导入卡，或先建一个空项目' : '换个筛选条件或清空搜索词试试',
      blank ? el('div.hero-card__form', { style: { width: 'auto', marginTop: '0' } },
        el('button.btn.btn--primary', { type: 'button', html: icon('upload', { cls: 'icon icon--sm' }) + '<span>导入照片</span>', onclick: () => $('.hero-card')?.click() }),
        el('button.btn.btn--ghost', { type: 'button', html: icon('folder', { cls: 'icon icon--sm' }) + '<span>建空项目</span>', onclick: () => createEmpty('') })) : null);
  }
  return el('div.pgrid', {}, list.map((p, i) => pcard(p, i)));
}

function pcard(p, i) {
  const imgs = detail.get(p.id) || [];
  const s = statOf(p.id) || { total: p.cnt || imgs.length, masked: 0 };
  const cov = el('div.pcard__cov', {}, ...collage(p, imgs),
    el('span.pcard__badge', { html: s.masked && s.masked < s.total
      ? `已涂 <i>${s.masked}</i> / ${s.total} 张`
      : `共 <i>${s.total}</i> 张` }),
    el('span.pcard__prog', {}, el('i', { style: { width: `${s.total ? (s.masked / s.total) * 100 : 0}%` } })),
    el('button.pcard__del', {
      type: 'button', 'aria-label': '删除项目', 'data-tip': '删除项目',
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
    el('div.pcard__meta', {}, el('span', { text: `更新于 ${fmtStamp(p.updated_at)}` })),
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
  toastOk('画布建好了', '画几笔，写下要生成什么，点生成');
  go(`/p/${r.project_id}/c/${r.image_id}`);
}

/** 空项目：一张照片都没有也能先把坑占下，进去后再导入 */
async function createEmpty(name) {
  try {
    const r = await api.createProject((name || '').trim() || defaultName(), []);
    detail.set(r.id, []);
    setStat(r.id, { total: 0, masked: 0 });
    toastOk('空项目建好了', `${r.name} · 点「导入更多」加照片`);
    go(`/p/${r.id}`);
  } catch (e) { toastErr('建不起来', e.message); }
}

async function importFiles(files, name) {
  const busy = toastBusy('准备导入…');
  try {
    const r = await importPhotos(files, {
      name: (name || '').trim() || defaultName(),
      onProgress: (done, total) => busy.update({ msg: `导入 ${done}/${total} 张…` }),
    });
    busy.close();
    if (!r.image_ids.length) { toastErr('没有可用的图片', '支持 jpg / png / webp'); return; }
    toastOk('导入完成', `${r.image_ids.length} 张已入库` + (r.skipped ? ` · 跳过 ${r.skipped} 个非图片` : ''));
    detail.delete(r.id);
    go(`/p/${r.id}`);
  } catch (e) { busy.close(); toastErr('导入失败', e.message); }
}

const defaultName = () => {
  const d = new Date();
  return `${String(d.getMonth() + 1).padStart(2, '0')}${String(d.getDate()).padStart(2, '0')} 项目`;
};

async function removeProject(p) {
  const ok = await confirm({
    title: `删除「${p.name}」`,
    html: el('div', {},
      el('p', { text: `将连同 ${p.cnt || 0} 张原图、遮罩和全部生成结果一起删除。` }),
      el('p', { style: { color: 'var(--danger)', fontSize: '12px', marginTop: '8px' }, text: '磁盘上的文件也会清掉，不可恢复。' })),
    danger: true, okLabel: '删除项目',
  });
  if (!ok) return;
  try {
    await api.deleteProject(p.id);
    detail.delete(p.id);
    toastOk('项目已删除', p.name);
    await renderHome();
  } catch (e) { toastErr('删除失败', e.message); }
}

export { helpModal };
