// 生成任务层：提交 / 轮询 / 状态回写，项目页批量与编辑器单张共用
'use strict';
import { api, ApiError } from './core/api.js';
import { store, setJob, touchImage, saveSettings } from './state.js';
import { toast, toastErr, toastOk, toastBusy } from './ui/toast.js';

/** resultId -> { imgId, t0, onDone } */
const pending = new Map();
const listeners = new Set();
let next = 0;                         // 下一轮的 setTimeout 句柄（0 = 没排）
let running = false;                  // 上一轮还在飞：这时不能再排第二次，也别再插队

export const pendingCount = () => pending.size;
export function onTick(fn) { listeners.add(fn); return () => listeners.delete(fn); }
const emit = () => { for (const fn of [...listeners]) { try { fn(pendingCount()); } catch (e) { console.error(e); } } };

function notify() {
  if (!pending.size) { clearTimeout(next); next = 0; }
  emit();
}

async function tick() {
  if (!pending.size) return notify();
  for (const [rid, job] of [...pending]) {
    let r;
    try { r = await api.result(rid); }
    catch (e) {
      /* 404 = 这一行已经不在了（被删、或从来没建起来）。继续敲下去只会一直敲到刷新页面为止，
         而角标会永远停在"生成中"——那是撒谎，不是等待。 */
      if (e instanceof ApiError && e.status === 404) {
        pending.delete(rid);
        setJob(job.imgId, { state: 'idle', resultId: null });
        continue;
      }
      // 其余错误按网络抖动处理，但也要有尽头：服务换了端口、后端整个不在时失败是永远不会成功的
      if (++job.fails >= 8) {
        pending.delete(rid);
        setJob(job.imgId, { state: 'err', resultId: rid, error: '读不到这条任务的状态，已停止轮询' });
        toastErr('轮询已停止', `#${rid}：连续 8 次读不到状态（服务可能已经重启）`);
      }
      continue;
    }
    job.fails = 0;
    // 服务端队列里的 queued 与 running 都还没落定，都算"在跑"
    if (r.status === 'running' || r.status === 'queued') { job.onTick?.(r.status, r); continue; }
    pending.delete(rid);
    job.onTick?.(r.status, r);
    if (r.status === 'done') {
      setJob(job.imgId, { state: 'done', resultId: rid, error: null });
      const cur = store.peek('images').find(i => i.id === job.imgId);
      // 角标读的是 result_count / result_done：本次会话跑完的那张要就地加一，等下次进项目页自然被库里的数覆盖
      touchImage(job.imgId, {
        result_count: (cur?.result_count ?? 0) + 1,
        result_done: (cur?.result_done ?? 0) + 1,
        latest_result_url: r.thumb_url || r.final_url || cur?.latest_result_url || null,
        last_result: r,
      });
      toastOk('生成完成', job.t0 ? `#${rid} · 用时 ${Math.round((Date.now() - job.t0) / 1000)} 秒` : `#${rid}`);
    } else {
      setJob(job.imgId, { state: 'err', resultId: rid, error: r.error || 'ComfyUI 未返回原因' });
      toastErr('生成失败', String(r.error || '').slice(0, 120));
    }
    job.onDone?.(r);
  }
  notify();
}

/* 上一轮跑完才排下一轮。固定 setInterval 撞慢网络时会让同一个 result id 被并发查两次
   （一次请求超过 2.5 秒，下一轮已经带着同一批 id 起飞），终态于是可能落两遍：
   重复 toast、重复写回角标、onDone 里那次入库或删除跟着跑第二次。 */
async function loop() {
  running = true;
  try { await tick(); } finally { running = false; }
  if (pending.size) next = setTimeout(loop, 2500);
  else notify();
}
function startPolling() {
  if (running) return;                 // 正在飞的那一轮带着同一个 Map，跑完自然会看到新排进来的
  clearTimeout(next); next = 0;
  loop();
}

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
    pending.set(one.result_id, { imgId: one.image_id, t0: Date.now(), fails: 0, onTick: opt.onTick, onDone: opt.onDone });
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
  pending.set(resultId, { imgId, t0: 0, fails: 0, onTick: opt.onTick, onDone: opt.onDone });
  setJob(imgId, { state: 'run', resultId });
  startPolling();
  return true;
}
