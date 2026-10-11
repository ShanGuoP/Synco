// 遮罩绘制层：画笔/橡皮笔画、撤销栈、笔刷光标、防抖自动保存
// 只跟 mask canvas 打交道；视口缩放/平移由 viewport.js 负责
// 光标定位走 transform + 帧同步、PNG 编码走 Worker：主线程在 ComfyUI 抢 GPU 时也不掉帧
'use strict';
import { encodeMask } from '../../core/maskEncode.js';

const UNDO_MAX = 8;

/**
 * @param {{mask:HTMLCanvasElement, cursor?:HTMLElement, viewport?:object,
 *   onSaved?:Function, onDirty?:Function, ink?:string}} opt
 *   ink 是描边色：遮罩用白墨（服务端按 alpha 取反），画布用深色墨（拍到白底上就是线稿）
 */
export function createPainter({ mask, cursor, viewport, onSaved, onDirty, ink = '#fff' }) {
  let tool = 'brush';
  let brush = 70;
  let strokeColor = ink;
  let painting = false;
  let undoStack = [];
  let saveTimer = 0;
  let failStreak = 0;                // 连败几次：超过上限就不再自己重试
  let inflight = null;                // 正在飞的保存（含排队的那个），换图前必须等它落定
  let dirtyAt = 0, savedAt = 0;       // 落笔序号 / 已落盘序号：两者不等就有笔迹还没进磁盘
  let owner = 0;                      // 这批笔迹属于哪张图：落笔时定，异步段不再读外部上下文
  let loadSeq = 0;                    // 装载世代：连点两张图时迟到的 onload 要能认出自己过期
  let iw = 0, ih = 0, k = 1;          // 原图尺寸 / 涂抹层缩放系数
  let rect = null;                    // 落笔时取一次，整笔复用，省掉每次事件的强制布局
  let hostRect = null;                // 光标定位宿主 rect，与 rect 同批缓存
  let cursorSize = 0;                 // 光标直径（CSS px），只在换笔刷/换图/缩放时重算
  let appliedCursorSize = 0;
  let pendingCursor = null;           // 待本帧 flush 的光标位置（move 事件只记坐标不碰样式）
  let cursorRaf = 0;
  let queue = [];                     // 本帧待画的点（涂抹层像素坐标）
  let rafId = 0;
  const undoFree = [];                // 撤销画布回收池：超出深度的快照复用，避免每笔分配
  /* 落过笔的范围（涂抹层像素坐标）；baseline=载入时铺过已有遮罩，覆盖范围未知 */
  let bbox = null;
  let baseline = false;

  /* 不加 willReadFrequently：那个标记会把画布后端从 GPU 拽回 CPU，逐笔填充反而更慢 */
  const ctx = mask.getContext('2d');

  const toImage = (cx, cy) => ({
    x: (cx - rect.left) * mask.width / Math.max(1, rect.width),
    y: (cy - rect.top) * mask.height / Math.max(1, rect.height),
  });

  const measure = () => {
    rect = mask.getBoundingClientRect();
    if (cursor) {
      hostRect = (cursor.offsetParent || cursor.parentElement).getBoundingClientRect();
      cursorSize = Math.max(4, brush * (rect.width / Math.max(1, iw)));
    }
  };

  /* 光标直径：单独抽出来，因为调笔刷要**立刻**改，不能等下一次 rAF 定位时顺手改
     （隐藏面板里 rAF 不跑是一回事，更主要的是拖动滑块时指针根本不在画布上） */
  function applyCursorSize() {
    if (!cursor || !(cursorSize > 0) || cursorSize === appliedCursorSize) return;
    appliedCursorSize = cursorSize;
    cursor.style.width = cursor.style.height = `${cursorSize}px`;
  }

  /* 光标只改 transform（合成器直接位移，不触发重排），写入合帧到 rAF */
  function flushCursor() {
    cursorRaf = 0;
    applyCursorSize();
    if (!cursor || !pendingCursor || !hostRect) return;
    cursor.style.transform =
      `translate(${pendingCursor.x - hostRect.left}px, ${pendingCursor.y - hostRect.top}px) translate(-50%, -50%)`;
    pendingCursor = null;
    if (cursor.style.display !== 'block') cursor.style.display = 'block';
  }
  const scheduleCursor = () => { if (!cursorRaf) cursorRaf = requestAnimationFrame(flushCursor); };

  function strokeTo(pts) {
    ctx.globalCompositeOperation = tool === 'erase' ? 'destination-out' : 'source-over';
    ctx.strokeStyle = strokeColor;
    const lw = Math.max(1, brush * k);
    ctx.lineWidth = lw;
    ctx.lineCap = ctx.lineJoin = 'round';
    /* 记下这一笔碰到的范围，判空只读这一块 */
    const r = lw / 2 + 1;
    for (const p of pts) {
      if (!bbox) bbox = { x0: p.x - r, y0: p.y - r, x1: p.x + r, y1: p.y + r };
      else {
        bbox.x0 = Math.min(bbox.x0, p.x - r); bbox.y0 = Math.min(bbox.y0, p.y - r);
        bbox.x1 = Math.max(bbox.x1, p.x + r); bbox.y1 = Math.max(bbox.y1, p.y + r);
      }
    }
    ctx.beginPath();
    ctx.moveTo(pts[0].x, pts[0].y);
    /* 单点也要落一枚圆点；零长度路径不会描出线帽 */
    if (pts.length === 1) ctx.lineTo(pts[0].x + .01, pts[0].y + .01);
    else for (let i = 1; i < pts.length; i++) ctx.lineTo(pts[i].x, pts[i].y);
    ctx.stroke();
    ctx.globalCompositeOperation = 'source-over';
  }

  /* 一帧只描一条折线：pointermove 可以到 120Hz，逐事件画就是成倍浪费 */
  function pump() {
    rafId = 0;
    if (queue.length) {
      strokeTo(queue);
      queue = [queue[queue.length - 1]];   // 留一个接点，避免分段之间露缝
      markDirty();
    }
    if (painting) rafId = requestAnimationFrame(pump);
  }
  const schedule = () => { if (!rafId) rafId = requestAnimationFrame(pump); };

  function snapshot() {
    /* 画布从回收池取：撤销栈溢出时 shift 出来的旧快照直接改当新快照，省掉每笔的分配 */
    const c = undoFree.pop() || document.createElement('canvas');
    if (c.width !== mask.width || c.height !== mask.height) { c.width = mask.width; c.height = mask.height; }
    const x = c.getContext('2d');
    x.clearRect(0, 0, c.width, c.height);
    x.drawImage(mask, 0, 0);
    return c;
  }

  function moveCursor(e) {
    if (!cursor || e.pointerType === 'touch') return;
    if (!rect) measure();
    pendingCursor = { x: e.clientX, y: e.clientY };
    scheduleCursor();
  }

  function onDown(e) {
    if (e.button !== 0 && e.pointerType === 'mouse') return;
    if (mask.parentElement.classList.contains('pan-through')) return;
    try { mask.setPointerCapture(e.pointerId); } catch { /* 合成事件或已捕获，不影响涂抹 */ }
    measure();
    undoStack.push(snapshot());
    if (undoStack.length > UNDO_MAX) undoFree.push(undoStack.shift());
    queue = [toImage(e.clientX, e.clientY)];
    painting = true;
    schedule();
    moveCursor(e);
  }
  function onMove(e) {
    moveCursor(e);
    if (!painting) return;
    /* 合并事件把丢掉的中间点补回来，快速甩笔才不会断成多段 */
    const evs = e.getCoalescedEvents ? (e.getCoalescedEvents().length ? e.getCoalescedEvents() : [e]) : [e];
    for (const ev of evs) queue.push(toImage(ev.clientX, ev.clientY));
  }
  function onUp() {
    if (!painting) return;
    painting = false;
    if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
    if (queue.length) {
      strokeTo(queue);
      queue = [];
      markDirty();   // 落笔到抬笔之间一帧都没跑过时，pump 没机会标脏，这里必须补上
    }
    rect = null;
    queueSave();
  }

  mask.addEventListener('pointerdown', onDown);
  mask.addEventListener('pointermove', onMove);
  mask.addEventListener('pointerup', onUp);
  mask.addEventListener('pointercancel', onUp);
  /* 捕获失败或中途丢捕获时（画笔甩出画布、系统弹层抢走指针），松手事件不会落在涂抹层上：
     painting 会一直为真，pump 每帧空转 rAF，下一笔的撤销快照也跟着错位。窗口级兜一层。 */
  window.addEventListener('pointerup', onUp);
  window.addEventListener('pointercancel', onUp);
  mask.addEventListener('lostpointercapture', onUp);
  mask.addEventListener('pointerleave', () => { if (cursor) cursor.style.display = 'none'; });
  mask.addEventListener('pointerenter', e => moveCursor(e));
  if (cursor) { cursor.style.left = '0px'; cursor.style.top = '0px'; }   // 定位交给 transform

  /* 判空必须按真实分辨率：降采样会把一笔几个像素的墨摊薄到阈值以下，
     涂一颗痣保存后就会被当成空遮罩删掉。橡皮也算落笔，所以擦干净了照样判空。 */
  function alphaEmpty(x, y, w, h) {
    if (!(w > 0) || !(h > 0)) return true;
    const d = ctx.getImageData(x, y, w, h).data;
    for (let i = 3; i < d.length; i += 4) if (d[i] > 8) return false;
    return true;
  }

  function isEmpty() {
    if (!bbox) return !baseline;
    if (baseline) return alphaEmpty(0, 0, mask.width, mask.height);
    const x = Math.max(0, Math.floor(bbox.x0));
    const y = Math.max(0, Math.floor(bbox.y0));
    return alphaEmpty(x, y, Math.min(mask.width, Math.ceil(bbox.x1)) - x, Math.min(mask.height, Math.ceil(bbox.y1)) - y);
  }

  function markDirty() { if (dirtyAt === savedAt) onDirty?.(true); dirtyAt++; failStreak = 0; }

  function queueSave() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(save, 700);
  }

  /* 保存失败的自动重试要有上限：这一张被服务端拒了（尺寸不合法、库里没这行）或者进程没起来时，
     固定 700ms 的无限重试就是无限次重编码整张 PNG + 无限次 toast。
     笔迹仍然挂着脏：换图、提交、关页那几次显式 flush，以及下一笔新落笔，都还会再试。 */
  const RETRY_MAX = 3;
  function retryLater() {
    failStreak++;
    if (failStreak > RETRY_MAX) return false;
    clearTimeout(saveTimer);
    saveTimer = setTimeout(save, 700 * 2 ** (failStreak - 1));   // 700ms → 1.4s → 2.8s
    return false;
  }

  /**
   * 一次只让一个保存在飞：并发两次 POST 谁先落地没人保证，遮罩会写反。
   * 用「落笔序号 / 落盘序号」两个计数而不是一个 dirty 布尔位：编码与上传在飞的那几毫秒里落的笔
   * 必须还挂着脏，把复位写成 await 之后一句 `dirty = false` 正好会把这批新笔迹误标成已保存。
   */
  function save() {
    clearTimeout(saveTimer);
    const run = (inflight || Promise.resolve()).then(async () => {
      if (dirtyAt === savedAt) return true;      // 没有新笔迹，本来就不用存
      const upto = dirtyAt;                      // 这批存到第几笔为止
      const empty = isEmpty();
      const id = owner;
      let b64 = null;
      if (!empty) {
        // 涂抹层原样落盘：上采样到原图尺寸是服务端的事（imagesvc::mask_to_orig）
        try { b64 = (await encodeMask(mask)).b64; }
        catch (e) { retryLater(); throw e; }
      }
      /* 钩子返 false = 这次没落住：不推进落盘序号，下一次 flush/换图/定时器都会带上这批笔迹 */
      if (await onSaved?.({ empty, b64, id }) === false) return retryLater();
      savedAt = upto;
      failStreak = 0;
      return true;
    });
    inflight = run.catch(() => false);
    return run;
  }

  return {
    get tool() { return tool; },
    get brush() { return brush; },
    setTool(t) { tool = t; if (cursor) cursor.classList.toggle('is-erase', t === 'erase'); },
    /** 调笔刷要当场改光标直径：rect 是缓存的，move 里那句 measure() 只在落笔或换图后才跑，
        不然"把笔刷拖大"屏幕上那个圈纹丝不动，要等下一次落笔才对得上 */
    setBrush(n) {
      brush = Math.max(2, n);
      if (cursor && rect && iw) cursorSize = Math.max(4, brush * (rect.width / Math.max(1, iw)));
      applyCursorSize();
    },

    /** 载入图片：按给定的涂抹层分辨率重设画布，清撤销栈，可选铺上已有遮罩。id 是这批笔迹的归属 */
    async load(w, h, maskUrl, id, paint, current = () => true) {
      const seq = ++loadSeq;
      /* 先把上一张还没存出去的笔迹落盘再动这块画布：直接 resize 会把没保存的墨抹掉，
         而防抖到点那次 save 见没有新笔迹直接 return，用户视角就是"涂完切图，回来遮罩空了" */
      clearTimeout(saveTimer);
      if (dirtyAt !== savedAt || inflight) { try { await save(); } catch { /* 存不上也别挡住换图，钩子里已经报过 */ } }
      if (seq !== loadSeq || !current()) return false;   // 保存期间可能换图或关闭视图
      iw = w; ih = h;
      owner = id || 0;
      // 涂抹层的档位由调用方定（修图页见 PAINT_EDGE），保存时原样落盘，上采样交服务端
      k = Math.max(1e-6, (paint?.w || w) / Math.max(1, w));
      mask.width = Math.max(1, Math.round(paint?.w || w));
      mask.height = Math.max(1, Math.round(paint?.h || h));
      undoStack = [];
      dirtyAt = savedAt = 0;            // 新画布：落笔与落盘同时归零
      inflight = null;
      queue = [];
      painting = false;
      rect = null;                    // 换图后画布位置变了，旧 rect 不能用
      bbox = null; baseline = false;
      ctx.clearRect(0, 0, mask.width, mask.height);
      if (!maskUrl) return false;
      let drew = false;
      await new Promise(res => {
        const im = new Image();
        im.onload = () => {
          /* 迟到的 onload 不能往已经换掉的画布上画：那会把上一张的遮罩铺进当前这张，
             接着任意一笔落盘就是"B 图上带着 A 图的笔迹"——外层 showSeq 判在 load 返回之后，撤不回这一步 */
          if (seq === loadSeq && current()) { ctx.drawImage(im, 0, 0, mask.width, mask.height); drew = true; }
          res(true);
        };
        im.onerror = () => res(false);
        // 遮罩是原地覆写的文件，服务端对它发 ETag + no-cache：判新交给条件请求，不再自己拼 ?t=
        im.src = maskUrl;
      });
      if (seq !== loadSeq || !current()) return false;
      baseline = drew;                // 已有遮罩的覆盖范围未知，之后判空读整幅
      return drew;                    // 落盘的遮罩一定是非空的，不必为此整幅回读
    },

    undo() {
      const prev = undoStack.pop();
      if (!prev) return Promise.resolve(false);
      ctx.clearRect(0, 0, mask.width, mask.height);
      ctx.drawImage(prev, 0, 0);
      bbox = null; baseline = true;    // 回滚到快照后覆盖范围同样未知
      markDirty();
      queueSave();
      return Promise.resolve(true);
    },
    get canUndo() { return undoStack.length > 0; },

    clear() {
      undoStack.push(snapshot());
      if (undoStack.length > UNDO_MAX) undoFree.push(undoStack.shift());
      ctx.clearRect(0, 0, mask.width, mask.height);
      bbox = null; baseline = false;
      markDirty();
      save();
    },

    /** 视口缩放/平移后调用：光标尺寸与定位缓存全部失效，下次 move 重新量 */
    invalidate() { rect = null; hostRect = null; cursorSize = 0; },
    flush: save,
    detach() {
      clearTimeout(saveTimer);
      if (rafId) cancelAnimationFrame(rafId);
      if (cursorRaf) cancelAnimationFrame(cursorRaf);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
    },
  };
}
