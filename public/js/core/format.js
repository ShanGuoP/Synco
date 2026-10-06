// 格式化层：日期 / 路径 / 尺寸 / 耗时，视图只调用不自己拼
'use strict';

/** 'YYYY-MM-DD HH:MM:SS' → '2026.9.25 18:51' */
export function fmtStamp(s) {
  if (!s) return '—';
  const m = /^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2})/.exec(String(s));
  if (!m) return String(s);
  return `${+m[1]}.${+m[2]}.${+m[3]} ${m[4]}:${m[5]}`;
}

/** 秒 → '1 分 12 秒' */
export function fmtElapsed(sec) {
  const s = Math.max(0, Math.round(sec));
  if (s < 60) return `${s} 秒`;
  return `${Math.floor(s / 60)} 分 ${String(s % 60).padStart(2, '0')} 秒`;
}

/** Windows/Unix 路径都取末段 */
export const baseName = p => String(p || '').split(/[\\/]/).pop();

export const fmtDims = (w, h) => (w && h ? `${w}×${h}` : '—');

/** 长文件名中段省略，保留扩展名 */
export function fmtFile(name, max = 26) {
  const n = baseName(name);
  if (n.length <= max) return n;
  const ext = n.slice(n.lastIndexOf('.'));
  return `${n.slice(0, max - ext.length - 3)}…${ext}`;
}

/** 滑杆/直输共用的显示精度：整数不带小数点 */
export function fmtNum(v, step) {
  const n = Number(v);
  if (!Number.isFinite(n)) return '0';
  const dec = step >= 1 ? 0 : String(step).split('.')[1]?.length || 2;
  return n.toFixed(dec);
}
