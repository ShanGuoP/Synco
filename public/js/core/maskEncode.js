// 蒙版导出：优先走 Worker（PNG 编码不占主线程），环境不支持时退回同步 toDataURL
// 涂抹层本来就按 proxy 分辨率画，保存时原样编码；上采样到原图尺寸是服务端的事
'use strict';

import { t } from './i18n.js';

let worker = null;      // null=未初始化, false=不可用
let seq = 0;
const pending = new Map();     // id -> { res, rej }
/* 全分辨率 PNG 编码正常一两秒；20 秒不回话就是 Worker 没了，不能让"保存中…"永远挂着 */
const ENCODE_MS = 20000;

/** Worker 挂了：把手上每个等待都结掉，之后的调用回落同步路径 */
function retireWorker(err) {
  for (const [, p] of pending) p.rej(err);
  pending.clear();
  worker = false;
}

function ensureWorker() {
  if (worker !== null) return worker;
  try {
    if (typeof Worker === 'undefined' || typeof OffscreenCanvas === 'undefined' || typeof createImageBitmap !== 'function') {
      worker = false;
      return worker;
    }
    worker = new Worker('/public/js/core/maskEncode.worker.js');
    worker.onmessage = (e) => {
      const { id } = e.data;
      const p = pending.get(id);
      if (!p) return;
      pending.delete(id);
      if (e.data.error) p.rej(new Error(e.data.error)); else p.res(e.data);
    };
    worker.onerror = ev => retireWorker(new Error(t('maskEncode.workerDead', { msg: (ev && ev.message) || t('maskEncode.unknown') })));
  } catch {
    worker = false;
  }
  return worker;
}

function blobToDataURL(blob) {
  return new Promise((res, rej) => {
    const rd = new FileReader();
    rd.onload = () => res(rd.result);
    rd.onerror = () => rej(new Error(t('maskEncode.fileRead')));
    rd.readAsDataURL(blob);
  });
}

function upscaled(paintCanvas, fullW, fullH) {
  const c = document.createElement('canvas');
  c.width = fullW;
  c.height = fullH;
  const x = c.getContext('2d');
  x.imageSmoothingEnabled = true;
  x.imageSmoothingQuality = 'high';
  x.drawImage(paintCanvas, 0, 0, fullW, fullH);
  return c;
}

/**
 * 把涂抹层导成 PNG dataURL。返回 { b64 }，可能抛错（调用侧决定是否提示）。
 * 给了 fullW/fullH 且与画布不等才上采样——遮罩现在按 proxy 分辨率存，服务端再还原到原图尺寸。
 */
export async function encodeMask(paintCanvas, fullW, fullH) {
  const src = fullW && (fullW !== paintCanvas.width || fullH !== paintCanvas.height)
    ? upscaled(paintCanvas, fullW, fullH) : paintCanvas;
  if (!ensureWorker()) return encodeSync(src, src.width, src.height);
  /* 主线程只做上采样（GPU，极快），编码才是交给 Worker 的重活 */
  let bmp;
  try { bmp = await createImageBitmap(src); }
  catch { return encodeSync(src, src.width, src.height); }          // 连位图都造不出来，Worker 这条路就走不通
  /* 挂号在 postMessage 之前完成、失败即撤，pending 里不会留没人应答的 resolver（原来就是这么漏的） */
  const id = ++seq;
  let r;
  try {
    r = await new Promise((res, rej) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        rej(new Error(t('maskEncode.timeout', { s: ENCODE_MS / 1000 })));
      }, ENCODE_MS);
      pending.set(id, {
        res: v => { clearTimeout(timer); res(v); },
        rej: e => { clearTimeout(timer); rej(e); },
      });
      worker.postMessage({ id, full: bmp }, [bmp]);
    });
  } catch (e) {
    retireWorker(e);                       // 之后每张图都改走主线程，不再挂第二次
    // 用已经算好的 src：这里传 fullW/fullH 的话，调用方没给尺寸（遮罩按 proxy 分辨率原样存，
    // 平时就是 undefined）会退化成 0×0 画布，toDataURL 出 "data:,"，写盘就是三字节垃圾，
    // 用户已有的笔迹被盖掉且不可恢复
    return encodeSync(src, src.width, src.height);
  }
  if (r.error) throw new Error(r.error);   // 编码本身失败：同步路径也一样会失败，如实抛出去
  return { b64: await blobToDataURL(r.blob) };
}

/** 同步兜底：与 worker 同一管线，只是编码回到主线程 */
function encodeSync(paintCanvas, fullW, fullH) {
  return { b64: upscaled(paintCanvas, fullW, fullH).toDataURL('image/png') };
}
