// 生成任务层：提交 / 轮询 / 状态回写，项目页批量与编辑器单张共用
'use strict';
import { api, ApiError } from './core/api.js';
import { store, setJob, touchImage, saveSettings } from './state.js';
import { toast, toastErr, toastOk, toastBusy } from './ui/toast.js';

/** resultId -> { imgId, t0, onDone } */
const pending = new Map();
const listeners = new Set();
let timer = 0;

export const pendingCount = () => pending.size;
export function onTick(fn) { listeners.add(fn); return () => listeners.delete(fn); }
const emit = () => { for (const fn of [...listeners]) { try { fn(pendingCount()); } catch (e) { console.error(e); } } };

function notify() {
  if (!pending.size) { clearInterval(timer); timer = 0; }
  emit();
}

async function tick() {
  if (!pending.size) return notify();
  for (const [rid, job] of [...pending]) {
    let r;
    try { r = await api.result(rid); } catch (e) { continue; /* 单次网络抖动，下轮再试 */ }
    // 服务端队列里的 queued 与 running 都还没落定，都算"在跑"
    if (r.status === 'running' || r.status === 'queued') { job.onTick?.(r.status, r); continue; }
    pending.delete(rid);
    job.onTick?.(r.status, r);
    if (r.status === 'done') {
      setJob(job.imgId, { state: 'done', resultId: rid, error: null });
      touchImage(job.imgId, { has_result: true, last_result: r });
      toastOk('生成完成', job.t0 ? `#${rid} · 用时 ${Math.round((Date.now() - job.t0) / 1000)} 秒` : `#${rid}`);
    } else {
      setJob(job.imgId, { state: 'err', resultId: rid, error: r.error || 'ComfyUI 未返回原因' });
      toastErr('生成失败', String(r.error || '').slice(0, 120));
    }
    job.onDone?.(r);
  }
  notify();
}

function startPolling() { if (!timer) timer = setInterval(tick, 2500); tick(); }

/**
 * 提交一批图片去生成。
 * @param {number[]} ids
 * @param {object} settings
 * @param {{onDone?:Function, quiet?:boolean}} [opt]
 * @returns {Promise<{ok:number, skipped:Array, error?:string}>}
 */
export async function submit(ids, settings, opt = {}) {
  if (!ids.length) return { ok: 0, skipped: [] };
  await saveSettings();
  const done = [];
  for (const id of ids) setJob(id, { state: 'run', error: null, resultId: null });

  let r;
  try {
    r = await api.run(ids, settings, opt.rerunOf);
  } catch (e) {
    for (const id of ids) setJob(id, { state: 'err', error: e.message });
    toastErr('提交失败', e instanceof ApiError ? e.message : String(e));
    return { ok: 0, skipped: [], error: e.message };
  }

  /* 后端在 ComfyUI 拒收时返回 200 + {error} */
  if (r?.error) {
    for (const id of ids) setJob(id, { state: 'err', error: r.error });
    toastErr('ComfyUI 拒绝提交', String(r.error).slice(0, 160));
    return { ok: 0, skipped: [], error: r.error };
  }

  let ok = 0;
  const skipped = [];
  for (const one of (r.results || [])) {
    if (one.skipped) {
      skipped.push(one);
      setJob(one.image_id, { state: 'skip', error: one.reason });
      continue;
    }
    ok++;
    pending.set(one.result_id, { imgId: one.image_id, t0: Date.now(), onTick: opt.onTick, onDone: opt.onDone });
    setJob(one.image_id, { state: 'run', error: null, resultId: one.result_id });   // 记下 id，界面上才有"中断这一张"的对象
    done.push(one);
  }
  if (ok && !opt.quiet) toastBusy(`已提交 ${ok} 张`, 'ComfyUI 排队采样中，可继续操作');
  startPolling();
  return { ok, skipped, results: done, error: ok ? undefined : (skipped[0]?.reason || '没有任务被接受（可能遮罩未保存）') };
}

/** 用户中断：从轮询里摘掉，别让下一轮又把状态改回 running */
export function drop(resultId) {
  const had = pending.delete(resultId);
  if (had) notify();
  return had;
}

/**
 * 接回一个仍在跑的库内任务（刷新页面或切走再回来时，内存里的轮询队列已经丢了）
 * @returns {boolean} 是否新接了一个
 */
export function adopt(resultId, imgId, opt = {}) {
  if (!resultId || pending.has(resultId)) return false;
  pending.set(resultId, { imgId, t0: 0, onTick: opt.onTick, onDone: opt.onDone });
  setJob(imgId, { state: 'run', resultId });
  startPolling();
  return true;
}
