// 说明类弹窗：修图流程 / 快捷键 / 工作流默认参数，多处复用
'use strict';
import { el } from '../core/dom.js';
import { modal } from './modal.js';
import { fmtNum, baseName } from '../core/format.js';
import { t as tr } from '../core/i18n.js';

/* 键进字典取成对文案：dlg.<k> 是步骤名，dlg.<k>D 是那句解释 */
const FLOW = ['imp', 'mask', 'submit', 'compare'].map(k => [k, `dlg.${k}`, `dlg.${k}D`]);

const KEYS = [
  ['B', 'dlg.dBrush'], ['E', 'dlg.dEraser'], ['H', 'dlg.dPan'],
  ['dlg.dSpace', 'dlg.dPanTemp'], ['Ctrl + Z', 'dlg.dUndo'],
  ['dlg.dWheel', 'dlg.dZoomAnchor'], ['0', 'dlg.dFit'], ['1', 'dlg.dOne2One'],
  ['Ctrl + Enter', 'dlg.dSubmitGen'], ['dlg.dEsc', 'dlg.dEscWhat'],
];

/* 左列混着两种东西：`B` / `Ctrl + Z` 是键帽上印的字（哪国语言都一样），
   「空格（按住）」「滚轮」是要翻译的说法。带 dlg. 前缀的才查字典，别的一律原样上键帽 */
const cap = s => (s.startsWith('dlg.') ? tr(s.slice(4)) : s);

const keyList = () => el('dl', { class: 'kv' },
  ...KEYS.flatMap(([k, d]) => [el('dt', {}, el('span.kbd', { text: cap(k) })), el('dd', { text: tr(d) })]));

export function flowModal() {
  modal({
    title: tr('dlg.flow'),
    wide: true,
    body: el('div.dlg-flow', {},
      ...FLOW.map(([k, tk, dk], i) => el('div.dlg-step', {},
        el('span.dlg-step__k', { text: String(i + 1) }),
        el('b.dlg-step__t', { text: tr(tk) }),
        el('span.dlg-step__d', { text: tr(dk) }))),
      el('div.dlg-note', { text: tr('dlg.flowNote') }),
    ),
    actions: [{ label: tr('common.gotIt'), kind: 'primary' }],
  });
}

export function shortcutsModal() {
  modal({
    title: tr('dlg.shortcuts'),
    body: keyList(),
    actions: [{ label: tr('common.gotIt'), kind: 'primary' }],
  });
}

export function helpModal() {
  const grid = el('div.dlg-grid', {},
    ...FLOW.map(([k, tk, dk], i) => el('div.dlg-card', {},
      el('b', { text: `${i + 1} · ${tr(tk)}` }),
      el('span', { text: tr(dk) }))));

  modal({
    title: tr('dlg.help'),
    wide: true,
    body: el('div.dlg-flow', {},
      el('div', {}, el('h4.dlg-h4', { text: tr('dlg.steps') }), grid),
      el('div', {}, el('h4.dlg-h4', { text: tr('dlg.shortcuts') }), keyList()),
    ),
    actions: [{ label: tr('common.close'), kind: 'primary' }],
  });
}

export function workflowModal(cfg) {
  const loras = cfg.loras || [];
  modal({
    title: tr('dlg.wfDefaults'),
    wide: true,
    body: el('div.dlg-flow', {},
      el('dl', { class: 'kv' },
        el('dt', { text: tr('dlg.src') }), el('dd', { text: cfg.cfg_source === 'workflow' ? tr('dlg.srcWf', { path: cfg.workflow_path || '' }) : tr('dlg.srcBuiltin', { err: cfg.workflow_error || tr('dlg.srcNoWf') }) }),
        el('dt', { text: tr('dlg.defSteps') }), el('dd', { text: fmtNum(cfg.steps, 1) }),
        el('dt', { text: tr('dlg.defCfg') }), el('dd', { text: fmtNum(cfg.cfg, 0.5) }),
        el('dt', { text: tr('dlg.defNeg') }), el('dd', { text: cfg.negative || tr('dlg.empty') }),
        el('dt', { text: tr('dlg.defLora') }), el('dd', {},
          loras.length
            ? el('div.dlg-list', {},
              ...loras.map(l => el('div.dlg-row', {},
                el('span.dot', { class: `dot ${l.enabled === false ? '' : 'dot--done'}` }),
                el('span', { text: baseName(l.name) }),
                el('span.muted', { text: `×${fmtNum(l.strength, 0.05)}` }))))
            : el('span', { text: tr('dlg.none') }))),
      el('div', {},
        el('h4.dlg-h4', { text: tr('dlg.defPrompt') }),
        el('div.dlg-prompt', { text: cfg.prompt_default || tr('dlg.noPreset') })),
      el('div.dlg-note', { text: tr('dlg.wfNote') }),
    ),
    actions: [{ label: tr('common.close'), kind: 'primary' }],
  });
}
