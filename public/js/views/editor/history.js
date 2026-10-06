// 左列「结果历史」：数据来自本机库的 results 表
'use strict';
import { el, fill } from '../../core/dom.js';
import { icon } from '../../core/icons.js';
import { fmtStamp, fmtNum } from '../../core/format.js';

const settingsOf = r => { try { return JSON.parse(r.settings_json || 'null'); } catch { return null; } };

export function createHistory({ onPick, onRestore, onFork, onDel }) {
  let mode = 'all';
  let list = [];
  let active = null;

  const listBox = el('div.pre-list');
  const seg = el('div.seg', {},
    el('button.seg__it', { type: 'button', class: 'seg__it is-on', dataset: { k: 'all' }, text: '全部', onclick: () => setMode('all') }),
    el('button.seg__it', { type: 'button', dataset: { k: 'ok' }, text: '已出图', onclick: () => setMode('ok') }),
  );

  function setMode(k) {
    mode = k;
    for (const b of seg.children) b.classList.toggle('is-on', b.dataset.k === k);
    paint();
  }

  function paint() {
    const rows = list.filter(r => (mode === 'ok' ? r.status === 'done' && !r.final_dead : true));
    if (!rows.length) {
      fill(listBox, el('p', { class: 'muted', style: { padding: '14px 6px', lineHeight: '1.7' },
        text: list.length ? '这个筛选下没有记录。' : '这张图还没有提交过生成。涂好遮罩后点下方「提交生成」。' }));
      return;
    }
    fill(listBox, rows.map(r => {
      const s = settingsOf(r);
      const nl = s && s.loras ? s.loras.filter(x => x.enabled).length : null;
      /* 云端记录别拿步数/CFG 去解释 */
      const c = s && s.cloud;
      const body = c ? [c.model, c.quality].filter(Boolean).join(' · ') || '云端'
                     : `${r.steps}步 CFG${fmtNum(r.cfg, 0.5)}${nl != null ? ` · LoRA ${nl}` : ''}`;
      const meta = `${fmtStamp(r.created_at).slice(5)} · ${body}`;
      /* 状态是 done 但 PNG 不在盘上（清过 data/projects、手工删过文件）：缩略图与对比都没有左边可画，
         这里按"文件丢失"渲染，比摆一张碎图标诚实 */
      /* 优先服务端切好的 320 成图缩略；没有才回落到裁切图/成图（老记录与成图丢失都走这条） */
      const gone = !!r.final_dead;
      const thumb = gone ? null : (r.thumb_url || (r.crop_url && !r.crop_dead ? r.crop_url : r.final_url));
      return el('div.pre-it', {
        class: `pre-it${active === r.id ? ' is-on' : ''}`,
        role: 'button', tabindex: '0', dataset: { rid: String(r.id) },
        onclick: () => pick(r), onkeydown: e => { if (e.key === 'Enter') pick(r); },
      },
        thumb ? el('img.pre-it__th', { src: thumb, alt: '', loading: 'lazy' })
              : el('span.pre-it__ph', { class: `pre-it__ph${gone ? ' pre-it__ph--gone' : ''}` }),
        el('span.pre-it__tx', {},
          el('b', { text: `#${r.id} · ${gone ? '文件已丢失' : r.status === 'done' ? '已出图' : r.status === 'error' ? '失败' : '生成中'}` }),
          el('span.pre-it__meta', { text: meta, title: meta }),
          /* 失败原因原来只在右下角 toast 里活 5 秒，回头看这条就什么都没有了 */
          r.status === 'error' && r.error && !gone ? el('span.pre-it__err', { text: String(r.error).slice(0, 160), title: String(r.error || '') }) : null,
          r.rerun_of ? el('span.pre-it__from', { text: `改自 #${r.rerun_of}` }) : null),
        /* 这一列在 .pre-list（overflow 滚动容器）里，自绘气泡会被裁掉一截：
           实测 174px 宽的提示只有 33px 落在面板内，剩下的被切了。改用原生 title，浏览器画的浮层不受我们布局的裁剪。 */
        el('span.pre-it__acts', {},
          el('button.pre-it__btn', {
            type: 'button', title: s ? '回填这组参数，改完自己点提交' : '这条没存参数', disabled: !s,
            html: icon('sliders', { cls: 'icon icon--sm' }),
            'aria-label': '回填这组参数',
            onclick: e => { e.stopPropagation(); onRestore?.(r); },
          }),
          !gone && r.final_url && onFork ? el('button.pre-it__btn', {
            type: 'button', title: '把这张成图另存为项目里的新图，之后在它上面涂',
            html: icon('copy', { cls: 'icon icon--sm' }),
            'aria-label': '另存为新图',
            onclick: e => { e.stopPropagation(); onFork(r); },
          }) : null,
          onDel ? el('button.pre-it__btn', {
            type: 'button', title: gone ? '删掉这条记录（文件已经不在了，只清库里这一行）'
              : r.status === 'running' ? '还在生成中，等它落定再删' : '删掉这条记录与它的成图，原图和遮罩不动',
            disabled: r.status === 'running',
            html: icon('trash', { cls: 'icon icon--sm' }),
            'aria-label': '删除这条记录',
            onclick: e => { e.stopPropagation(); if (r.status !== 'running') onDel(r); },
          }) : null),
        el('span.dot', { class: `dot ${gone ? 'dot--err' : r.status === 'done' ? 'dot--done' : r.status === 'error' ? 'dot--err' : 'dot--pending'}` }),
      );
    }));
  }

  function pick(r) {
    active = r.id;
    for (const n of listBox.children) n.classList.toggle('is-on', +n.dataset.rid === r.id);
    onPick?.(r);
  }

  const node = el('aside.ed-pre', {},
    el('div.col-hd', {}, el('h3', { html: icon('layers', { cls: 'icon icon--sm' }) + '<span>结果历史</span>' })),
    el('div.col-sub', {}, seg),
    listBox,
  );

  return {
    node,
    setResults(rows, activeId) { list = rows || []; active = activeId ?? null; paint(); },
    count: () => list.length,
    doneCount: () => list.filter(r => r.status === 'done').length,
  };
}
