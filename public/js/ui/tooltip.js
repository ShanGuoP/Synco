// 悬停提示：body 上的一层浮标，取代画在元素自己身上的 CSS 伪元素。
//
// 为什么非得搬出去：属性面板、窄屏抽屉、对话框都在带 overflow 的滚动容器里，
// ::after 属于容器内容，容器一裁就切掉半截——按钮越靠边切得越狠
// （旋转键那句「左转 90°（无损，不插值）」就是这么被 .prop-body 吃掉的）。
// 挂在 body 上不受任何祖先 overflow 影响，出界了还会自己翻到另一侧。
'use strict';

const GAP = 8;            // 浮标与锚点之间的缝
const EDGE = 8;           // 离视口边留多少
let node = null;
let host = null;          // 当前显示的是哪个元素：换元素与移出都拿它比

function layer() {
  if (!node) {
    node = document.createElement('div');
    node.className = 'tip';
    node.setAttribute('role', 'tooltip');
    node.hidden = true;
    document.body.appendChild(node);
  }
  return node;
}

function hide() {
  host = null;
  if (node) node.hidden = true;
}

/** 默认浮在上方；那一侧放不下就翻到对面；四个方向都夹回视口内 */
function show(el) {
  const text = el.dataset.tip;
  if (!text) return hide();
  const n = layer();
  if (host === el && !n.hidden) return;
  host = el;
  n.textContent = text;      // 只走 textContent：提示里出现尖括号也不当结构解析
  n.hidden = false;
  const r = el.getBoundingClientRect();
  const b = n.getBoundingClientRect();
  const vw = innerWidth, vh = innerHeight;
  const side = el.dataset.tipSide || 'top';
  let x, y;
  if (side === 'right' || side === 'left') {
    const right = side === 'right';
    x = right ? r.right + GAP : r.left - GAP - b.width;
    if (right && x + b.width > vw - EDGE) x = r.left - GAP - b.width;
    if (!right && x < EDGE) x = r.right + GAP;
    y = r.top + r.height / 2 - b.height / 2;
  } else {
    const below = side === 'below';
    y = below ? r.bottom + GAP : r.top - GAP - b.height;
    if (!below && y < EDGE) y = r.bottom + GAP;
    if (below && y + b.height > vh - EDGE) y = r.top - GAP - b.height;
    x = r.left + r.width / 2 - b.width / 2;
  }
  n.style.left = `${Math.round(Math.min(Math.max(x, EDGE), Math.max(EDGE, vw - b.width - EDGE)))}px`;
  n.style.top = `${Math.round(Math.min(Math.max(y, EDGE), Math.max(EDGE, vh - b.height - EDGE)))}px`;
}

/** 全局挂一次。事件委托：面板每次重建 DOM，按钮不可能各自接线 */
export function mountTooltip() {
  const anchor = t => (typeof t?.closest === 'function' ? t.closest('[data-tip]') : null);
  document.addEventListener('pointerover', e => { const el = anchor(e.target); if (el) show(el); });
  document.addEventListener('pointerout', e => { if (anchor(e.target) === host) hide(); });
  document.addEventListener('pointerdown', hide, true);
  document.addEventListener('focusin', e => { const el = anchor(e.target); if (el) show(el); });
  document.addEventListener('focusout', hide);
  document.addEventListener('keydown', e => { if (e.key === 'Escape') hide(); });
  // 滚动与缩放都会让锚点跑掉：直接收起，别留一个飘在原地的浮标
  window.addEventListener('scroll', hide, true);
  window.addEventListener('resize', hide);
}
