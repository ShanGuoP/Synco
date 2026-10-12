// 裁切框渲染与指针草稿；拖拽会话归组件，参数提交仍由调整入口负责。
import { moveRect } from './cropMath.js';

export function createCropOverlay({ cropBox, layer, viewport, modeOf, frameOf, ratioOf, boxOf, toFrame, onDraft, onEnd }) {
  function paint() {
    // 没拖过也要给一个能抓的框：进裁切模式时参数里的 crop 还是 null，
    // 那时候既不写尺寸也不写不透明度，屏幕上等于摆了一个"看不见的零尺寸框"——用户压根没有框可拖
    const c = boxOf();
    cropBox.style.opacity = c ? '1' : '0';
    if (!c) return;
    cropBox.style.left = `${c[0] * 100}%`;
    cropBox.style.top = `${c[1] * 100}%`;
    cropBox.style.width = `${c[2] * 100}%`;
    cropBox.style.height = `${c[3] * 100}%`;
  }

  let drag = null;
  cropBox.addEventListener('pointerdown', ev => {
    // 中键留给平移：它在 stage 上有处理器，这里早退让事件冒泡上去就行
    if (modeOf() !== 'crop' || viewport.mode === 'pan' || (ev.pointerType === 'mouse' && ev.button !== 0)) return;
    ev.preventDefault();
    ev.stopPropagation();
    drag = {
      dir: ev.target.dataset?.d || 'move',
      rect: layer.getBoundingClientRect(),
      start: toFrame(ev, layer.getBoundingClientRect()),
      base: [...(boxOf() || [0, 0, 1, 1])],
    };
    try { cropBox.setPointerCapture?.(ev.pointerId); } catch { /* 合成事件或已被捕获：指针捕获只是优化，不是必需 */ }
  });
  cropBox.addEventListener('pointermove', ev => {
    if (!drag) return;
    const p = toFrame(ev, drag.rect);
    const frame = frameOf();
    onDraft(moveRect(drag.base, drag.dir, (p.x - drag.start.x) / frame.w, (p.y - drag.start.y) / frame.h, frame, ratioOf()));
    paint();
  });
  const endCrop = () => {
    if (!drag) return;
    drag = null;
    // 松手只把框留在屏幕上：不写参数、不渲预览——那是「✔ 裁这一版」的活
    onEnd();
  };
  cropBox.addEventListener('pointerup', endCrop);
  cropBox.addEventListener('pointercancel', endCrop);

  return { paint, reset() { drag = null; } };
}
