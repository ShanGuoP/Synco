// 应用状态层：跨视图共享的唯一事实来源（当前项目 / 图片 / 勾选 / 参数 / 任务状态）
// 视图只读 store + 调 action，不互相持有引用
'use strict';
import { createStore } from './core/store.js';
import { api } from './core/api.js';
import { baseName } from './core/format.js';

export const store = createStore({
  cfg: null,
  projects: [],
  projectsAt: 0,
  project: null,
  images: [],
  sel: [],                 // 勾选的 imageId 数组
  settings: null,          // 当前修图参数
  jobs: {},                // imageId -> {state:'run|done|err|skip', resultId, error, t0}
  editor: null,            // {imgId, tool, brush, zoom, fit}
  home: { filter: 'all', query: '' },   // 首页筛选 + 顶栏搜索词
  stats: {},               // projectId -> {total, masked} 由详情请求补齐
  comfy: null,             // 当前生效的 ComfyUI 后端地址
  cloud: null,             // 云端局部重绘配置（base/模型/挡位/外扩羽化），key 不回传
});

/** 生成走哪条线：云端那套给没装 ComfyUI 的机器用 */
export const isCloud = () => store.peek('cloud')?.kind === 'cloud';

/** 云端没有独立的负面提示词位，并进正向一句发过去 */
export function cloudPrompt(settings) {
  const p = String(settings?.prompt || '').trim();
  const n = String(settings?.negative || '').trim();
  return n ? (p ? `${p}。避免：${n}` : `避免：${n}`) : p;
}

/* ---------- 首页筛选 / 搜索 ---------- */
export function setHome(patch) {
  store.set({ home: { ...store.peek('home'), ...patch } }, 'home');
}
export function setStat(projectId, stat) {
  store.set({ stats: { ...store.peek('stats'), [projectId]: stat } }, 'stats');
}
export const statOf = id => store.peek('stats')[id] || null;

/* ---------- 参数模型 ---------- */
export function defaultsFromCfg(cfg) {
  return {
    prompt: cfg.prompt_default || '',
    negative: cfg.negative || '',
    steps: cfg.steps || 20,
    cfg: cfg.cfg || 3,
    seed: 0,
    randomSeed: true,
    loras: (cfg.loras || []).map(l => ({ name: l.name, strength: l.strength ?? 1, enabled: l.enabled !== false })),
  };
}

const pick = (src, keys, fb) => {
  const out = { ...fb };
  for (const k of keys) if (src && src[k] !== undefined && src[k] !== null) out[k] = src[k];
  return out;
};
const KEYS = ['prompt', 'negative', 'steps', 'cfg', 'seed', 'randomSeed', 'loras', 'edge'];

/** 项目记忆的参数 > 工作流默认；LoRA 以工作流为骨架、按名字合并强度/开关 */
export function resolveSettings(saved, cfg) {
  const base = defaultsFromCfg(cfg);
  const s = pick(saved, KEYS, base);
  /* 与预设走同一套语义：本机工作流里没有的 LoRA 保留并标 missing，而不是在这里被丢掉
     （丢掉的那次之后 saveSettings 会回写，预设里的 LoRA 就永久消失了） */
  s.loras = mergeLoras(base.loras, s.loras);
  s.steps = Number(s.steps) || base.steps;
  s.cfg = Number(s.cfg) || base.cfg;
  s.seed = Number(s.seed) || 0;
  return s;
}

/* ---------- 参数比对与合并 ---------- */
export const loraSig = ls => (ls || []).map(l => `${l.name}|${l.strength}|${l.enabled ? 1 : 0}`).join(';');

/** 两组参数不同的键，用于回填后高亮 */
export function diffSettings(a, b) {
  const out = ['prompt', 'negative', 'steps', 'cfg'].filter(k => String(a?.[k] ?? '') !== String(b?.[k] ?? ''));
  if (loraSig(a?.loras) !== loraSig(b?.loras)) out.push('loras');
  return out;
}

/** 把预设的 LoRA 期望值套到当前工作流的 LoRA 骨架上；本机没有的置灰并标 missing */
export function mergeLoras(base, wanted) {
  const b = base || [], w = wanted || [];
  const hit = n => w.find(x => x.name === n);
  const out = b.map(d => { const x = hit(d.name); return x ? { ...d, strength: Number(x.strength) || 0, enabled: !!x.enabled } : { ...d }; });
  const extra = w.filter(x => !b.some(d => d.name === x.name)).map(x => ({ ...x, enabled: false, missing: true }));
  return [...out, ...extra];
}

/** 预设 → settings：种子不进预设，沿用面板当前值 */
export function settingsFromPreset(p, cur, cfgLoras) {
  return {
    prompt: p.prompt || '', negative: p.negative || '',
    steps: p.steps ?? 20, cfg: p.cfg ?? 3,
    seed: cur?.seed ?? 0, randomSeed: cur?.randomSeed !== false,
    loras: mergeLoras(cfgLoras ?? (store.peek('cfg') || {}).loras, p.loras),
  };
}

/* ---------- 数据装载 ---------- */
export async function loadProjects() {
  const projects = await api.projects();
  store.set({ projects, projectsAt: Date.now() }, 'projects');
  return projects;
}

export async function loadProject(id) {
  const d = await api.project(id);
  if (!d.project) throw new Error('项目不存在');
  const cfg = store.peek('cfg') || await api.cfg().then(c => { store.set({ cfg: c }); return c; });
  store.set({
    project: d.project,
    images: d.images.map(normImage),
    sel: [],
    settings: resolveSettings(safeJson(d.project.settings_json), cfg),
  }, 'project');
  return d;
}

const normImage = i => ({ ...i, stem: baseName(i.name) });

export function saveSettings() {
  const { project, settings } = store.get();
  if (!project || !settings) return Promise.resolve();
  return api.saveSettings(project.id, settings).catch(() => { /* 参数写库失败不阻断生成 */ });
}

export function patchSettings(patch) {
  store.set({ settings: { ...store.peek('settings'), ...patch } }, 'settings');
}

export function collectLoras(list) {
  patchSettings({ loras: list.map(l => ({ name: l.name, strength: l.strength, enabled: l.enabled })) });
}

function safeJson(s) { try { return JSON.parse(s || '{}'); } catch { return {}; } }

/* ---------- 勾选 ---------- */
export const isSel = id => store.peek('sel').includes(id);
export function toggleSel(id, on) {
  const cur = store.peek('sel');
  const has = cur.includes(id);
  const want = on ?? !has;
  if (want === has) return;
  store.set({ sel: want ? [...cur, id] : cur.filter(x => x !== id) }, 'sel');
}
export function selectWhere(pred) {
  store.set({ sel: store.peek('images').filter(pred).map(i => i.id) }, 'sel');
}
export const clearSel = () => store.set({ sel: [] }, 'sel');
export function invertSel() {
  const cur = new Set(store.peek('sel'));
  store.set({ sel: store.peek('images').filter(i => !cur.has(i.id)).map(i => i.id) }, 'sel');
}

/* ---------- 任务状态（网格 / 胶片条 / 编辑器共用） ---------- */
export function jobOf(imgId) { return store.peek('jobs')[imgId] || null; }
export function setJob(imgId, patch) {
  const jobs = { ...store.peek('jobs') };
  jobs[imgId] = { state: 'idle', t0: Date.now(), ...jobs[imgId], ...patch };
  store.set({ jobs }, 'jobs');
}
export function touchImage(imgId, patch) {
  store.set({ images: store.peek('images').map(i => (i.id === imgId ? { ...i, ...patch } : i)) }, 'images');
}

/* ---------- 图片状态推导：给卡片角标 / 胶片条状态点用 ---------- */
export function stateOf(img) {
  const job = jobOf(img.id);
  if (job && job.state === 'run') return 'run';
  if (job && job.state === 'err') return 'err';
  if (job && job.state === 'skip') return 'skip';
  if (job && job.state === 'done') return 'done';
  if (img.has_result) return 'done';
  if (!img.has_mask) return 'nomask';
  return 'ready';
}

export const STATE_TEXT = {
  run: '生成中', done: '已出图', err: '失败', skip: '已跳过',
  nomask: '未涂遮罩', ready: '待提交',
};

export default store;
