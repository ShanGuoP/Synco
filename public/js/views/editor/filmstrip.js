// 底部胶片条：左批量胶囊 · 中缩略图(序号+状态点+勾选) · 右当前文件信息
'use strict';
import { el, fill } from '../../core/dom.js';
import { icon } from '../../core/icons.js';
import { stateText } from '../../state.js';
import { fmtFile, fmtDims } from '../../core/format.js';
import { t } from '../../core/i18n.js';

const DOT = { run: 'dot--pending', done: 'dot--done', err: 'dot--err', skip: '', nomask: 'dot--mask', ready: '' };

export function createFilmstrip(hooks = {}) {
  const strip = el('div.film-strip', { role: 'listbox', 'aria-label': t('film.aria') });
  const count = el('span.film-count');
  const meta = el('div.film-meta');

  const chip = (ico, label, onclick, id) => el('button.chip', {
    type: 'button', id, 'data-tip': label, onclick,
    html: icon(ico, { cls: 'icon icon--sm' }) + `<span>${label}</span>`,
  });

  const node = el('footer.ed-film', {},
    el('div.film-chips', {},
      el('button.btn.btn--icon.btn--sm', { type: 'button', 'data-tip': t('film.add'), html: icon('plus', { cls: 'icon icon--sm' }), onclick: () => hooks.onImport?.() }),
      chip('check', t('film.masked'), () => hooks.onSelectMasked?.()),
      chip('images', t('film.invert'), () => hooks.onInvert?.()),
      chip('close', t('film.clear'), () => hooks.onClearSel?.()),
      count),
    strip,
    meta,
  );

  /**
   * 每次同步都重建 <img> 的话，浏览器要把每张 4000×6000 的原图重新解码一遍，点缩略图切图就卡在这。
   * 这里按 image id 复用节点：只改类名、标题与状态点，DOM 不重建；
   * src 用的是服务端切好的 320 档（没补出来的存量图才回退原图）。
   */
  const items = new Map();

  function sync({ images, curId, sel, stateOf }) {
    fill(count, t('film.selected', { n: sel.length, total: images.length }));
    const seen = new Set();
    let prev = null;
    images.forEach((img, i) => {
      seen.add(img.id);
      let it = items.get(img.id);
      if (!it || it.dead !== !!img.orig_dead) {
        if (it) it.node.remove();          // 原图从"丢了"变回"在"（重新导入）时旧节点要摘掉，否则留一个空的
        const thumb = img.orig_dead ? el('span.film-it__gone')
                                    : el('img', { src: img.thumb_url || img.orig_url, alt: '', loading: 'lazy', decoding: 'async' });
        const num = el('span.film-it__i');
        const dot = el('span.film-it__d');
        const node = el('button.film-it', {
          type: 'button', role: 'option',
          onclick: e => { e.shiftKey || e.ctrlKey || e.metaKey ? hooks.onToggleSel?.(img.id) : hooks.onPick?.(img.id); },
          oncontextmenu: e => { e.preventDefault(); hooks.onToggleSel?.(img.id); },
        }, thumb, el('span.film-it__pick', { html: icon('check', { cls: 'icon icon--sm' }) }), num, dot);
        it = { node, num, dot, dead: !!img.orig_dead };
        items.set(img.id, it);
      }
      const st = stateOf(img);
      it.node.className = `film-it${img.id === curId ? ' is-on' : ''}${sel.includes(img.id) ? ' is-pick' : ''}`;
      it.node.setAttribute('aria-selected', String(img.id === curId));
      it.node.title = `${img.name} · ${img.orig_dead ? t('film.lostFile') : stateText(st)}`;
      it.num.textContent = String(i + 1);
      it.dot.className = `film-it__d ${DOT[st] || ''}`;
      const want = prev ? prev.nextSibling : strip.firstChild;
      if (want !== it.node) strip.insertBefore(it.node, want);
      prev = it.node;
    });
    for (const [id, it] of items) if (!seen.has(id)) { it.node.remove(); items.delete(id); }
    const cur = images.find(i => i.id === curId);
    fill(meta, cur ? [
      el('b', { text: fmtFile(cur.name, 22) }),
      el('span', { text: fmtDims(cur.w, cur.h) }),
      el('span', { text: stateText(stateOf(cur)) }),
    ] : null);
    const on = strip.querySelector('.film-it.is-on');
    if (on) on.scrollIntoView({ block: 'nearest', inline: 'center' });
  }

  return { node, sync };
}
