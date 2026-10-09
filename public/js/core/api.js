// 网络层：唯一与后端 REST 契约打交道的地方，视图不直接 fetch
'use strict';

import { t } from './i18n.js';

export class ApiError extends Error {
  constructor(msg, status) { super(msg); this.name = 'ApiError'; this.status = status || 0; }
}

async function request(path, { method = 'GET', body, signal } = {}) {
  let res;
  try {
    res = await fetch(path, body === undefined && method === 'GET'
      ? { method, signal }
      : { method, signal, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  } catch (e) {
    if (e.name === 'AbortError') throw e;
    throw new ApiError(t('api.offline'), 0);
  }
  const text = await res.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch { data = { raw: text }; }
  if (!res.ok) throw new ApiError(data?.error || `HTTP ${res.status}`, res.status);
  return data;
}

export const api = {
  get:  p => request(p),
  post: (p, body) => request(p, { method: 'POST', body: body ?? {} }),
  del:  p => request(p, { method: 'DELETE' }),

  projects:    () => request('/api/projects'),
  project:     id => request(`/api/projects/${id}`),
  createProject: (name, files) => request('/api/projects', { method: 'POST', body: { name, files } }),
  renameProject: (id, name) => request(`/api/projects/${id}/rename`, { method: 'POST', body: { name } }),
  deleteProject: id => request(`/api/projects/${id}`, { method: 'DELETE' }),
  addImages:   (id, files) => request(`/api/projects/${id}/images`, { method: 'POST', body: { files } }),
  saveSettings: (id, settings) => request(`/api/projects/${id}/settings`, { method: 'POST', body: settings }),

  image:   id => request(`/api/images/${id}`),
  /** 画布（图生图）：建空白画布、存画稿、提交生成。画稿不走局部重绘那两条入口 */
  canvasCreate:   opt => request('/api/canvas/create', { method: 'POST', body: opt }),
  canvas:         id => request(`/api/canvas/${id}`),
  saveSketch:     (id, b64) => request(`/api/canvas/${id}/sketch`, { method: 'POST', body: { b64 } }),
  canvasGenerate: (id, settings, rerunOf) => request(`/api/canvas/${id}/generate`, { method: 'POST', body: { settings, rerun_of: rerunOf || null } }),
  /** 把某一版当时的线稿快照写回画稿本体（"这条不满意 → 回到当时那版笔迹接着改"） */
  useSketch: (id, resultId) => request(`/api/canvas/${id}/use-sketch`, { method: 'POST', body: { result_id: resultId } }),
  /** 参考图槽位：加（files[].b64 / image_ids[] / from_result）与整组替换。集合存服务端，刷新还在 */
  canvasAddRefs: (id, opt) => request(`/api/canvas/${id}/refs`, { method: 'POST', body: opt }),
  canvasSetRefs: (id, paths) => request(`/api/canvas/${id}/refs`, { method: 'PUT', body: { paths } }),
  delImage: id => request(`/api/images/${id}`, { method: 'DELETE' }),
  /** 这张图「另存为新图」出去的那些子图（库里按 derived_from 查，不靠文件名猜） */
  imageDerived: id => request(`/api/images/${id}/derived`),
  saveMask: (id, b64) => request(`/api/images/${id}/mask`, { method: 'POST', body: { b64 } }),
  /** 派生档是服务端后台切的，没补出来时 thumb_url 为 null，调用方回落到 orig_url */
  thumb:   id => request(`/api/images/${id}/thumb`),
  /** 瓦片清单：首次访问会当场切一套，所以这个请求可能慢 */
  tiles:   id => request(`/api/images/${id}/tiles`),
  /** 本地调整（0.3）：读参数+预设/LUT 名单、存参数、预览、成图、另存为新图 */
  adjust:       id => request(`/api/images/${id}/adjust`),
  saveAdjust:   (id, ops) => request(`/api/images/${id}/adjust`, { method: 'POST', body: { ops } }),
  adjustPreview: id => request(`/api/images/${id}/adjust/preview`, { method: 'POST', body: {} }),
  /** 带 inline 参数的那次预览只有一种场合用得到：裁切 overlay 背后要铺"没裁但其它都算完"的那一张 */
  adjustPreviewWith: (id, ops) => request(`/api/images/${id}/adjust/preview`, { method: 'POST', body: { ops } }),
  adjustRender: id => request(`/api/images/${id}/adjust/render`, { method: 'POST', body: {} }),
  adjustFork:   id => request(`/api/images/${id}/adjust/fork`, { method: 'POST', body: {} }),
  /** 调整视图的真像素档：成图（原分辨率）切的一套瓦片。参数为空时服务端直接回源图那一套 */
  adjustTiles:  id => request(`/api/images/${id}/adjust/tiles`),

  run:      (imageIds, settings, rerunOf) => request('/api/run', { method: 'POST', body: { image_ids: imageIds, settings, rerun_of: rerunOf || null } }),
  result:   id => request(`/api/results/${id}`),
  /** 结果集合：{project_id} 或 {image_id}，项目页的派生查看用（不带判僵尸之类的副作用） */
  results:  q => request('/api/results?' + new URLSearchParams(q)),
  delResult: id => request(`/api/results/${id}`, { method: 'DELETE' }),
  interruptResult: id => request(`/api/results/${id}/interrupt`, { method: 'POST', body: {} }),
  forkResult: id => request(`/api/results/${id}/fork`, { method: 'POST', body: {} }),

  cfg: () => request('/api/cfg'),

  backends:      () => request('/api/backends'),
  scanBackends:  () => request('/api/backends/scan', { method: 'POST', body: {} }),
  addBackend:    (url, label) => request('/api/backends', { method: 'POST', body: { url, label } }),
  removeBackend: url => request('/api/backends/remove', { method: 'POST', body: { url } }),
  selectBackend: url => request('/api/backends/select', { method: 'POST', body: { url } }),

  /** kind='preset' 是参数预设，kind='phrase' 是修图界面那一排提示词短语（共用一套 CRUD） */
  presets:       (projectId, kind = 'preset') => request(`/api/presets?kind=${kind}` + (projectId ? `&project_id=${projectId}` : '')),
  savePreset:    p => request('/api/presets', { method: 'POST', body: p }),
  updatePreset:  (id, p) => request(`/api/presets/${id}/update`, { method: 'POST', body: p }),
  deletePreset:  id => request(`/api/presets/${id}/delete`, { method: 'POST' }),

  setWorkflow:   path => request('/api/settings/workflow', { method: 'POST', body: { path } }),
  /** 清点当前存着的工作流文件里有哪些节点（只认库里那条路径，不接参数） */
  workflowInspect: () => request('/api/workflow/inspect'),
  /** 角色映射：这个文件里谁负责装图、谁负责采样、成图从哪个节点读回来 */
  workflowRoles: () => request('/api/workflow/roles'),
  setWorkflowRoles: roles => request('/api/workflow/roles', { method: 'POST', body: { roles } }),

  exportGet:    () => request('/api/export'),
  exportSetDir: dir => request('/api/export/dir', { method: 'POST', body: { dir } }),
  exportRun:    resultIds => request('/api/export/run', { method: 'POST', body: { result_ids: resultIds } }),

  cloud:       () => request('/api/cloud'),
  saveCloud:   patch => request('/api/cloud', { method: 'POST', body: patch }),
  testCloud:   () => request('/api/cloud/test', { method: 'POST', body: {} }),
  /** 一次请求：裁切、调云端、缝合、落盘全在服务端做完，进度看这条 results 行 */
  cloudEdit:   p => request('/api/cloud/edit', { method: 'POST', body: p }),
  /** 批量：建 N 行 queued，服务端按 concurrency 排着跑，关了页面也继续 */
  cloudQueue:  (imageIds, settings, rerunOf) => request('/api/cloud/queue', { method: 'POST', body: { image_ids: imageIds, settings, rerun_of: rerunOf || null } }),
  queueState:  () => request('/api/cloud/queue'),

  setProxyEdge: edge => request('/api/settings/proxy-edge', { method: 'POST', body: { proxy_edge: edge } }),
  /** 全局设置一次读回：工作流路径、生效后端、proxy 档位、界面语言 */
  settings:   () => request('/api/settings'),
  /** 界面语言存 app_settings（跟着库走，重装不丢）；服务端只认 zh/en，别的夹回 zh */
  setLang:    lang => request('/api/settings/lang', { method: 'POST', body: { lang } }),
  /** 字典是 `public/locales/` 下的静态文件，no-store；fetch 仍只走这一个口（R5） */
  locale:     l => request(`/public/locales/${l}.json`),

  setup:        () => request('/api/setup'),
  setupProgress: () => request('/api/setup/progress'),
  setSetupRoot: path => request('/api/setup/root', { method: 'POST', body: { path } }),
  genSetup:     opt => request('/api/setup/script', { method: 'POST', body: opt || {} }),
  verifySetup:  path => request('/api/setup/verify', { method: 'POST', body: { path } }),
};

/** File → { name, b64, w, h }：后端要求 base64 dataURL + 原始宽高 */
export async function fileToPayload(file) {
  const b64 = await new Promise((res, rej) => {
    const rd = new FileReader();
    rd.onload = () => res(rd.result);
    rd.onerror = () => rej(new ApiError(t('api.fileRead', { name: file.name })));
    rd.readAsDataURL(file);
  });
  const { width: w, height: h } = await new Promise(res => {
    const im = new Image();
    im.onload = () => res(im);
    im.onerror = () => res({ naturalWidth: 0, naturalHeight: 0 });
    im.src = b64;
  });
  return { name: file.name, b64, w, h };
}

const isImage = f => /^image\//.test(f.type) || /\.(jpe?g|png|webp|bmp|avif)$/i.test(f.name);
/* 整包转 base64 会多占三分之一内存，所以一批既限张数也限体积：
   后端 body 上限 80MB，只按张数切的话 8 张 9MB 的照片（一张漫展原图就是这个量级）
   编出来就超 100MB，第一批正撞 413——建第一个项目恰好卡在这里 */
const CHUNK = 8;
const CHUNK_BYTES = 40 * 1024 * 1024;

/** 张数与体积双限切批：单张就超预算的也照样自成一批，让后端的"太大"话说给那一张 */
function batches(files) {
  const out = [];
  let cur = [];
  let bytes = 0;
  for (const f of files) {
    const sz = Number(f.size) || 0;
    if (cur.length && (bytes + sz > CHUNK_BYTES || cur.length === CHUNK)) { out.push(cur); cur = []; bytes = 0; }
    cur.push(f);
    bytes += sz;
  }
  if (cur.length) out.push(cur);
  return out;
}

/**
 * 分批转码 + 分批上传。projectId 为空时第一批建项目，后面几批续传。
 * @returns {Promise<{id:number|null, image_ids:number[], skipped:number}>}
 */
export async function importPhotos(list, { projectId = null, name = '', onProgress } = {}) {
  const all = Array.from(list || []);
  const files = all.filter(isImage);
  const groups = batches(files);
  const ids = [];
  let pid = projectId;
  let done = 0;
  for (const group of groups) {
    const payloads = [];
    for (const f of group) {
      try { payloads.push(await fileToPayload(f)); } catch { /* 单张坏图不阻断整批 */ }
    }
    if (payloads.length) {
      try {
        if (!pid) { const r = await api.createProject(name, payloads); pid = r.id; ids.push(...r.image_ids); }
        else { const r = await api.addImages(pid, payloads); ids.push(...r.ids); }
      } catch (e) {
        /* 项目已经建起来了，说清楚停在第几张，免得用户以为一张都没进 */
        if (ids.length) e.message = t('api.importAborted', { n: ids.length, msg: e.message });
        throw e;
      }
    }
    done += group.length;
    onProgress?.(done, files.length);
  }
  return { id: pid, image_ids: ids, skipped: all.length - files.length };
}

/** 拖拽事件里的目录/文件条目拍平成 File[] */
export async function dropToFiles(dataTransfer) {
  const items = Array.from(dataTransfer.items || []);
  const entries = items.map(i => i.webkitGetAsEntry?.()).filter(Boolean);
  if (!entries.length) return Array.from(dataTransfer.files || []);
  const out = [];
  const walk = async entry => {
    if (entry.isFile) {
      const f = await new Promise(res => entry.file(res, () => {}));
      if (f) out.push(f);
    } else if (entry.isDirectory) {
      const reader = entry.createReader();
      const kids = [];
      /* readEntries 每轮最多返回 100 条，不循环就会把几百张的文件夹截断 */
      for (;;) {
        const batch = await new Promise(res => reader.readEntries(res, () => res([])));
        if (!batch.length) break;
        kids.push(...batch);
      }
      for (const k of kids) await walk(k);
    }
  };
  for (const e of entries) await walk(e);
  return out;
}
