import { buildEditorShell } from './workspace/shell.js';
import { createResultActions, resultTag } from './workspace/results.js';
// 编辑器装配：五区版式 + 工具/视口/绘制/参数/历史/胶片条的接线
'use strict';
import { $, $$, fill } from '../../core/dom.js';
import { icon } from '../../core/icons.js';
import { api } from '../../core/api.js';
import { go } from '../../core/router.js';
import { createViewSession } from '../../core/viewSession.js';
import { store, loadProject, saveSettings, stateOf, touchImage, diffSettings, settingsFromPreset, setJob, isCloud, effMode, patchSettings, cloudPrompt } from '../../state.js';
import { submit, adopt, drop, claimSubmission } from '../../gen.js';
import { toastOk, toastErr, toastBusy } from '../../ui/toast.js';
import { shortcutsModal } from '../../ui/dialogs.js';
import { confirm } from '../../ui/modal.js';
import { fmtFile, fmtDims } from '../../core/format.js';
import { beforeSwitch, dx, t } from '../../core/i18n.js';
import { hold } from '../../core/guard.js';
import { appearance } from '../../core/theme.js';

let ctx = null;   // 当前编辑器上下文（只建一次，切图复用）
const session = createViewSession();
let rerunFrom = null;       // 下一次提交是「哪条结果的重跑」
let lastSubmitted = null;   // 上次提交的参数快照，用来判断面板里是否有未提交改动

/**
 * 涂抹层的长边档位，与底图脱钩。
 * 底图在 321–3072 这一档只有 320 的缩略图（proxy 只在原图超出档位时才切），
 * 跟着底图走就等于把笔迹画在 213×320 的格子上再让服务端放大——边界成块，外扩与羽化全算在粗格子上。
 * 上限就是 proxy 那一档：撤销栈存 8 张同尺寸快照，24MP 满尺寸要 768MB。
 */
const PAINT_EDGE = 3072;

export const isEditorOpen = () => !!ctx && !$('#editor').hidden;

/* ==================== 骨架（只建一次） ==================== */
function buildShell() {
  return buildEditorShell({
    context: () => ctx,
    syncZoomPills, restoreFromResult, forkResult, onZoom, setTool, onEdit,
    exportCurrent, toggleDrawer, openBestCompare, goBack, toggleCol,
    showCompare, delResult, doSubmit, stopCurrent, applyMode, applyScope,
    applyPreset, showImage, syncFilm, onMaskSaved, syncInvertHint, forkAdjusted, paintHud,
  });
}

const narrow = () => window.matchMedia('(max-width: 1100px)').matches;

function toggleCol(cls, key) {
  /* 窄屏没有三栏可收，图标轨改当抽屉/浮层开关 */
  if (narrow() && appearance() !== 'glass') {
    if (key === 'prop') toggleDrawer();
    else if (key === 'pre') toastErr(t('ed.narrowHist'), t('ed.narrowHistBody'));
    return;
  }
  const off = $('#editor').classList.toggle(cls);
  if (!off && appearance() === 'glass' && window.matchMedia('(max-width: 960px)').matches) {
    const other = key === 'pre' ? 'prop' : 'pre';
    $('#editor').classList.add(other === 'pre' ? 'no-pre' : 'no-prop');
    const otherButton = $(`.ed-rail [data-key="${other}"]`);
    if (otherButton) { otherButton.classList.remove('is-on'); otherButton.setAttribute('aria-expanded', 'false'); }
  }
  const b = $(`.ed-rail [data-key="${key}"]`);
  if (b) { b.classList.toggle('is-on', !off); b.setAttribute('aria-expanded', String(!off)); }
}

/** 窄屏：属性面板走底部抽屉 */
function toggleDrawer() {
  if (appearance() === 'glass') { toggleCol('no-prop', 'prop'); return; }
  const ed = $('#editor');
  const on = ed.classList.toggle('is-drawer');
  ed.classList.remove('no-prop');
  if (on) ed.querySelector('.prop-body')?.scrollTo({ top: 0 });
}

/* ==================== 打开 / 关闭 ==================== */
export async function openEditor(projectId, imgId) {
  const opening = session.begin();
  if (ctx) {
    ctx.showSeq = (ctx.showSeq || 0) + 1;
    try { await Promise.all([ctx.painter.flush(), ctx.adjust.flush()]); }
    catch (e) { if (opening.current()) toastErr(t('ed.loadFail'), e.message); return; }
    if (!opening.current()) return;
  }
  const ed = $('#editor');
  if (!ctx) {
    ctx = buildShell();
    for (const part of [ctx.top, ctx.tools, ctx.stage, ctx.history.node, ctx.params.node, ctx.rail, ctx.film.node]) ed.append(part);
    if (appearance() === 'glass') {
      ed.classList.add('no-pre');
      const historyButton = ctx.rail.querySelector('[data-key="pre"]');
      historyButton.classList.remove('is-on');
      historyButton.setAttribute('aria-expanded', 'false');
    }
    wireKeys(ctx);
  }
  /* 数据到手再掀覆盖层：脏 URL（比如 #/p/undefined/e/3）会在这里抛，此时应留在原页面而不是显示一个空编辑器 */
  const { project, images } = store.get();
  if (!project || project.id !== +projectId || !images.some(i => i.id === +imgId)) {
    try { await loadProject(projectId, opening.current); }
    catch (e) { if (opening.current()) { toastErr(t('ed.projFail'), e.message || String(e)); go('/'); } return; }
  }
  if (!opening.current()) return;
  ed.hidden = false;
  ed.classList.remove('is-drawer');
  $('#shell').style.visibility = 'hidden';

  ctx.projectId = +projectId;
  ctx.params.sync(store.peek('settings'));
  await showImage(+imgId);
}

export async function beforeLeaveEditor() {
  if (!isEditorOpen()) return;
  await Promise.all([ctx.painter.flush(), ctx.adjust.flush()]);
}

export function closeEditor() {
  session.end();
  if (ctx) ctx.showSeq = (ctx.showSeq || 0) + 1;
  ctx?.compare?.hide();
  $('#editor').hidden = true;
  $('#shell').style.visibility = '';
  /* 不等：路由已经要走了。这笔由画笔模块串行排出去，下一次 showImage 的 load() 会 await 到它 */
  Promise.resolve(ctx?.painter.flush()).catch(() => {});
  /* 调整面板同理：最后一次滑杆改动可能还压在 150ms 防抖里，落库要赶在关窗前发出去 */
  ctx?.adjust.exit();
  Promise.resolve(ctx?.adjust.flush()).catch(() => {});
}

/* 关页/刷新/系统休眠前的最后一搏：防抖里的 700ms 和排队中的保存都得此刻冲出去（能不能落地看网络，至少不静默） */
window.addEventListener('pagehide', () => {
  Promise.resolve(ctx?.painter.flush()).catch(() => {});
  Promise.resolve(ctx?.adjust?.flush()).catch(() => {});
});

/* 换语言走的是"存好 → 重载"，pagehide 那条只能尽力（异步保存赶得上卸载就输了），
   所以这里给一次真正 await 的机会：注册制，设置弹窗不认识编辑器内部 */
beforeSwitch(async () => {
  if (!ctx) return;
  await Promise.resolve(ctx.painter.flush());
  await Promise.resolve(ctx.adjust.flush());
});

const goBack = () => { const p = store.peek('project'); go(p ? `/p/${p.id}` : '/'); };

/* ==================== 单图装载 ==================== */
async function showImage(imgId) {
  if (!session.current()) return;
  const row = store.peek('images').find(i => i.id === imgId);
  if (!row) { toastErr(t('ed.notInProject')); return; }
  /* 连点胶片条会并发跑好几次装载：认领 imgId 之后还有三个 await，
     晚到的旧响应必须能认出自己已经过期，否则它会把画布、海报与 painter 的归属写成上一张 */
  const seq = (ctx.showSeq = (ctx.showSeq || 0) + 1);
  const stale = () => seq !== ctx.showSeq || !session.current();
  try { await Promise.all([ctx.painter.flush(), ctx.adjust.flush()]); }
  catch (e) { if (!stale()) toastErr(t('ed.saveFail'), e.message); return; }
  if (stale()) return;
  ctx.imgId = imgId;

  const busy = toastBusy(t('ed.loadingImage'));
  let info;
  try { info = await api.image(imgId); } catch (e) { busy.close(); if (!stale()) toastErr(t('ed.loadFail'), e.message); return; }
  busy.close();
  if (stale()) return;

  ctx.info = info;
  ctx.hudFrame = null;      // 上一张的画幅读数不能跟着换图留下（adjust.onImage 会按这一张重新报）
  ctx.fname.textContent = fmtFile(info.name, 30);
  ctx.fname.title = info.name;
  ctx.fdims.textContent = fmtDims(info.w, info.h);
  ctx.params.setCloud(isCloud());
  paintHud();
  /* 库里有过这行 ≠ 盘上还有这个文件（清过 data/projects、手工删过图都会留下指向空气的记录） */
  ctx.setFlag(info.orig_dead ? t('film.lostFile') : '', info.orig_dead ? 'err' : '');

  // 底图先换掉：切图时旧照片不该还贴在屏幕上，瓦片清单随后异步补
  ctx.tiles.abort();
  if (info.orig_dead) ctx.poster.removeAttribute('src');
  else ctx.poster.src = info.proxy_url || info.thumb_url || info.orig_url;
  ctx.poster.hidden = !!info.orig_dead;
  if (!ctx.poster.hidden) await new Promise(res => {
    const done = () => res();
    ctx.poster.onload = done;
    ctx.poster.onerror = () => { toastErr(t('ed.decodeFail'), info.name); done(); };
    if (ctx.poster.complete && ctx.poster.naturalWidth) done();
  });
  if (stale()) return;

  ctx.viewport.setContentSize(info.w, info.h);
  // 涂抹层按自己的档位走（见 PAINT_EDGE），不再跟着底图：底图可能只是一张 320 的缩略图，
  // 跟着它就把笔迹锁死在粗格子上，边界成块、外扩与羽化全算在粗格子上。上采样到原图尺寸是服务端的事。
  // 原图文件已丢失时底图 naturalWidth 是 0，过去会退化成按库里的报称尺寸开满尺寸缓冲（24MP 的撤销栈≈768MB）
  const s = Math.min(1, PAINT_EDGE / Math.max(1, info.w, info.h));
  const paint = { w: Math.max(1, Math.round(info.w * s)), h: Math.max(1, Math.round(info.h * s)) };
  const had = await ctx.painter.load(info.w, info.h, info.mask_url, imgId, paint, () => !stale());
  if (stale()) return;
  /* 「本地调整」要在这一步把底图换成库里那套参数渲染出来的预览（如果有参数）。
     它排在 fit 之前：裁切/旋转会换画幅尺寸，先定尺寸再适应窗口才不会再歪一次。
     返回 true = 这一张显示的是调整预览，源图瓦片就不该再去摆 */
  const adjustedView = await ctx.adjust.onImage(info, () => !stale());
  if (stale()) return;
  syncInvertHint();
  ctx.viewport.fit();
  showHint(!had && !info.orig_dead);
  setTool('brush');
  // 瓦片是懒切的，首次进去可能要等它几秒；海报先到，画面不会空
  if (!info.orig_dead && info.tiles_url && !adjustedView) {
    api.tiles(imgId).then(m => {
      if (ctx.imgId !== imgId) return;
      ctx.tiles.setMeta(m);
      ctx.viewport.refresh();
    }).catch(() => { /* 没有瓦片就停在海报上，一样能涂能看 */ });
  }

  ctx.history.setResults(info.results, info.last?.status === 'done' ? info.last.id : null);
  ctx.compare.hide();
  /* 从项目页的派生弹窗点进来：把选中的那条摆到对比层上（弹窗只做看与删，回填走这里）。
     必须排在 compare.hide() 之后，否则刚开的对比层会被上面那句关掉 */
  const intent = store.peek('intent');
  if (intent?.focusResult) {
    store.set({ intent: null }, 'intent');
    const hit = info.results.find(x => x.id === intent.focusResult)
      || (await api.result(intent.focusResult).catch(() => null));
    if (stale()) return;
    if (hit?.status === 'done' && !hit.final_dead) {
      ctx.history.setResults(info.results, hit.id);
      showCompare(hit);
    } else {
      toastErr(t('ed.resultBlind'), hit ? (dx(hit.error, hit.error_args) || t('ed.resultBlindWhy')).slice(0, 120) : t('ed.recordGone'));
    }
  }
  ctx.params.stages.reset();
  ctx.params.setBusy(false);
  ctx.params.clearMarks();
  ctx.params.line(had ? t('ed.hasMask') : '');
  rerunFrom = null;
  lastSubmitted = null;

  /* 库里有仍在排或仍在跑的任务（页面刷新或切走过）→ 接回轮询。
     云端行现在也由服务端推进，所以两条链路都要接 */
  const resumable = info.results.filter(r => r.status === 'running' || r.status === 'queued');
  if (resumable.length) {
    ctx.params.setBusy(true);
    for (const key of ['submit', 'queue']) ctx.params.stages.set(key, 'done');
    ctx.params.stages.set('sample', 'run');
    ctx.params.line(t('ed.rejoin'));
    for (const r of resumable) adopt(r.id, imgId, {
      /* 轮询回来时可能已经切图：这一张的阶段条只在这张还是当前图时改 */
      onTick: s => { if (ctx.imgId === imgId && s !== 'running' && s !== 'queued') ctx.params.stages.set('sample', s === 'done' ? 'done' : 'err'); },
      onDone: done => onJobSettled(done),
    });
  }

  syncFilm();
  syncExport();
}

const showHint = on => {
  ctx.hint.hidden = !on;
  if (on) ctx.hint.innerHTML = `${icon('brush', { cls: 'icon icon--sm' })}<span>${t('ed.noMaskHint')}</span>`;
};

/** 左下角那条 HUD：两条出图路的口径不一样，切换之后必须重写，不能停在装载时那一句 */
function paintHud() {
  const info = ctx?.info;
  if (!info) return;
  // 摆的是哪一版就说哪一版的尺寸：带着裁切或旋转时，画幅已与源图不同
  const d = ctx.hudFrame || info;
  const inv = isCloud() && !!store.peek('settings')?.invert;
  ctx.hudSize.textContent = inv
    ? t('ed.dimsA', { wh: `${d.w}×${d.h}` })
    : isCloud()
      ? t('ed.dimsB', { wh: `${d.w}×${d.h}` })
      : t('ed.dimsC', { wh: `${d.w}×${d.h}` });
}

/* ---------------- 反向涂抹（只在这条路上开） ---------------- */
const HINT_W = 512;
let spotCache = '';

/** 画布要的是字面色值，CSS 变量拿不到就直接问 computed style，退档用朱红的默认值 */
function spotColor() {
  if (!spotCache) {
    spotCache = getComputedStyle(document.documentElement).getPropertyValue('--spot').trim() || '#c72c2c';
  }
  return spotCache;
}

function invertOn() {
  const s = store.peek('settings') || {};
  // 整张重绘根本不看遮罩，这时候铺朱红预览是在说一件不会发生的事
  return isCloud() && !!s.invert && !s.full;
}

/** 朱红铺满整幅，再用笔迹按 alpha 挖洞：剩下的红就是"会被重绘的地方" */
function syncInvertHint() {
  if (!ctx?.mask) return;
  const on = invertOn();
  ctx.stage.classList.toggle('is-invert', on);
  ctx.invert.hidden = !on;
  if (!on) return;
  const m = ctx.mask;
  const w = HINT_W;
  const h = Math.max(1, Math.round((HINT_W * m.height) / Math.max(1, m.width)));
  const c = ctx.invert;
  if (c.width !== w || c.height !== h) { c.width = w; c.height = h; }
  const x = c.getContext('2d');
  x.globalCompositeOperation = 'source-over';
  x.clearRect(0, 0, w, h);
  x.fillStyle = spotColor();
  x.fillRect(0, 0, w, h);
  x.globalCompositeOperation = 'destination-out';
  x.drawImage(m, 0, 0, w, h);
  x.globalCompositeOperation = 'source-over';
}

/** 涂要改的 / 涂要保留的 / 整张重绘：换的是这一枪的语义，遮罩文件本身不动 */
function applyScope(mode) {
  if (!ctx?.imgId) return;
  const s = store.peek('settings') || {};
  const next = { invert: mode === 'keep', full: mode === 'full' };
  if (!!s.invert === next.invert && !!s.full === next.full) return;
  patchSettings(next);
  saveSettings().catch(() => { /* 参数写库失败不阻断这次编辑 */ });
  ctx.params.sync(store.peek('settings'));
  ctx.params.setScope();
  paintHud();
  syncInvertHint();
  ctx.params.line(mode === 'full'
    ? t('ed.modeFullNote')
    : mode === 'keep'
      ? t('ed.modeKeepNote')
      : t('ed.modePartNote'));
}

/**
 * 就地切本机 / 云端。模式记在**这个项目**的参数里（不是全局设置），
 * 所以"这个项目的活交给云端、下一个还走本机"是各记各的；全局那一栏是没选过时的默认值。
 */
function applyMode(next) {
  if (!ctx?.imgId) return;
  if (next !== 'cloud' && next !== 'comfyui') return;
  if (next === effMode()) return;
  // 反向涂抹只在这条路上成立；切回本机就把它关掉，别让面板上留着一个不会生效的开关
  patchSettings(next === 'comfyui' ? { mode: next, invert: false } : { mode: next });
  // 写完就落库：切了模式不提交、下次进这个项目还应该是刚才那一个
  saveSettings().catch(() => { /* 参数写库失败不阻断这次编辑 */ });
  ctx.params.setCloud(isCloud());
  paintHud();
  ctx.params.line(next === 'cloud'
    ? t('ed.switchedCloud')
    : t('ed.switchedLocal'));
}

/* ==================== 工具 / 缩放 ==================== */
function setTool(k) {
  /* 裁切/旋转换了画幅，遮罩那一层还是按源图坐标系存的：这时涂上去的笔迹会歪。
     与其静默错位，不如挡住并说清楚出口（先「应用为新图」，新图的源图就是那张裁好的） */
  if ((k === 'brush' || k === 'erase') && ctx.adjust?.geometryActive) {
    toastErr(t('ed.brushLocked'), t('ed.adjustLocked'));
    return;
  }
  if (k === 'brush' || k === 'erase') ctx.adjust?.exit();   // 交回画笔：裁切框与液化盘让位
  ctx.tool = k;
  for (const b of $$('.ed-tools .tool[data-k]')) b.classList.toggle('is-on', b.dataset.k === k);
  ctx.viewport.setMode(k === 'pan' ? 'pan' : 'paint');
  ctx.painter.setTool(k === 'erase' ? 'erase' : 'brush');
  ctx.cursor.style.display = 'none';
}

function onZoom(k) {
  const viewport = ctx.compare.isOn ? ctx.compare : ctx.viewport;
  ({ fit: () => viewport.fit(), one: () => viewport.one2one(),
     zin: () => viewport.zoomBy(1.35), zout: () => viewport.zoomBy(1 / 1.35) })[k]?.();
}

function onEdit(k) {
  if (ctx.compare.isOn) return;
  if (k === 'undo') {
    if (!ctx.painter.canUndo) { toastErr(t('ed.noUndo')); return; }
    ctx.painter.undo().then(ok => { if (ok) { ctx.setFlag(t('ed.undone')); syncInvertHint(); } });
  } else if (k === 'clear') {
    ctx.painter.clear();
    syncInvertHint();
    showHint(true);
  }
}

function syncZoomPills() {
  if (!ctx) return;
  const fit = ctx.viewport.isFit;
  ctx.hudFit.classList.toggle('is-on', fit);
  ctx.hudOne.classList.toggle('is-on', !fit && ctx.viewport.zoomPct === 100);
}

/* ==================== 遮罩保存 ==================== */
/** id 由画笔模块随笔迹一起交回来：await 之后再读 ctx.imgId，切过图就会把旧图的笔迹写进新图的遮罩文件 */
async function onMaskSaved({ empty, b64, id }) {
  if (!id) return true;
  const here = id === ctx.imgId;                 // 这批笔迹是否还属于屏幕上这张
  if (here) ctx.setFlag(t('ed.saving'), 'busy');
  try {
    await api.saveMask(id, b64);
    touchImage(id, { has_mask: !empty });
    if (here) {
      ctx.setFlag(empty ? t('ed.maskCleared') : t('ed.maskSaved'), 'ok');
      if (!empty) ctx.hint.hidden = true;
    }
    syncFilm();
    return true;
  } catch (e) {
    if (here) ctx.setFlag(t('ed.saveFail'), 'err');
    toastErr(t('ed.maskSaveFail'), e.message);
    return false;                                // 画笔模块据此把这批笔迹重新标脏并排一次重试
  }
}

/* ==================== 提交生成 ==================== */
/** 套用预设：LoRA 往当前工作流的骨架上贴，本机没有的置灰 */
function applyPreset(p) {
  const cur = store.peek('settings') || {};
  if (isCloud()) {
    /* 云端只吃指令：预设里的步数 / CFG / LoRA 在这条线上没有对应参数 */
    const next = { prompt: p.prompt || '', negative: p.negative || '' };
    rerunFrom = null;
    ctx.params.applySettings(next, diffSettings(cur, next));
    ctx.params.line(t('ed.appliedCloudPreset', { name: p.name }));
    return;
  }
  const next = settingsFromPreset(p, cur, (store.peek('cfg') || {}).loras);
  const missing = next.loras.filter(l => l.missing);
  rerunFrom = null;
  ctx.params.applySettings(next, diffSettings(cur, next));
  ctx.params.line(missing.length
    ? t('ed.appliedPresetMissing', { name: p.name, n: missing.length })
    : t('ed.appliedPreset', { name: p.name }));
  if (missing.length) toastErr(t('ed.loraPartial'), missing.map(l => l.name).join(t('settings.sepList')));
}

/** 回填是"接着改"的起点：对比层还开着、工具还停在抓手就涂不了，退回画布并切到画笔 */
function backToCanvas() {
  ctx.compare.hide();
  setTool('brush');
  // 历史行带着当初的 invert（按行存的），回填后预览层要立刻站到同一边
  syncInvertHint();
  paintHud();
}

/* ==================== 云端局部重绘 ==================== */

/**
 * 云端 = 裁切-缝合，但整条链路在服务端一次做完：
 * 浏览器只交一个 image_id 与提示词，回来的是库里这一行的进度，不再有二跳回传。
 */
async function doSubmitCloud(snapshot) {
  const { cloud: c, settings: s, imgId, rerunOf, current: here } = snapshot;
  if (!c.key_saved) { toastErr(t('ed.cloudOff'), t('ed.cloudOffBody')); return; }
  const full = !!s?.full;
  // 反向与整张重绘都不要求涂过：前者"没涂=整幅"，后者根本不看遮罩；正向才要求先涂出区域
  if (!full && !s?.invert && !store.peek('images').find(i => i.id === imgId)?.has_mask) {
    toastErr(t('ed.noMask'), t('ed.noMaskBody'));
    return;
  }
  const st = ctx.params.stages;
  let cur = 'submit';
  /* 等模型的几十秒里可能已经切图：面板/阶段条是别人家的了，别再往上画 */
  /* 阶段条是"这一张"的面板：切了图就别再往上画，只推进游标（与本机链路同一口径） */
  const advance = (key, msg) => {
    if (!here()) { cur = key; return; }
    st.set(cur, 'done'); cur = key; st.set(key, 'run'); if (msg) ctx.params.line(msg);
  };
  const fail = msg => {
    const text = String(msg || t('ed.noReason')).slice(0, 200);
    if (here()) {
      st.set(cur, 'err');
      ctx.params.setBusy(false);
      ctx.params.line(text);
    }
    setJob(imgId, { state: 'err', error: text });
    toastErr(t('ed.cloudFail'), text.slice(0, 140));
  };

  st.reset();
  st.set('submit', 'run');
  ctx.params.setBusy(true);
  ctx.params.line(full ? t('ed.queuingFull') : t('ed.queuingCrop'));
  ctx.compare.hide();
  lastSubmitted = structuredClone(s);

  let r;
  try {
    r = await api.cloudEdit({
      image_id: imgId, rerun_of: rerunOf,
      settings: {
        prompt: cloudPrompt(s), negative: '', steps: 0, cfg: 0, loras: [],
        // 尺寸胶囊选出来的长边交给服务端按这一行执行（0/空 = 用设置里填的 stitch_edge）
        edge: Number(s.edge) || 0,
        // 反向标志按行存进 results.settings_json：历史列回放才认得出"这张是反向生成的"
        invert: !!s.invert,
        // 整张重绘同样按行存：它连遮罩都不读，回来也不缝合
        full,
        cloud: { model: c.model, quality: c.quality },
      },
    });
  } catch (e) { fail(e.message || e); return; }
  if (!r?.result_id) { fail(dx(r?.error, r?.error_args) || t('ed.cloudBadReply')); return; }

  setJob(imgId, { state: 'run', resultId: r.result_id, error: null });
  advance('sample', full ? t('ed.cloudGen', { id: r.result_id }) : t('ed.cloudCrop', { id: r.result_id }));
  // 之后与本机链路同一套：轮库里这一行，缝合与落盘都在服务端推进
  adopt(r.result_id, imgId, {
    onTick: status => {
      if (!here()) return;
      if (status === 'running' || status === 'queued') st.set('sample', 'run');
      else if (status === 'done') { if (full) st.set('sample', 'done'); else advance('stitch', t('ed.serverStitch')); }
      else if (status === 'error') { st.set(cur, 'err'); }
    },
    onDone: done => {
      if (done?.status === 'done' && !full) advance('stitch');
      if (here()) onJobSettled(done);
    },
  });
}

async function doSubmitLocal(snapshot) {
  const { imgId, settings, rerunOf, current: mine, claim } = snapshot;
  if (settings.full) { toastErr(t('ed.fullNeedsCloud'), t('ed.fullNeedsCloudBody')); return; }
  const img = store.peek('images').find(i => i.id === imgId);
  if (!img?.has_mask) { toastErr(t('ed.noMask'), t('ed.noMaskLocalBody')); return; }
  lastSubmitted = structuredClone(settings);
  const st = ctx.params.stages;
  let cur = 'submit';                 // 当前推进到哪一段，出错时就标红这一段
  /* 采样那几十秒里可能已经切图：阶段条与提示语是"这张图"的，别画到别人家的面板上 */
  const advance = (key, msg) => {
    if (!mine()) { cur = key; return; }      // 已经切走：只推进游标，不动别人家面板上的阶段条
    st.set(cur, 'done'); cur = key; st.set(key, 'run'); if (msg) ctx.params.line(msg);
  };

  st.reset();
  st.set('submit', 'run');
  ctx.params.setBusy(true);
  ctx.params.line(t('ed.uploading'));
  ctx.compare.hide();

  const r = await submit([imgId], settings, {
    claim, comfyUrl: snapshot.comfyUrl,
    quiet: true,
    rerunOf,
    onTick: status => {
      if (status === 'running') advance('sample', t('ed.comfySampling'));
      else if (status === 'error') { if (mine()) { st.set(cur, 'err'); ctx.params.line(t('ed.comfyError'), true); } }
      else advance('stitch', t('ed.returning'));
    },
    onDone: status => { if (mine()) onJobSettled(status); },
  });

  if (!r.ok) {
    if (mine()) {
      st.set(cur, 'err');
      ctx.params.setBusy(false);
      ctx.params.line(dx(r.error, r.error_args) || t('gen.noneAccepted'), true);
    }
    return;
  }
  advance('queue', t('ed.queued'));
}

// 在任何保存等待前捕获输入并认领图片；失败和过期均释放本次占位。
async function doSubmit() {
  if (!ctx?.imgId) return;
  if (ctx.info?.orig_dead) { toastErr(t('ed.origGone'), t('ed.origGoneBody')); return; }
  if (ctx.adjust?.geometryActive) { toastErr(t('ed.geomPending'), t('ed.geomPendingBody')); return; }
  const claim = claimSubmission([ctx.imgId]);
  if (!claim) return;
  const revision = ctx.adjust.revision;
  const snapshot = { ...captureAction(), claim, comfyUrl: store.peek('comfy'), settings: structuredClone(store.peek('settings')), cloud: structuredClone(store.peek('cloud') || {}), cloudMode: isCloud(), rerunOf: rerunFrom };
  try {
    if (await ctx.adjust.flush() === false || !snapshot.current()) return;
    if (await ctx.painter.flush() === false || !snapshot.current() || ctx.adjust.revision !== revision) return;
    await saveSettings();
    if (!snapshot.current()) return;
    rerunFrom = null;
    return await (snapshot.cloudMode ? doSubmitCloud(snapshot) : doSubmitLocal(snapshot));
  } catch (e) { if (snapshot.current()) { ctx.params.setBusy(false); toastErr(t('ed.saveFail'), e.message); } }
  finally { claim.release(); }
}

/** 任务终态 → 刷新历史并自动进入对比 */
async function onJobSettled(r) {
  if (!ctx?.imgId) return;
  /* 后台任务完成时你可能已经在看另一张：别把它的成图弹到你当前这张上 */
  if (r?.image_id && r.image_id !== ctx.imgId) return;
  ctx.params.setBusy(false);
  // 带得上原因就直接说原因：轮询放弃那两条出口（记录没了、连续读不到）并没有右下角通知可看
  if (r.status !== 'done') { ctx.params.line(r.error ? dx(r.error, r.error_args).slice(0, 120) : t('ed.genFailNote'), true); return; }
  ctx.params.stages.finish(true);
  ctx.params.line(t('ed.doneDrag'));
  const { imgId, current: same } = captureAction();
  try {
    const info = await api.image(imgId);
    if (!same()) return;   // 这一阵里切了图：别把上一张的结果画到当前这张上
    ctx.info = info;
    ctx.history.setResults(info.results, info.last?.id);
    if (info.last?.status === 'done') showCompare(info.last);
    syncExport();
  } catch { /* 刷新失败不影响已完成的生成 */ }
}

/**
 * 中断这一张正在跑的任务。
 * 本机：给 ComfyUI 发 /interrupt 并把它从队列移走；云端那次请求在对面跑着撤不回来，
 * 只把记录与界面解开，结果回来时 adopt 会因为不再是 running 而被拒。
 */
async function stopCurrent() {
  const { imgId, current } = captureAction();
  if (!imgId) return;
  const rid = store.peek('jobs')[imgId]?.resultId;
  if (!rid) {
    toastErr(t('ed.stopCloudNo'), t('ed.stopCloudNoBody'));
    return;
  }
  const ok = await confirm({
    title: t('ed.stopTitle', { id: rid }),
    text: isCloud() ? t('ed.stopCloudText')
                    : t('ed.stopLocalText'),
    okLabel: t('ed.stop'), danger: true,
  });
  if (!ok || !current()) return;
  try {
    await api.interruptResult(rid);
    drop(rid);                                   // 别再轮询它，否则下一轮又把状态刷回 running
    setJob(imgId, { state: 'err', resultId: rid, error: t('ed.stoppedByUser') });
    if (current()) {
      ctx.params.setBusy(false);
      ctx.params.stages.finish(false);
      ctx.params.line(t('ed.interrupted'));
      try {
        const info = await api.image(imgId);
        if (!current()) return;
        ctx.info = info;
        ctx.history.setResults(info.results, null);
        syncExport();
      } catch { /* 列表刷新失败不影响已中断 */ }
    }
    toastOk(t('ed.stopped'), `#${rid}`);
  } catch (e) { toastErr(t('ed.stopFail'), e.message); }
}

function showCompare(r) {
  if (!r?.final_url) { toastErr(t('ed.noResultYet')); return; }
  if (r.final_dead) { toastErr(t('ed.resultGone'), t('ed.resultGoneShort')); return; }
  if (ctx.info?.orig_dead) { toastErr(t('film.lostFile'), t('ed.compareNeedsOrig')); return; }
  ctx.cmpId = r.id;
  ctx.compare.show({
    // 对比看的是 proxy 档：两张 20–33MB 的 PNG 一起解码是"点大图卡"的直接来源
    origUrl: ctx.info.proxy_url || ctx.info.orig_url,
    resultUrl: r.final_url,
    cropUrl: r.crop_url,
    overlayUrl: r.maskoverlay_url,
    title: `#${r.id} · ${resultTag(r)}`,
    result: r,
  });
}

/* 「导出」「对比」「下载」找的是同一条：最新一张文件还在盘上的成图 */
// 图片装载世代和视图世代都属于动作身份，覆盖 A→B→A 与关闭重开。
function captureAction() {
  const owner = ctx, imgId = ctx?.imgId, projectId = ctx?.projectId, seq = ctx?.showSeq;
  const live = session.capture();
  return { imgId, projectId, current: () => live() && ctx === owner && ctx.imgId === imgId && ctx.showSeq === seq };
}

const aliveDone = () => (ctx.info?.results || []).find(r => r.status === 'done' && !r.final_dead);

function openBestCompare() {
  const done = aliveDone();
  if (!done) { toastErr(t('ed.noResult'), t('ed.noResultBody')); return; }
  showCompare(done);
}

function syncExport() {
  ctx.exportBtn.disabled = !aliveDone();
}

/* ==================== 胶片条 ==================== */
function syncFilm() {
  const { images, sel } = store.get();
  ctx.film.sync({ images, curId: ctx.imgId, sel, stateOf });
}

/* ==================== 快捷键 ==================== */
function wireKeys(c) {
  const keydown = e => {
    if ($('#editor').hidden) return;
    const tag = document.activeElement?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA') {
      if (e.key === 'Escape') document.activeElement.blur();
      return;
    }
    /* 对比层开着时，缩放类按键归对比层，别去动背后那张画布 */
    const vp = c.compare.isOn ? c.compare : c.viewport;
    if (e.code === 'Space') { e.preventDefault(); vp.setSpace(true); return; }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z') { e.preventDefault(); onEdit('undo'); return; }
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
      /* .is-busy 只挡鼠标点击，键盘绕得过去：生成中再按就是多排一条任务、多烧一次云端额度 */
      if (c.params.busy) { toastErr(t('ed.busySwitch'), t('ed.busySwitchBody')); return; }
      e.preventDefault(); doSubmit(); return;
    }
    const k = e.key.toLowerCase();
    if (k === 'b' && !c.compare.isOn) setTool('brush');
    else if (k === 'e' && !c.compare.isOn) setTool('erase');
    else if (k === 'h' && !c.compare.isOn) setTool('pan');
    else if (k === '0') vp.fit();
    else if (k === '1') vp.one2one();
    else if (k === '+' || k === '=') vp.zoomBy(1.3);
    else if (k === '-') vp.zoomBy(1 / 1.3);
    else if (k === '?') shortcutsModal();
    else if (e.key === 'Escape' && c.compare.isOn) c.compare.hide();
  };
  const keyup = e => { if (e.code === 'Space') (c.compare.isOn ? c.compare : c.viewport).setSpace(false); };
  window.addEventListener('keydown', keydown);
  window.addEventListener('keyup', keyup);
  // 窗口缩窄时收起历史列，让参数与画布仍有可用宽度；按钮状态同步更新。
  const panels = window.matchMedia('(max-width: 960px)');
  panels.addEventListener('change', () => {
    const ed = $('#editor');
    if (panels.matches && appearance() === 'glass' && !ed.classList.contains('no-pre') && !ed.classList.contains('no-prop')) {
      toggleCol('no-pre', 'pre');
    }
  });
}

// 控制器延迟装配，所有动作共享入口当前上下文与重跑快照。
let resultController;
function resultActions() {
  return resultController ||= createResultActions({
    context: () => ctx, submitted: () => lastSubmitted, setRerun: id => { rerunFrom = id; },
    backToCanvas, syncExport, aliveDone, capture: captureAction,
  });
}
function forkAdjusted(...args) { return resultActions().forkAdjusted(...args); }
function forkResult(...args) { return resultActions().forkResult(...args); }
function delResult(...args) { return resultActions().delResult(...args); }
function restoreFromResult(...args) { return resultActions().restoreFromResult(...args); }
function exportCurrent(...args) { return resultActions().exportCurrent(...args); }
