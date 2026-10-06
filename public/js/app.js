// 应用入口：装载配置 → 装配外壳 → 注册路由 → 视图切换
'use strict';
import { $ } from './core/dom.js';
import { api } from './core/api.js';
import { createRouter, go } from './core/router.js';
import { store, setHome, loadProjects, loadPhrases } from './state.js';
import { initShell, setActiveNav, setCrumb, renderRecent, refreshStats, setWorkflowChip } from './shell.js';
import { renderHome, repaintHome } from './views/home.js';
import { renderProject, watchJobs } from './views/project.js';
import { openEditor, closeEditor } from './views/editor/index.js';
import { openCanvas, closeCanvas } from './views/canvas/index.js';
import { helpModal, shortcutsModal, flowModal } from './ui/dialogs.js';
import { settingsModal } from './ui/settings.js';
import { toastErr } from './ui/toast.js';
import { apply, watch } from './core/theme.js';

const show = which => {
  $('#viewHome').classList.toggle('is-active', which === 'home');
  $('#viewProject').classList.toggle('is-active', which === 'project');
  $('#scroll').scrollTop = 0;
};

/* ---------- 路由 ---------- */
const router = createRouter()
  .on('/', async () => {
    closeEditor();
    closeCanvas();
    show('home');
    setHome({ filter: 'all' });
    setCrumb([]);
    setActiveNav('/');
    await renderHome();
    afterHome();
  })
  .on('/f/:filter', async params => {
    closeEditor();
    closeCanvas();
    show('home');
    setHome({ filter: params.filter });
    setCrumb([]);
    setActiveNav(`/f/${params.filter}`);
    await renderHome(params.filter);
    afterHome();
  })
  .on('/p/:id', async params => {
    closeEditor();
    closeCanvas();
    show('project');
    await renderProject(+params.id);
    const p = store.peek('project');
    setCrumb([{ label: '主页', href: '/' }, { label: p?.name || '项目' }]);
    setActiveNav('');
  })
  .on('/p/:id/e/:imgId', async params => {
    closeCanvas();
    show('project');
    setCrumb([{ label: '主页', href: '/' }, { label: store.peek('project')?.name || '项目', href: `/p/${params.id}` }, { label: '精修' }]);
    setActiveNav('');
    await openEditor(+params.id, +params.imgId);
  })
  .on('/p/:id/c/:imgId', async params => {
    closeEditor();
    show('project');
    setCrumb([{ label: '主页', href: '/' }, { label: store.peek('project')?.name || '项目', href: `/p/${params.id}` }, { label: '画布' }]);
    setActiveNav('');
    await openCanvas(+params.id, +params.imgId);
  })
  .fallback(() => go('/'));

async function afterHome() {
  renderRecent(store.peek('projects'));
  refreshStats();
}

/* 首页详情补齐统计后，左栏的最近打开与总量跟着走（视图不直接 import 外壳） */
store.subscribe((s, key) => {
  if (key === 'projects' || key === 'stats' || key === 'comfy') { renderRecent(s.projects); refreshStats(); }
  if (key === 'cloud') paintChip();
});

/** 顶栏徽章按生成方式说人话：云端那条没有步数/CFG/LoRA 可说 */
function paintChip() {
  const cfg = store.peek('cfg') || {};
  const cloud = store.peek('cloud');
  if (cloud?.kind === 'cloud') {
    setWorkflowChip(`云端 ${cloud.model || '未填模型名'} · ${cloud.size || '未填挡位'}`);
    return;
  }
  const n = (cfg.loras || []).length;
  setWorkflowChip(cfg.cfg_source === 'workflow'
    ? `步数 ${cfg.steps} · CFG ${cfg.cfg} · LoRA ${n}`
    : '工作流未关联 · 用内置默认参数');
}

/* ---------- 启动 ---------- */
async function boot() {
  /* 底色已由 index.html 的内联脚本定过；这里再走一次是为了把生效档位递给了桌面壳，
     并且让"跟随系统"这一档在设置页没打开时也跟着变 */
  apply();
  watch(() => apply());
  initShell({
    onRefresh: () => router.refresh(),
    onHelp: () => helpModal(),
    onGuide: () => flowModal(),
    onBackends: () => settingsModal('backend'),
    onSearch: value => {
      setHome({ query: value });
      /* 搜索是本地过滤：走 renderHome 的话每敲一个字都要重新拉一遍全部项目与详情，还会顶一串"读取项目…" */
      if ($('#viewHome').classList.contains('is-active')) repaintHome();
    },
  });

  try {
    const [cfg, be, cloud] = await Promise.all([api.cfg(), api.backends(), api.cloud().catch(() => null), loadPhrases()]);
    store.set({ cfg, comfy: be.active, cloud: cloud || { kind: 'comfyui' } }, 'comfy');
    paintChip();
  } catch (e) {
    toastErr('读不到默认参数', '检查 ComfyUI 与 workflows 目录后刷新');
    setWorkflowChip('工作流未就绪');
  }

  watchJobs();
  await router.start();

  // 桌面版第一次替我们选了数据目录，把这一页摊开让用户确认（改完就摘掉参数，免得刷新再弹）
  const q = new URLSearchParams(location.search);
  if (q.get('firstrun') === '1') {
    q.delete('firstrun');
    history.replaceState(null, '', `${location.pathname}${q.toString() ? '?' + q : ''}${location.hash}`);
    settingsModal('data');
  }

  window.addEventListener('error', e => { if (e.message) toastErr('页面异常', String(e.message).slice(0, 120)); });
  window.addEventListener('unhandledrejection', e => toastErr('请求失败', String(e.reason?.message || e.reason).slice(0, 120)));
}

document.addEventListener('keydown', e => {
  if (e.key === '?' && e.shiftKey) shortcutsModal();
});

/* 桌面版为了拿回 HTML5 的 drop，关掉了壳自己的拖放拦截；代价是落在导入卡之外的文件拖放会走
   原生默认行为——整个页面被导航到那张图的 file:// 地址。这里只接文件拖放：页面内拖动选中文字
   的那套还要留给原生行为，不能一并掐掉。冒泡到这一层的 preventDefault 不会挡住卡片自己的处理，
   默认动作要等整条事件路径跑完才执行。 */
const isFileDrag = e => Array.from(e.dataTransfer?.types || []).includes('Files');
for (const type of ['dragenter', 'dragover', 'drop']) {
  window.addEventListener(type, e => { if (isFileDrag(e)) e.preventDefault(); });
}

boot();
