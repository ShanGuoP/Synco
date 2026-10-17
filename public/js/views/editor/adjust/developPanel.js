// Accordion assembly and development controls; the parent owns the edit session.
import { el, fill } from '../../../core/dom.js';
import { t } from '../../../core/i18n.js';
import { makeProw } from '../../../ui/controls.js';
import { grp, switchRow } from './controls.js';
import { blankDevelop, blankLook, blankLocal } from './developModel.js';
import { makeCurveEditor } from './curveEditor.js';

export function makeDevelopPanel({ getOps, colorControls, geometry, beauty, presets, lut, onChange, onAuto, onWhiteBalance, onEditMask, onConfirmMask, maskEditing, onMaskSelection }) {
  const bindings = [], updates = [];
  let localIndex = 0;
  const dev = () => getOps().develop;
  const local = () => dev().locals[localIndex];
  const dflt = blankLocal();
  const row = (label, get, set, min = -100, max = 100, track = '', step = 1) => {
    const ctl = makeProw({ label: t(`dev.${label}`), min, max, step, formatInput: step < 1 ? v => String(Number(v.toFixed(3))) : undefined, value: get(), onChange: v => { set(v); onChange(); } });
    if (track) ctl.node.classList.add(`develop-track--${track}`);
    bindings.push(() => ctl.set(get(), true)); return ctl.node;
  };
  const section = (key, collapsed, body, reset, actions = []) => {
    const node = grp(t(`dev.${key}`), collapsed, body);
    node.dataset.section = key;
    const head = node.querySelector('.grp__hd');
    const resetBtn = el('button.develop-reset', { type: 'button', text: '↻', title: t('dev.resetSection'), 'aria-label': `${t('dev.resetSection')} · ${t(`dev.${key}`)}`, onclick: () => { reset(); sync(); onChange(); } });
    const heading = el('div.develop-heading'); head.before(heading); heading.append(head, ...actions, resetBtn); return node;
  };
  const stack = (...nodes) => el('div.develop-stack', {}, ...nodes);
  const basic = section('basic', false, stack(
    colorControls.exposure.node, colorControls.contrast.node, colorControls.highlights.node, colorControls.shadows.node,
    row('whites', () => dev().look.basic.whites, v => dev().look.basic.whites = v),
    row('blacks', () => dev().look.basic.blacks, v => dev().look.basic.blacks = v),
    row('brightness', () => dev().look.basic.brightness, v => dev().look.basic.brightness = v)),
    () => { for (const k of ['exposure', 'contrast', 'highlights', 'shadows']) getOps().color[k] = 0; getOps().color.preset = null; dev().look.basic = blankLook().basic; },
    [el('button.btn.btn--ghost.btn--sm.develop-auto', { type: 'button', text: t('dev.auto'), onclick: onAuto })]);
  const picker = el('button.develop-reset', { type: 'button', text: '⌾', title: t('dev.whitePicker'), 'aria-label': t('dev.whitePicker'), onclick: onWhiteBalance });
  const color = section('color', false, stack(...['temp', 'tint', 'vibrance', 'saturation'].map(k => { const node = colorControls[k].node; node.classList.add(`develop-track--${k}`); return node; })), () => { for (const k of ['temp', 'tint', 'vibrance', 'saturation']) getOps().color[k] = 0; }, [picker]);
  const curves = makeCurveEditor({ getCurves: () => dev().look.curves, onChange }); updates.push(curves.sync);
  function hslControls(getLook) {
    let band = 0;
    const colors = ['#d66565', '#d8a270', '#d4bf67', '#7da984', '#6fafb9', '#7499ca', '#9884bd', '#bc81aa'];
    const tabs = el('div.develop-swatches', {}, ...colors.map((c, i) => el('button.develop-swatch', { type: 'button', style: { '--swatch': c }, 'aria-label': t(`dev.band${i}`), title: t(`dev.band${i}`), onclick: () => { band = i; sync(); } })));
    updates.push(() => { for (const [i, n] of [...tabs.children].entries()) { n.classList.toggle('is-on', band === i); n.setAttribute('aria-pressed', String(band === i)); } });
    return stack(tabs, ...['hue', 'saturation', 'luminance'].map(k => row(k, () => getLook().hsl[band][k], v => getLook().hsl[band][k] = v, -100, 100, k)));
  }
  const hsl = section('hsl', true, hslControls(() => dev().look), () => dev().look.hsl = blankLook().hsl);
  let zone = 'shadows';
  const zoneTabs = el('div.develop-tabs', {}, ...['shadows', 'midtones', 'highlights'].map(k => el('button.chip-s', { type: 'button', text: t(`dev.${k}`), dataset: { zone: k }, onclick: () => { zone = k; sync(); } })));
  const wheel = el('input.develop-color-wheel', { type: 'color', value: '#ff0000', 'aria-label': t('dev.gradingColor'), oninput: e => {
    const hex = e.target.value, rgb = [1, 3, 5].map(i => parseInt(hex.slice(i, i + 2), 16) / 255), hi = Math.max(...rgb), lo = Math.min(...rgb), d = hi - lo;
    if (d > 0) { let h = hi === rgb[0] ? ((rgb[1] - rgb[2]) / d + 6) % 6 : hi === rgb[1] ? (rgb[2] - rgb[0]) / d + 2 : (rgb[0] - rgb[1]) / d + 4; dev().look.grading[zone].hue = Math.round(h * 60) % 360; }
    dev().look.grading[zone].saturation = Math.round(d * 100); sync(); onChange();
  } });
  updates.push(() => {
    for (const b of zoneTabs.children) b.classList.toggle('is-on', b.dataset.zone === zone);
    const h = dev().look.grading[zone].hue / 60, s = dev().look.grading[zone].saturation / 100;
    const c = s, x = c * (1 - Math.abs(h % 2 - 1)), m = (1 - c) / 2;
    const rgb = h < 1 ? [c,x,0] : h < 2 ? [x,c,0] : h < 3 ? [0,c,x] : h < 4 ? [0,x,c] : h < 5 ? [x,0,c] : [c,0,x];
    wheel.value = '#' + rgb.map(v => Math.round((v + m) * 255).toString(16).padStart(2, '0')).join('');
  });
  const grading = section('grading', true, stack(zoneTabs, wheel,
    row('hue', () => dev().look.grading[zone].hue, v => dev().look.grading[zone].hue = v, 0, 359, 'hue'),
    row('saturation', () => dev().look.grading[zone].saturation, v => dev().look.grading[zone].saturation = v, 0, 100),
    row('luminance', () => dev().look.grading[zone].luminance, v => dev().look.grading[zone].luminance = v),
    row('balance', () => dev().look.grading.balance, v => dev().look.grading.balance = v),
    row('blending', () => dev().look.grading.blending, v => dev().look.grading.blending = v, 0, 100)), () => dev().look.grading = blankLook().grading);
  const detail = section('detail', true, stack(colorControls.clarity.node, colorControls.sharpen.node,
    row('lumaNoise', () => dev().detail.luma, v => dev().detail.luma = v, 0, 100),
    row('chromaNoise', () => dev().detail.chroma, v => dev().detail.chroma = v, 0, 100)), () => { getOps().color.clarity = 0; getOps().color.sharpen = 0; dev().detail = blankDevelop().detail; });
  const lens = section('lens', true, stack(
    el('p.muted.develop-hint', { text: t('dev.manualLens') }),
    row('distortion', () => dev().lens.distortion, v => dev().lens.distortion = v),
    row('chromatic', () => dev().lens.chromatic, v => dev().lens.chromatic = v),
    geometry), () => { dev().lens = blankDevelop().lens; getOps().geometry = { crop: null, rotate_deg: 0, flip_h: false, flip_v: false, fill: 'edge' }; });
  const tetra = switchRow(t('dev.tetra'), '', v => { dev().lut_tetra = v; onChange(); }); updates.push(() => tetra.set(dev().lut_tetra));
  const effects = section('effects', true, stack(presets, lut, tetra.node,
    row('film', () => dev().effects.film, v => dev().effects.film = v, 0, 100),
    row('vignette', () => dev().effects.vignette, v => dev().effects.vignette = v),
    row('midpoint', () => dev().effects.midpoint, v => dev().effects.midpoint = v, 0, 100),
    row('feather', () => dev().effects.feather, v => dev().effects.feather = v, 0, 100),
    row('grain', () => dev().effects.grain, v => dev().effects.grain = v, 0, 100),
    row('grainSize', () => dev().effects.size, v => dev().effects.size = v, 0, 100),
    row('roughness', () => dev().effects.roughness, v => dev().effects.roughness = v, 0, 100),
    el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('dev.newSeed'), onclick: () => { dev().effects.seed = (dev().effects.seed + 1) >>> 0; onChange(); } })), () => { dev().effects = blankDevelop().effects; dev().lut_tetra = false; getOps().lut = null; getOps().color.preset = null; });

  const localSelect = el('select.input', { 'aria-label': t('dev.localLayer'), onchange: () => { localIndex = Number(localSelect.value); sync(); onMaskSelection(); } });
  const localEnabled = switchRow(t('dev.enabled'), '', v => { if (local()) { local().enabled = v; onChange(); } });
  const invert = switchRow(t('dev.invert'), '', v => { if (local()) { local().mask.invert = v; onChange(); onMaskSelection(); } });
  const kind = el('select.input', { 'aria-label': t('dev.maskKind'), onchange: () => { if (local()) { local().mask.kind = kind.value; onChange(); onMaskSelection(); sync(); } } },
    ...['radial', 'linear', 'brush'].map(k => el('option', { value: k, text: t(`dev.${k}`) })));
  const maskRows = ['centerX', 'centerY', 'radiusX', 'radiusY', 'endX', 'endY'].map((k, i) => {
    const field = i < 2 ? 'center' : i < 4 ? 'radius' : 'end', axis = i % 2;
    return row(k, () => Math.round((local() || dflt).mask[field][axis] * 100), v => { if (local()) { local().mask[field][axis] = v / 100; onMaskSelection(); } }, field === 'radius' ? 1 : 0, 100);
  });
  const localLook = () => local()?.look || dflt.look;
  const localCurves = makeCurveEditor({ getCurves: () => localLook().curves, onChange }); updates.push(localCurves.sync);
  const editMask = el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('dev.editMask'), onclick: onEditMask });
  const confirmMask = el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('dev.confirmMask'), hidden: true, onclick: onConfirmMask });
  const syncMaskMode = () => { editMask.hidden = maskEditing(); confirmMask.hidden = !maskEditing(); };
  const localBody = stack(localEnabled.node, kind, editMask, confirmMask,
    el('p.muted.develop-hint', { text: t('dev.maskHint') }), ...maskRows,
    row('feather', () => (local() || dflt).mask.feather, v => { if (local()) { local().mask.feather = v; onMaskSelection(); } }, 0, 100), invert.node,
    el('button.btn.btn--ghost.btn--sm', { type: 'button', text: t('dev.undoStroke'), onclick: () => { local()?.mask.strokes.pop(); onChange(); onMaskSelection(); } }),
    ...['exposure', 'contrast', 'highlights', 'shadows', 'temp', 'tint', 'saturation'].map(k => row(k, () => (local() || dflt).color[k] * (k === 'exposure' ? 0.015 : 1), v => { if (local()) local().color[k] = k === 'exposure' ? Math.round(v / 0.015) : v; }, k === 'exposure' ? -1.5 : -100, k === 'exposure' ? 1.5 : 100, '', k === 'exposure' ? 0.015 : 1)),
    grp(t('dev.hsl'), true, hslControls(localLook)), grp(t('dev.curves'), true, localCurves.node));
  const localSection = section('local', true, stack(
    el('div.adj-row.develop-local-tools', {}, localSelect,
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '+', title: t('dev.addLayer'), 'aria-label': t('dev.addLayer'), onclick: () => { if (dev().locals.length >= 16) return; dev().locals.push(blankLocal()); localIndex = dev().locals.length - 1; sync(); onChange(); onMaskSelection(); } }),
      el('button.btn.btn--ghost.btn--sm', { type: 'button', text: '−', title: t('dev.deleteLayer'), 'aria-label': t('dev.deleteLayer'), onclick: () => { if (!local()) return; dev().locals.splice(localIndex, 1); localIndex = Math.max(0, localIndex - 1); sync(); onChange(); onMaskSelection(); } })), localBody),
    () => { dev().locals = []; localIndex = 0; onMaskSelection(); });
  function sync() {
    syncMaskMode();
    localIndex = Math.min(localIndex, Math.max(0, dev().locals.length - 1));
    for (const fn of bindings) fn(); for (const fn of updates) fn();
    fill(localSelect, ...dev().locals.map((l, i) => el('option', { value: i, text: t('dev.layerName', { n: i + 1 }) })));
    localSelect.value = String(localIndex); localSelect.disabled = !dev().locals.length;
    localBody.hidden = !local(); kind.value = (local() || dflt).mask.kind;
    localEnabled.set(local()?.enabled); invert.set(local()?.mask.invert);
    // Existing color controls are synchronized by the session after group resets.
  }
  const groups = [basic, color, section('curves', true, curves.node, () => dev().look.curves = blankLook().curves), hsl, grading, detail, lens, effects, localSection,
    section('beauty', true, beauty, () => { getOps().beauty = { ...getOps().beauty, smooth: 0, texture: 0, blemish: 0, even_tone: 0, brighten: 0, de_shine: 0, sharpen: 0 }; getOps().portrait.faces = []; for (const k of Object.keys(getOps().portrait.body.strength)) getOps().portrait.body.strength[k] = 0; getOps().warp.strokes = []; for (const k of Object.keys(getOps().warp.auto)) getOps().warp.auto[k] = 0; })];
  return { groups, sync, syncMaskMode, local, moveActive: () => !!local() && !localSection.classList.contains('is-collapsed'), resetSelection() { localIndex = 0; } };
}
