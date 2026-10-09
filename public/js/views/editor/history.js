// 左列「结果历史」：数据来自本机库的 results 表
'use strict';
import { el, fill } from '../../core/dom.js';
import { icon } from '../../core/icons.js';
import { fmtStamp, fmtNum } from '../../core/format.js';
import { t } from '../../core/i18n.js';

const settingsOf = r => { try { return JSON.parse(r.settings_json || 'null'); } catch { return null; } };

/**
 * 结果历史列表。编辑器左栏与项目页的「派生查看」弹窗共用这一个组件：
 * 数据由外面喂进来（setResults），回调不给就不画那个动作，所以弹窗里只有看/删。
 * @param {{onPick?:Function, onRestore?:Function, onFork?:Function, onDel?:Function, bare?:boolean, emptyHint?:string}} opt
 */
export function createHistory({ onPick, onRestore, onFork, onDel, bare = false, emptyHint }) {
  /* 空态文案默认说编辑器的话；画布那条线没有遮罩和「提交生成」，进来时自己给一句 */
  const emptyText = emptyHint || t('hist.empty');
  let mode = 'all';
  let list = [];
  let active = null;

  const listBox = el('div.pre-list');
  const seg = el('div.seg', {},
    el('button.seg__it', { type: 'button', class: 'seg__it is-on', dataset: { k: 'all' }, text: t('hist.all'), onclick: () => setMode('all') }),
    el('button.seg__it', { type: 'button', dataset: { k: 'ok' }, text: t('status.done'), onclick: () => setMode('ok') }),
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
        text: list.length ? t('hist.noneInFilter') : emptyText }));
      return;
    }
    fill(listBox, rows.map(r => {
      const s = settingsOf(r);
      const nl = s && s.loras ? s.loras.filter(x => x.enabled).length : null;
      /* 云端记录别拿步数/CFG 去解释：判据用行上的 backend，
         批量那条写库时 settings_json 里没有 cloud 这一节，早先只看 s.cloud 会漏掉它 */
      const c = settingsOf(r)?.cloud;
      /* 本机那一路要说清用的是谁的图：接管失败退回内置图时，历史记录里看得见的差别
         只有这一句，不然用户会以为改了自己那张图却出了内置的样 */
      const graph = s?.graph_source === 'builtin' ? t('hist.builtinGraph') : '';
      const body = r.backend === 'cloud'
        ? [c?.model, c?.quality].filter(Boolean).join(' · ') || t('hist.cloud')
        : t('hist.steps', { steps: r.steps, cfg: fmtNum(r.cfg, 0.5) }) + (nl != null ? t('hist.lora', { n: nl }) : '') + graph;
      // 画布那一版带了参考图要在条目上看得见：不然"这条和那条差在哪"只能点开对比层猜
      const withRefs = Array.isArray(s?.refs) && s.refs.length ? t('hist.refs', { n: s.refs.length }) : '';
      const meta = `${fmtStamp(r.created_at).slice(5)} · ${body}${withRefs}`;
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
          el('b', { text: `#${r.id} · ${gone ? t('hist.goneFile') : r.status === 'done' ? t('status.done') : r.status === 'error' ? t('status.err') : t('status.run')}` }),
          el('span.pre-it__meta', { text: meta, title: meta }),
          /* 失败原因原来只在右下角 toast 里活 5 秒，回头看这条就什么都没有了 */
          r.status === 'error' && r.error && !gone ? el('span.pre-it__err', { text: String(r.error).slice(0, 160), title: String(r.error || '') }) : null,
          r.rerun_of ? el('span.pre-it__from', { text: t('hist.from', { id: r.rerun_of }) }) : null),
        /* 这一列在 .pre-list（overflow 滚动容器）里，自绘气泡会被裁掉一截：
           实测 174px 宽的提示只有 33px 落在面板内，剩下的被切了。改用原生 title，浏览器画的浮层不受我们布局的裁剪。 */
        el('span.pre-it__acts', {},
          onRestore ? el('button.pre-it__btn', {
            type: 'button', title: s ? t('hist.restoreTitle') : t('hist.noParams'), disabled: !s,
            html: icon('sliders', { cls: 'icon icon--sm' }),
            'aria-label': t('hist.restoreAria'),
            onclick: e => { e.stopPropagation(); onRestore(r); },
          }) : null,
          !gone && r.final_url && onFork ? el('button.pre-it__btn', {
            type: 'button', title: t('hist.forkTitle'),
            html: icon('copy', { cls: 'icon icon--sm' }),
            'aria-label': t('hist.forkAria'),
            onclick: e => { e.stopPropagation(); onFork(r); },
          }) : null,
          onDel ? el('button.pre-it__btn', {
            type: 'button', title: gone ? t('hist.delGone')
              : r.status === 'running' ? t('hist.delRunning') : t('hist.delNormal'),
            disabled: r.status === 'running',
            html: icon('trash', { cls: 'icon icon--sm' }),
            'aria-label': t('hist.delAria'),
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

  const node = el(bare ? 'div' : 'aside', { class: `ed-pre${bare ? ' ed-pre--bare' : ''}` },
    bare ? null : el('div.col-hd', {}, el('h3', { html: icon('layers', { cls: 'icon icon--sm' }) + `<span>${t('hist.title')}</span>` })),
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
