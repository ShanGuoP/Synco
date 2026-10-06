// 说明类弹窗：修图流程 / 快捷键 / 工作流默认参数，多处复用
'use strict';
import { el } from '../core/dom.js';
import { modal } from './modal.js';
import { fmtNum, baseName } from '../core/format.js';

const FLOW = [
  ['导入', '首页拖入一批照片建项目，或在项目页「导入更多」追加'],
  ['涂抹', '打开图片用画笔涂出要修的区域；白色=要重绘，橡皮可擦回'],
  ['提交', '右面板写好指令与参数后提交，可勾选多张批量跑'],
  ['对比', '完成后拖动中缝对比原图与成图，满意再下载'],
];

const KEYS = [
  ['B', '画笔'], ['E', '橡皮'], ['H', '抓手平移'],
  ['空格（按住）', '临时平移'], ['Ctrl + Z', '撤销一笔'],
  ['滚轮', '以光标为锚点缩放'], ['0', '适应窗口'], ['1', '实际像素 1:1'],
  ['Ctrl + Enter', '提交生成'], ['Esc', '退出对比 / 关闭弹窗'],
];

const keyList = () => el('dl', { class: 'kv' },
  ...KEYS.flatMap(([k, d]) => [el('dt', {}, el('span.kbd', { text: k })), el('dd', { text: d })]));

export function flowModal() {
  modal({
    title: '修图流程',
    wide: true,
    body: el('div.dlg-flow', {},
      ...FLOW.map(([t, d], i) => el('div.dlg-step', {},
        el('span.dlg-step__k', { text: String(i + 1) }),
        el('b.dlg-step__t', { text: t }),
        el('span.dlg-step__d', { text: d }))),
      el('div.dlg-note', { text: '遮罩以 PNG 存在项目目录，随时回来接着改；生成结果会进图片的历史列表，可反复对比。' }),
    ),
    actions: [{ label: '知道了', kind: 'primary' }],
  });
}

export function shortcutsModal() {
  modal({
    title: '快捷键',
    body: keyList(),
    actions: [{ label: '知道了', kind: 'primary' }],
  });
}

export function helpModal() {
  const grid = el('div.dlg-grid', {},
    ...FLOW.map(([t, d], i) => el('div.dlg-card', {},
      el('b', { text: `${i + 1} · ${t}` }),
      el('span', { text: d }))));

  modal({
    title: '帮助',
    wide: true,
    body: el('div.dlg-flow', {},
      el('div', {}, el('h4.dlg-h4', { text: '四步走' }), grid),
      el('div', {}, el('h4.dlg-h4', { text: '快捷键' }), keyList()),
    ),
    actions: [{ label: '关闭', kind: 'primary' }],
  });
}

export function workflowModal(cfg) {
  const loras = cfg.loras || [];
  modal({
    title: '工作流默认参数',
    wide: true,
    body: el('div.dlg-flow', {},
      el('dl', { class: 'kv' },
        el('dt', { text: '参数来源' }), el('dd', { text: cfg.cfg_source === 'workflow' ? `本机工作流 · ${cfg.workflow_path || ''}` : `内置默认 · ${cfg.workflow_error || '未关联工作流'}` }),
        el('dt', { text: '默认步数' }), el('dd', { text: fmtNum(cfg.steps, 1) }),
        el('dt', { text: '默认 CFG' }), el('dd', { text: fmtNum(cfg.cfg, 0.5) }),
        el('dt', { text: '负面提示词' }), el('dd', { text: cfg.negative || '（空）' }),
        el('dt', { text: 'LoRA 链' }), el('dd', {},
          loras.length
            ? el('div.dlg-list', {},
              ...loras.map(l => el('div.dlg-row', {},
                el('span.dot', { class: `dot ${l.enabled === false ? '' : 'dot--done'}` }),
                el('span', { text: baseName(l.name) }),
                el('span.muted', { text: `×${fmtNum(l.strength, 0.05)}` }))))
            : el('span', { text: '（无）' }))),
      el('div', {},
        el('h4.dlg-h4', { text: '默认正向指令' }),
        el('div.dlg-prompt', { text: cfg.prompt_default || '（工作流里没有预置指令）' })),
      el('div.dlg-note', { text: '这些值直接读自本机 ComfyUI 工作流文件，改工作流后重启服务即生效。' }),
    ),
    actions: [{ label: '关闭', kind: 'primary' }],
  });
}
