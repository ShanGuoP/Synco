// 归一化裁切框计算；frame 为当前图像域尺寸，ratioPick 为锁定的像素宽高比。
import { clamp } from '../../../core/dom.js';
export function relock(c, r, frame) {
  const cx = c[0] + c[2] / 2, cy = c[1] + c[3] / 2;
  let w = c[2], h = (w * frame.w) / (r * frame.h);
  if (h > 1) { h = 1; w = (h * r * frame.h) / frame.w; }
  if (w > 1) { w = 1; h = (w * frame.w) / (r * frame.h); }
  return [clamp(cx - w / 2, 0, 1 - w), clamp(cy - h / 2, 0, 1 - h), w, h];
}
// 连着按同一侧要能转得下去：角度在 (-180,180] 里**绕回**，不是夹住。
// 夹住的话第三下就钉死在 180（服务端那侧也钳在 ±180，绕回正好落在它的取值域里）
export const wrapDeg = a => { const v = ((a % 360) + 540) % 360 - 180; return v === -180 ? 180 : v; };
export function moveRect(b, dir, dx, dy, frame, ratioPick) {
  let [x, y, w, h] = b;
  if (dir === 'move') return [clamp(x + dx, 0, 1 - w), clamp(y + dy, 0, 1 - h), w, h];
  if (dir.includes('w')) { const nx = clamp(x + dx, 0, x + w - 0.02); w += x - nx; x = nx; }
  if (dir.includes('e')) w = clamp(w + dx, 0.02, 1 - x);
  if (dir.includes('n')) { const ny = clamp(y + dy, 0, y + h - 0.02); h += y - ny; y = ny; }
  if (dir.includes('s')) h = clamp(h + dy, 0.02, 1 - y);
  if (ratioPick > 0 && dir !== 'n' && dir !== 's' && dir !== 'e' && dir !== 'w') {
    // 角柄锁比：以横向为准重算纵向，越界就整体缩回画面
    h = (w * frame.w) / (ratioPick * frame.h);
    if (y + h > 1) { h = 1 - y; w = (h * ratioPick * frame.h) / frame.w; }
    if (x + w > 1) { w = 1 - x; h = (w * frame.w) / (ratioPick * frame.h); }
  }
  if (ratioPick > 0 && (dir === 'n' || dir === 's')) { w = (h * ratioPick * frame.h) / frame.w; x = clamp(x + (b[2] - w) / 2, 0, Math.max(0, 1 - w)); }
  if (ratioPick > 0 && (dir === 'e' || dir === 'w')) { h = (w * frame.w) / (ratioPick * frame.h); y = clamp(y + (b[3] - h) / 2, 0, Math.max(0, 1 - h)); }
  return [clamp(x, 0, 1 - w), clamp(y, 0, 1 - h), Math.max(0.02, w), Math.max(0.02, h)];
}
