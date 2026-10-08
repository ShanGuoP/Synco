// 瓦片视图：底图不再是一张全分辨率 canvas，而是一层 proxy 海报 + 可视区的 512 瓦片。
// 浏览器解码位图因此与图片大小解耦（24MP 与 100MP 同开销），而坐标一律仍用原图像素，
// 好让涂抹层、笔刷光标与旧代码的换算口径不变。
'use strict';

/**
 * box 的 CSS 尺寸就是原图像素尺寸（由 viewport.setContentSize 决定），
 * 所以瓦片可以直接按"原图像素"定位，缩放交给父层的 transform。
 */
export function createTileView(box) {
  const live = new Map();          // "z/x/y" -> 已贴上去的 <img>
  let meta = null;                 // 服务端瓦片清单；tile=0 表示这张不需要瓦片
  let cur = null;                  // 当前使用的那一级 { z, k }
  let wanted = new Set();          // 这一轮该显示的键
  let gen = 0;                     // 换图/换清单时自增：在飞的瓦片发现自己过期就不再贴
  let switched = true;             // 下一批贴上来的是"换图后的第一批"：那才按列错峰（F7c）
  let raf = 0;
  let view = null;

  function clear() {
    for (const img of live.values()) img.remove();
    live.clear();
  }

  function setMeta(m) {
    meta = m && m.tile > 0 && m.levels && m.levels.length ? m : null;
    clear();
    wanted = new Set();
    gen++;
    switched = true;
  }

  /** 切图时在飞的请求全部作废：晚到的那一格会盖到另一张照片上 */
  function abort() {
    gen++;
    view = null;
    clear();
    wanted = new Set();
    switched = true;
  }

  /** 取"分辨率够屏幕用、但字节最少"的那一级 */
  function levelFor(scale) {
    const want = Math.max(1, Math.round(meta.w * scale));
    for (const l of meta.levels) if (l.w >= want) return l;
    return meta.levels[meta.levels.length - 1];
  }

  function run() {
    raf = 0;
    const v = view;
    if (!v || !meta) return;
    const l = levelFor(v.s);
    const k = l.w / meta.w;                     // 这一级相对原图的缩小系数
    const t = meta.tile / k;                    // 一格瓦片在原图像素下的边长
    cur = { z: l.z, k, t };
    const px = x => x * meta.tile / k;
    // 行列各多取一圈：平移时先看见上一级的糊图，也比看见一片空白好
    const x0 = Math.max(0, Math.floor((-v.tx / v.s) * k / meta.tile) - 1);
    const y0 = Math.max(0, Math.floor((-v.ty / v.s) * k / meta.tile) - 1);
    const x1 = Math.min(l.cols - 1, Math.ceil(((v.stageW - v.tx) / v.s) * k / meta.tile));
    const y1 = Math.min(l.rows - 1, Math.ceil(((v.stageH - v.ty) / v.s) * k / meta.tile));
    const next = new Set();
    for (let y = Math.max(0, y0); y <= y1; y++) {
      for (let x = Math.max(0, x0); x <= x1; x++) next.add(`${l.z}/${x}/${y}`);
    }
    wanted = next;
    for (const [key, img] of live) {
      if (!next.has(key)) { img.remove(); live.delete(key); }
    }
    if (!next.size) return;
    const g = gen;
    /* F7c：只有"换图后的第一批"按列错峰进场。平移/缩放补进来的那些不错峰——
       它们是要立刻盖住海报那档糊图的，晚一帧都是拖影（淡入本身照旧，见 editor.css） */
    const stagger = switched;
    switched = false;
    for (const key of next) {
      if (live.has(key)) continue;
      const [z, x, y] = key.split('/').map(Number);
      const img = new Image();
      // 边缘那格在原图里占不到一整格（4000 宽的最后一列只有 416），切出来就是窄的：
      // 一律按整格摆会把这条边拉伸近三成并溢出画框，遮罩看着整体错位
      const tw = Math.min(t, meta.w - x * t);
      const th = Math.min(t, meta.h - y * t);
      img.onload = () => {
        // 等到这时候视口可能已经变了：过期或已被别人贴上就不动 DOM
        if (g !== gen || !wanted.has(key) || live.has(key)) return;
        img.style.cssText = `position:absolute;left:${px(x)}px;top:${px(y)}px;width:${tw}px;height:${th}px`;
        // 列号交给 CSS 去乘时长：动效的数都留在样式表里，这里只给"第几列"
        if (stagger) img.style.setProperty('--tile-col', String(x % 4));
        img.alt = '';
        img.draggable = false;
        box.appendChild(img);
        live.set(key, img);
      };
      img.src = meta.url.replace('{z}', z).replace('{x}', x).replace('{y}', y);
    }
  }

  function sync(s, tx, ty, stageW, stageH) {
    view = { s, tx, ty, stageW, stageH };
    if (meta && !raf) raf = requestAnimationFrame(run);
  }

  /** 装载完清单后立刻算一次：换图不该再等一帧才看见像素 */
  function syncNow(s, tx, ty, stageW, stageH) {
    view = { s, tx, ty, stageW, stageH };
    if (raf) { cancelAnimationFrame(raf); raf = 0; }
    run();
  }

  return { setMeta, sync, syncNow, abort, clear, get active() { return !!meta; }, get count() { return live.size; }, level: () => cur };
}
