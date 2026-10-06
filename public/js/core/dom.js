// DOM 工具层：所有视图共用的最小原语，不含业务
'use strict';

export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

/**
 * 建元素。tag 支持 'div.card#id' 简写。
 * props 里 on* 走 addEventListener，dataset/style 展开，其余 setAttribute。
 * 文本一律走 text:（自动转义）；只有拼接自项目内常量的 HTML 才允许 html:。
 */
export function el(tag, props = null, ...kids) {
  const [name, ...rest] = String(tag).split(/(?=[.#])/);
  const node = document.createElement(name || 'div');
  for (const r of rest) {
    if (r[0] === '.') node.classList.add(r.slice(1));
    else if (r[0] === '#') node.id = r.slice(1);
  }
  if (props && (props.nodeType || typeof props === 'string' || Array.isArray(props))) {
    kids.unshift(props); props = null;
  }
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null || v === false) continue;
    if (k === 'class') node.className = v;
    else if (k === 'html') node.innerHTML = v;
    else if (k === 'text') node.textContent = v;
    else if (k === 'style') {
      for (const [prop, val] of Object.entries(v)) {
        if (prop.startsWith('--')) node.style.setProperty(prop, val);
        else node.style[prop] = val;
      }
    }
    else if (k === 'dataset') Object.assign(node.dataset, v);
    else if (k.startsWith('on') && typeof v === 'function') node.addEventListener(k.slice(2), v);
    else if (k in node && k !== 'list') node[k] = v;
    else node.setAttribute(k, v);
  }
  add(node, kids);
  return node;
}

function add(node, kids) {
  for (const k of kids.flat(4)) {
    if (k == null || k === false) continue;
    node.append(k.nodeType ? k : document.createTextNode(String(k)));
  }
}

/** 替换容器内容（只接受节点，杜绝把未转义字符串当 HTML 塞进去） */
export function fill(node, ...kids) {
  node.replaceChildren(...kids.flat(8).filter(k => k != null && k !== false));
  return node;
}

/** 合并到下一帧执行，避免高频事件里重复布局 */
export function raf(fn) {
  let queued = false, last;
  return (...args) => {
    last = args;
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => { queued = false; fn(...last); });
  };
}

/** 数值夹取 */
export const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
