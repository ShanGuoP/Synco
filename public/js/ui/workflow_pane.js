// 工作流参数页：文件导入、节点字段绑定与映射持久化。
'use strict';
import { el, fill } from '../core/dom.js';
import { api } from '../core/api.js';
import { t } from '../core/i18n.js';
import { store } from '../state.js';
import { toastOk, toastErr } from './toast.js';
import { confirm } from './modal.js';
import { createWorkflowCanvas } from './workflow_canvas.js';

const ROLES = ['source_image', 'source_image_mask', 'mask', 'prompt', 'negative', 'seed', 'steps', 'cfg'];
const IMAGE_CLASSES = ['LoadImage', 'LoadImageMask'];
export function createWorkflowPane() {
  let data = null, profile = { bindings: [], output_node: '' }, selected = '', dirty = false, busy = false, closed = false, generation = 0;
  const inp = el('input.input', { type: 'text', spellcheck: 'false', 'aria-label': t('settings.wf.pathTitle') });
  const message = el('p.wf-pane__status', { role: 'status', text: t('wfGraph.loading') });
  const inspector = el('aside.wf-pane__inspector');
  const bindings = el('div.wf-pane__bindings');
  const canvas = createWorkflowCanvas(id => { selected = id; inspect(); });
  const saveBtn = el('button.btn.btn--primary', { type: 'button', text: t('wfGraph.save'), onclick: save });
  const file = el('input', { type: 'file', accept: '.json,application/json', hidden: true, onchange: () => { const picked = file.files[0]; file.value = ''; if (picked) importFile(picked); } });
  const body = el('div.wf-pane__body', {}, canvas.node, inspector);
  const actions = [];
  const button = (key, run) => { const b = el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t(key), onclick: run }); actions.push(b); return b; };
  function status(key, args, error = false) { message.textContent = t(key, args); message.classList.toggle('is-error', error); }
  function lock(value) { busy = value; for (const b of [...actions, saveBtn]) b.disabled = value; inspect(); }
  function summary() {
    saveBtn.disabled = busy || !data;
    canvas.badges(profile);
    fill(bindings, el('strong', { text: t('wfGraph.summary', { count: profile.bindings.length }) }),
      ...profile.bindings.map(b => el('button.wf-pane__binding', { type: 'button', text: t('wfGraph.bindingLabel', { role: t(`wfGraph.roles.${b.role}`), node: b.node_id, field: b.field }), onclick: () => canvas.select(b.node_id, true) })),
      el('span.muted', { text: profile.output_node ? t('wfGraph.outputLabel', { node: profile.output_node }) : t('wfGraph.noOutput') }));
  }
  function change() { dirty = true; summary(); status('wfGraph.unsaved'); }
  function inspect() {
    const n = data?.graph[selected];
    if (!n) { fill(inspector, el('p.muted', { text: t('wfGraph.choose') })); return; }
    const inputs = Object.entries(n.inputs || {}).map(([field, value]) => {
      const edge = data.edges.find(e => e.target === selected && e.field === field);
      const row = el('div.wf-pane__field', {}, el('strong', { text: field }), el('pre.wf-pane__value', { text: JSON.stringify(value, null, 2) }));
      if (edge) {
        row.append(el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('wfGraph.upstream', { node: edge.source, port: edge.port }), onclick: () => canvas.select(edge.source, true) }));
        return row;
      }
      const compatible = ROLES.filter(role => ['seed', 'steps', 'cfg'].includes(role) ? typeof value === 'number' :
        ['source_image', 'source_image_mask', 'mask'].includes(role) ? typeof value === 'string' && field === 'image' && IMAGE_CLASSES.includes(n.class_type) && (role !== 'source_image_mask' || n.class_type === 'LoadImage') : typeof value === 'string');
      if (!compatible.length) return row;
      const nodeId = selected;
      const select = el('select.select', { 'aria-label': t('wfGraph.bindField', { field }), onchange: () => {
        profile.bindings = profile.bindings.filter(b => !(b.node_id === nodeId && b.field === field));
        if (select.value) profile.bindings.push({ node_id: nodeId, field, role: select.value }); change();
      } }, el('option', { value: '', text: t('wfGraph.keep') }), ...compatible.map(role => el('option', { value: role, text: t(`wfGraph.roles.${role}`) })));
      select.value = profile.bindings.find(b => b.node_id === nodeId && b.field === field)?.role || ''; select.disabled = busy;
      row.append(select); return row;
    });
    const output = ['SaveImage', 'PreviewImage'].includes(n.class_type) ? el('button.btn.btn--ghost', {
      type: 'button', disabled: busy, text: t(profile.output_node === selected ? 'wfGraph.selectedOutput' : 'wfGraph.setOutput'),
      onclick: () => { profile.output_node = selected; change(); inspect(); },
    }) : null;
    fill(inspector, el('span.muted', { text: `#${selected} · ${n.class_type}` }), el('h3', { text: n._meta?.title || n.class_type }), output,
      el('p.muted', { text: t('wfGraph.fieldNote') }), ...inputs);
  }
  function mount(result) {
    data = result; inp.value = result.path; dirty = false; selected = '';
    profile = result.profile || { bindings: [], output_node: '' };
    canvas.render(result.graph, result.edges); summary(); inspect(); status('wfGraph.offline');
  }
  async function discard() { return !dirty || await confirm({ title: t('wfGraph.discard'), text: t('wfGraph.discardNote') }); }
  async function load(initial = false) {
    if (busy || closed) return;
    lock(true);
    if (!initial && !await discard()) { if (!closed) { lock(false); summary(); } return; }
    if (closed) return;
    const rev = ++generation;
    try {
      const cfg = await api.cfg(); if (closed || rev !== generation) return;
      store.set({ cfg });
      inp.value = cfg.workflow_path || '';
      const result = await api.workflowGraph(); if (closed || rev !== generation) return; mount(result);
    } catch (e) { if (!closed && rev === generation) { data = null; dirty = false; profile = { bindings: [], output_node: '' }; canvas.render({}, []); summary(); inspect(); status('wfGraph.noGraph', { reason: e.message }, true); } }
    finally { if (!closed && rev === generation) { lock(false); summary(); } }
  }
  async function importFile(picked) {
    if (busy || closed) return;
    lock(true);
    if (!await discard()) { if (!closed) { lock(false); summary(); } return; }
    if (closed) return;
    const rev = ++generation;
    try {
      if (picked.size > 8 * 1024 * 1024) throw new Error(t('wfGraph.tooLarge'));
      const text = await picked.text(); if (closed || rev !== generation) return;
      let graph; try { graph = JSON.parse(text); } catch { throw new Error(t('wfGraph.invalidJson')); }
      const result = await api.importWorkflow(graph); if (closed || rev !== generation) return;
      mount(result); const cfg = await api.cfg(); if (closed || rev !== generation) return; store.set({ cfg }); toastOk(t('wfGraph.imported'));
    } catch (e) { if (!closed && rev === generation) toastErr(t('wfGraph.importFail'), e.message); }
    finally { if (!closed && rev === generation) { lock(false); summary(); inspect(); } }
  }
  async function savePath() {
    if (busy || closed) return;
    lock(true);
    if (!await discard()) { if (!closed) { lock(false); summary(); } return; }
    if (closed) return;
    try { await api.setWorkflow(inp.value.trim()); if (closed) return; dirty = false; }
    catch (e) { if (!closed) toastErr(t('settings.wf.saveFail'), e.message); }
    finally { lock(false); }
    if (!closed) await load(true);
  }
  async function save() {
    if (busy || !data || closed) return;
    lock(true); inspect();
    const snapshot = JSON.parse(JSON.stringify(profile));
    try {
      // 绑定始终带原工作流路径；旧页面不得把节点号写到刚切换的新图。
      const result = await api.saveWorkflowProfile({ ...snapshot, workflow_path: data.path }); if (closed) return;
      profile = result.profile; dirty = false; summary(); inspect(); status('wfGraph.saved'); toastOk(t('wfGraph.saved'));
    } catch (e) { if (!closed) toastErr(t('settings.roles.saveFail'), e.message); }
    finally { if (!closed) { lock(false); summary(); inspect(); } }
  }
  const node = el('div.dlg-flow.wf-pane', {},
    el('div.wf-pane__import', {}, el('h4.dlg-h4', { text: t('wfGraph.title') }), el('p.muted', { text: t('wfGraph.intro') }),
      el('div.wf-graph__toolbar', {}, button('wfGraph.import', () => file.click()), button('settings.wf.reload', () => load()), file),
      el('details', {}, el('summary', { text: t('settings.wf.pathTitle') }), inp, button('settings.wf.saveVerify', savePath))),
    message, body, el('div.wf-pane__footer', {}, bindings, saveBtn));
  node.addEventListener('dragover', e => { if (e.dataTransfer?.types.includes('Files')) { e.preventDefault(); node.classList.add('is-drop'); } });
  node.addEventListener('dragleave', e => { if (!node.contains(e.relatedTarget)) node.classList.remove('is-drop'); });
  node.addEventListener('drop', e => { e.preventDefault(); node.classList.remove('is-drop'); const picked = e.dataTransfer?.files[0]; if (picked) importFile(picked); });
  load(true);
  return { node, fit: canvas.fit, close: () => { closed = true; generation++; } };
}
